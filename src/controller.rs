//! Controller layer.
//!
//! Glue between the Model and the View. Lives behind a `dyn Controller`
//! trait that the Slint callbacks call into. Owns the 5s background
//! poller and a tokio task that mirrors Model state into the ViewSink.
//! All setter calls on the View go through the sink, which the SlintAppView
//! drains on the UI thread via a timer tick.

use crate::model::{AppModel, StateSnapshot, Status};
use crate::view::{Controller, ViewSink, ViewState};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(5);

pub struct AppController {
    model: AppModel,
    sink: ViewSink,
    /// Borrowed-once view-state handle for synchronous reads (e.g. for
    /// `ollama_host` when launching). Cloning a `Box<dyn ViewState>` is
    /// cheap and `Send + Sync` so this is movable into a tokio task.
    view_state: Box<dyn ViewState>,
    /// Weak self handle for the spawned tasks to call back.
    self_weak: std::sync::OnceLock<WeakSelf>,
}

/// Type-aliased weak self-reference used by the spawned tasks.
type WeakSelf = std::sync::Weak<dyn Controller>;

impl AppController {
    pub fn new(model: AppModel, sink: ViewSink, view_state: Box<dyn ViewState>) -> Arc<Self> {
        Arc::new(Self {
            model,
            sink,
            view_state,
            self_weak: std::sync::OnceLock::new(),
        })
    }

    /// Register the weak self-reference. Call once after `new()`.
    pub fn install_weak(self: &Arc<Self>) {
        let _ = self.self_weak.set(Arc::downgrade(&(self.clone() as Arc<dyn Controller>)));
    }

    /// Spawn the poller and the model -> view mirror task.
    pub fn start(self: &Arc<Self>, rt: &tokio::runtime::Handle) {
        // initial pull
        let me = Arc::downgrade(self);
        let m = self.model.clone();
        rt.spawn(async move {
            m.refresh().await;
            drop(me);
        });

        // 5s poller
        let m = self.model.clone();
        rt.spawn(async move {
            loop {
                tokio::time::sleep(POLL_INTERVAL).await;
                m.refresh().await;
            }
        });

        // mirror Model -> ViewSink
        let sink = self.sink.clone();
        let mut rx = self.model.subscribe();
        let initial = rx.borrow().clone();
        sink.apply_snapshot(initial);
        rt.spawn(async move {
            while rx.changed().await.is_ok() {
                let snap: StateSnapshot = rx.borrow().clone();
                sink.apply_snapshot(snap);
            }
        });
    }
}

impl Controller for AppController {
    fn on_launch(&self, agent_idx: i32, model: String) {
        let Some(agent) = self.model.agent_by_index(agent_idx) else {
            self.model.set_status(Status {
                message: "✗ Invalid agent".into(),
                kind: 2,
            });
            return;
        };
        let host = self.view_state.ollama_host();
        self.model.record_launch(agent.name.clone(), model.clone());
        let m = self.model.clone();
        let sink = self.sink.clone();
        tokio::spawn(async move {
            let res = m.launch(agent.clone(), model.clone(), Some(host)).await;
            let (msg, kind) = match res {
                Ok(()) => (format!("✓ {} launched · {}", agent.display, model), 1),
                Err(e) => (format!("✗ {e}"), 2),
            };
            m.set_status(Status { message: msg, kind });
            sink.set_status(
                m.snapshot().status.message.clone(),
                m.snapshot().status.kind,
            );
        });
    }

    fn on_restore(&self, agent_idx: i32) {
        let Some(agent) = self.model.agent_by_index(agent_idx) else {
            self.model.set_status(Status {
                message: "✗ Invalid agent".into(),
                kind: 2,
            });
            return;
        };
        let token = agent.name.clone();
        let display = agent.display.clone();
        let m = self.model.clone();
        let sink = self.sink.clone();
        tokio::spawn(async move {
            let res = m.restore(token.clone()).await;
            let (msg, kind) = match res {
                Ok(()) => (format!("✓ {display} restored to its original profile"), 1),
                Err(e) => (format!("✗ {e}"), 2),
            };
            m.set_status(Status { message: msg, kind });
            sink.set_status(
                m.snapshot().status.message.clone(),
                m.snapshot().status.kind,
            );
        });
    }

    fn on_refresh(&self) {
        let m = self.model.clone();
        tokio::spawn(async move {
            m.refresh().await;
        });
    }

    fn on_test_connection(&self, url: String) {
        let m = self.model.clone();
        tokio::spawn(async move {
            m.test_connection(url).await;
        });
    }

    fn on_dismiss_status(&self) {
        self.model.dismiss_status();
    }
    fn on_toggle_settings(&self) {
        self.model.toggle_settings();
    }
    fn on_close_settings(&self) {
        self.model.set_settings_open(false);
    }
    fn on_selection_changed(&self, agent: Option<String>, model: Option<String>) {
        self.model.record_selection(agent, model);
    }
    fn on_ollama_host_edited(&self, url: String) {
        self.model.set_ollama_host(url);
    }
}



#[cfg(test)]
mod tests {
    //! Unit tests for the Controller. The View is replaced with a
    //! `FakeSink` (collects `ViewCommand`s) and a `FakeViewState`
    //! (returns canned ollama_host / selection). The Model uses the
    //! same `FakeRepository` as the model tests.

    use super::*;
    use anyhow::Result;
    use crate::config::Prefs;
    use crate::model::{AppModel, Status};
    use crate::ollama::Agent;
    use crate::repository::{Repository, TestResult, WorldSnapshot};
    use crate::view::{ViewCommand, ViewState};
    use crate::test_util::HomeGuard;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct FakeInner {
        world: Option<Result<WorldSnapshot, String>>,
        test: Option<Result<TestResult, String>>,
        launches: Vec<(String, String, Option<String>)>,
        restores: Vec<String>,
    }
    struct FakeRepository(Arc<Mutex<FakeInner>>);

    fn agent(name: &str, display: &str, is_gui: bool) -> Agent {
        Agent { name: name.to_string(), display: display.to_string(), is_gui }
    }
    fn world(agents: Vec<Agent>) -> WorldSnapshot {
        let running = vec![false; agents.len()];
        let installed = vec![true; agents.len()];
        WorldSnapshot {
            agents,
            running,
            installed,
            cloud_models: vec!["gpt-oss:120b-cloud".into()],
        }
    }

    #[async_trait::async_trait]
    impl Repository for FakeRepository {
        async fn list_agents(&self) -> Result<Vec<Agent>> {
            self.0.lock().unwrap().world.as_ref().unwrap().as_ref().map(|w| w.agents.clone()).map_err(|e| anyhow::anyhow!("{e}"))
        }
        async fn list_cloud_models(&self) -> Result<Vec<crate::ollama::Model>> {
            self.0.lock().unwrap().world.as_ref().unwrap().as_ref()
                .map(|w| w.cloud_models.iter().map(|n| crate::ollama::Model { name: n.clone() }).collect())
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        async fn list_local_models(&self, _url: &str) -> Result<Vec<crate::ollama::Model>> {
            let g = self.0.lock().unwrap();
            match g.test.as_ref() {
                Some(Ok(t)) => Ok(t.local_models.iter().map(|n| crate::ollama::Model { name: n.clone() }).collect()),
                _ => Ok(Vec::new()),
            }
        }
        async fn test_connection(&self, _url: &str) -> Result<String> {
            self.0.lock().unwrap().test.as_ref().unwrap().as_ref().map(|t| t.info.clone()).map_err(|e| anyhow::anyhow!("{e}"))
        }
        fn running_states(&self, agents: &[Agent]) -> Vec<bool> {
            let _ = agents;
            self.0.lock().unwrap().world.as_ref().unwrap().as_ref().unwrap().running.clone()
        }
        fn installed_states(&self, agents: &[Agent]) -> Vec<bool> {
            let _ = agents;
            self.0.lock().unwrap().world.as_ref().unwrap().as_ref().unwrap().installed.clone()
        }
        fn restore_available(&self, _a: &str) -> bool { false }
        async fn restore_agent(&self, token: &str) -> Result<()> {
            self.0.lock().unwrap().restores.push(token.to_string());
            Ok(())
        }
        async fn launch_agent(&self, agent: &Agent, model: &str, host: Option<&str>) -> Result<()> {
            self.0.lock().unwrap().launches.push((agent.name.clone(), model.to_string(), host.map(String::from)));
            Ok(())
        }
    }

    /// Collect every `ViewCommand` the controller emits.
    #[derive(Clone, Default)]
    struct FakeSink {
        cmds: Arc<Mutex<Vec<ViewCommand>>>,
    }
    impl FakeSink {
        fn snapshot(&self) -> Vec<ViewCommand> {
            self.cmds.lock().unwrap().clone()
        }
        fn clear(&self) {
            self.cmds.lock().unwrap().clear();
        }
    }

    /// A `ViewSink`-like object: in the real View, `ViewSink` is a
    /// `mpsc::UnboundedSender<ViewCommand>`. For tests we build one
    /// pointing at a channel whose receiver we read from via `drain`.
    struct ChannelSink {
        tx: tokio::sync::mpsc::UnboundedSender<ViewCommand>,
        rx: Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<ViewCommand>>>,
    }
    impl ChannelSink {
        fn new() -> Self {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            Self { tx, rx: Arc::new(Mutex::new(rx)) }
        }
        fn sink(&self) -> ViewSink {
            // Build a ViewSink by emitting a command through `tx` and
            // and re-routing through the same channel. Since ViewSink
            // owns its own tx, we can't easily alias. Use the public
            // apply_snapshot/set_status path.
            //
            // We expose a tiny test-only helper: drain_all reads every
            // command currently buffered and returns them in order.
            let _ = self;
            // Hack: we cannot construct a ViewSink from outside the view
            // module. Use the FakeSink + drain pattern instead.
            unimplemented!()
        }
    }

    // In lieu of plumbing a custom sink, we use a simpler approach:
    // construct the real `ViewSink` by building a temporary SlintAppView
    // ... but that requires a UI thread.
    //
    // Pragmatic alternative: test the Controller via a method that
    // bypasses the sink — by calling the model's intents directly. The
    // sink's job is just to forward; the model's intent methods are the
    // real surface. So these tests focus on:
    //   * on_launch calls the repo and persists prefs
    //   * on_restore calls the repo
    //   * on_refresh / on_test_connection delegate to the model
    //   * on_toggle_settings / on_close_settings toggle the model
    //
    // To exercise the sink path we would need to either expose a test
    // constructor for ViewSink or make the sink generic. Skip for now.

    struct FakeViewState {
        host: Arc<Mutex<String>>,
    }
    impl ViewState for FakeViewState {
        fn ollama_host(&self) -> String { self.host.lock().unwrap().clone() }
        fn selected_agent_token(&self) -> Option<String> { None }
        fn selected_model_name(&self) -> Option<String> { None }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    struct TestRig {
        controller: Arc<AppController>,
        sink_rx: Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<ViewCommand>>>,
        inner: Arc<Mutex<FakeInner>>,
    }
    fn make_rig(world: WorldSnapshot, prefs: Prefs, host: &str) -> TestRig {
        let inner = Arc::new(Mutex::new(FakeInner {
            world: Some(Ok(world)),
            test: None,
            launches: Vec::new(),
            restores: Vec::new(),
        }));
        let repo: Arc<dyn Repository> = Arc::new(FakeRepository(inner.clone()));
        let model = AppModel::new(repo, prefs);
        let view_state = Box::new(FakeViewState { host: Arc::new(Mutex::new(host.to_string())) });
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = ViewSink::for_test(tx);
        let controller = AppController::new(model, sink, view_state);
        controller.install_weak();
        TestRig {
            controller,
            sink_rx: Arc::new(Mutex::new(rx)),
            inner,
        }
    }
    /// Drain every command the controller has emitted up to now.
    fn drain(rig: &TestRig) -> Vec<ViewCommand> {
        let mut rx = rig.sink_rx.lock().unwrap();
        let mut out = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            out.push(cmd);
        }
        out
    }

    // ─────────────── on_launch ───────────────

    #[test]
    fn on_launch_invalid_index_publishes_error_status() {
        let _home = HomeGuard::new("llaunchpad-ctrl-test");
        let rig = make_rig(world(vec![agent("claude", "Claude", false)]), Prefs::default(), "http://h");
        // No world refresh -> agent_by_index(0) returns None.
        rig.controller.on_launch(0, "gpt-oss:120b-cloud".into());
        let s = rig.controller.model.snapshot();
        assert_eq!(s.status.kind, 2);
        assert_eq!(s.status.message, "✗ Invalid agent");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_launch_valid_index_spawns_repo_call_and_persists_prefs() {
        let _home = HomeGuard::new("llaunchpad-ctrl-test");
        let rig = make_rig(
            world(vec![agent("claude", "Claude", false), agent("vscode", "VS Code", true)]),
            Prefs::default(),
            "http://myhost",
        );
        rig.controller.model.refresh().await;
        rig.controller.on_launch(1, "qwen3-coder:cloud".into());
        // spawn is async; let the runtime drain it.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // launches recorded with the right names and host
        let launches = rig.inner.lock().unwrap().launches.clone();
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].0, "vscode");
        assert_eq!(launches[0].1, "qwen3-coder:cloud");
        assert_eq!(launches[0].2.as_deref(), Some("http://myhost"));
        // prefs persisted
        let prefs = crate::config::load();
        assert_eq!(prefs.agent, "vscode");
        assert_eq!(prefs.model, "qwen3-coder:cloud");
    }

    // ─────────────── on_restore ───────────────

    #[tokio::test(flavor = "current_thread")]
    async fn on_restore_with_valid_index_calls_repo() {
        let _home = HomeGuard::new("llaunchpad-ctrl-test");
        let rig = make_rig(
            world(vec![agent("claude", "Claude", false)]),
            Prefs::default(),
            "http://h",
        );
        rig.controller.model.refresh().await;
        rig.controller.on_restore(0);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(rig.inner.lock().unwrap().restores, vec!["claude"]);
    }

    #[test]
    fn on_restore_invalid_index_publishes_error() {
        let _home = HomeGuard::new("llaunchpad-ctrl-test");
        let rig = make_rig(
            world(vec![agent("claude", "Claude", false)]),
            Prefs::default(),
            "http://h",
        );
        rig.controller.on_restore(99);
        assert_eq!(rig.controller.model.snapshot().status.kind, 2);
        assert!(rig.inner.lock().unwrap().restores.is_empty());
    }

    // ─────────────── on_refresh / on_test_connection ───────────────

    #[tokio::test(flavor = "current_thread")]
    async fn on_refresh_triggers_model_refresh() {
        let _home = HomeGuard::new("llaunchpad-ctrl-test");
        let rig = make_rig(
            world(vec![agent("claude", "Claude", false)]),
            Prefs::default(),
            "http://h",
        );
        assert!(rig.controller.model.snapshot().world.is_none());
        rig.controller.on_refresh();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(rig.controller.model.snapshot().world.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_test_connection_runs_repo_test_and_publishes_status() {
        let _home = HomeGuard::new("llaunchpad-ctrl-test");
        let rig = make_rig(
            world(vec![agent("claude", "Claude", false)]),
            Prefs::default(),
            "http://h",
        );
        rig.inner.lock().unwrap().test = Some(Ok(TestResult {
            info: "ollama v0.5".into(),
            local_models: vec!["llama3:latest".into()],
        }));
        rig.controller.on_test_connection("http://h".into());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let s = rig.controller.model.snapshot();
        assert_eq!(s.status.kind, 1);
        assert_eq!(s.local_models, vec!["llama3:latest"]);
    }

    // ─────────────── settings ───────────────

    #[test]
    fn on_toggle_settings_toggles_model_state() {
        let _home = HomeGuard::new("llaunchpad-ctrl-test");
        let rig = make_rig(world(vec![]), Prefs::default(), "http://h");
        assert!(!rig.controller.model.snapshot().settings_open);
        rig.controller.on_toggle_settings();
        assert!(rig.controller.model.snapshot().settings_open);
        rig.controller.on_close_settings();
        assert!(!rig.controller.model.snapshot().settings_open);
    }

    // ─────────────── selection / host edits ───────────────

    #[test]
    fn on_selection_changed_persists_to_prefs() {
        let _home = HomeGuard::new("llaunchpad-ctrl-test");
        let rig = make_rig(world(vec![]), Prefs::default(), "http://h");
        rig.controller.on_selection_changed(Some("codex-app".into()), Some("glm-4.6:cloud".into()));
        let prefs = crate::config::load();
        eprintln!("DEBUG: home={:?} agent={:?} model={:?}", std::env::var("HOME"), prefs.agent, prefs.model);
        assert_eq!(prefs.agent, "codex-app");
        assert_eq!(prefs.model, "glm-4.6:cloud");
    }

    #[test]
    fn on_ollama_host_edited_persists_url() {
        let _home = HomeGuard::new("llaunchpad-ctrl-test");
        let rig = make_rig(world(vec![]), Prefs::default(), "http://h");
        rig.controller.on_ollama_host_edited("http://remote:1234".into());
        let prefs = crate::config::load();
        assert_eq!(prefs.ollama_host, "http://remote:1234");
        assert_eq!(rig.controller.model.snapshot().ollama_host, "http://remote:1234");
    }

    // ─────────────── mirror loop pushes snapshots to the sink ───────────────

    #[tokio::test(flavor = "current_thread")]
    async fn start_mirror_pushes_apply_snapshot_to_sink() {
        let _home = HomeGuard::new("llaunchpad-ctrl-test");
        let rig = make_rig(
            world(vec![agent("claude", "Claude", false)]),
            Prefs::default(),
            "http://h",
        );
        rig.controller.start(&tokio::runtime::Handle::current());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let cmds = drain(&rig);
        // We expect at least one ApplySnapshot (the initial one). The
        // exact count depends on whether the initial refresh also fired
        // — but at minimum the first push from `start()` is there.
        let snapshots: Vec<_> = cmds
            .iter()
            .filter_map(|c| match c {
                ViewCommand::ApplySnapshot(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(!snapshots.is_empty(), "expected at least one ApplySnapshot");
    }
}
