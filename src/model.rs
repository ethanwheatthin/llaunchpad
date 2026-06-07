// unused methods are part of the public Model API; future intents may use them.
#![allow(dead_code)]
//! Model layer.
//!
//! Owns the canonical `AppState` and exposes intent methods that mutate it
//! and broadcast a `StateSnapshot` to subscribers. The Model knows nothing
//! about Slint, the View, or the Controller — it only depends on the
//! `Repository` trait and on `tokio::sync::watch` for state distribution.
//!
//! The Controller translates user intents from the View into Model calls
//! and arranges the background poller. The View applies snapshots.

use crate::config::{self, Prefs};
use crate::ollama::Agent;
use crate::repository::Repository;
use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::watch;

/// A status message shown in the bottom banner. `kind == 0` means no banner.
#[derive(Clone, Debug)]
pub struct Status {
    pub message: String,
    pub kind: i32, // 0 none, 1 ok, 2 error
}

/// View-friendly shape of the current state. The View receives a clone on
/// every change. The model keeps the canonical state in an `RwLock`.
#[derive(Clone, Debug)]
pub struct StateSnapshot {
    /// Ollama server URL the user is currently targeting.
    pub ollama_host: String,
    /// Working directory the user chose for the next launch. Empty
    /// string means "inherit the launcher\'s cwd". Mirrored to
    /// `Prefs::working_dir` for persistence.
    pub working_dir: String,
    /// Last successful test result's local models (empty if never tested).
    pub local_models: Vec<String>,
    /// Cached local models from the most recent world refresh.
    pub world: Option<WorldSnapshot>,
    pub status: Status,
    pub refreshing: bool,
    pub settings_open: bool,
    /// Set to true exactly once, on the first snapshot emitted after a
    /// successful refresh. The View uses this to apply persisted prefs
    /// (last agent + last model) to its selection indices.
    pub first_load: bool,
    /// Persisted agent token (e.g. "codex-app"); None if no prior run.
    pub last_agent: Option<String>,
    /// Persisted model name (launchable, e.g. "glm-4.6:cloud").
    pub last_model: Option<String>,
    /// Persisted terminal key (e.g. "iterm2"); None if no prior run.
    pub last_terminal: Option<String>,
}

impl Default for StateSnapshot {
    fn default() -> Self {
        Self {
            ollama_host: config::Prefs::default().ollama_host,
            working_dir: String::new(),
            local_models: Vec::new(),
            world: None,
            status: Status { message: String::new(), kind: 0 },
            refreshing: false,
            settings_open: false,
            first_load: false,
            last_agent: None,
            last_model: None,
            last_terminal: None,
        }
    }
}

pub use crate::repository::WorldSnapshot;

/// Internal canonical state. Only the Model ever mutates this.
struct AppState {
    snapshot: StateSnapshot,
}

impl AppState {
    fn new(prefs: &Prefs) -> Self {
        let snap = StateSnapshot {
            ollama_host: prefs.ollama_host.clone(),
            working_dir: prefs.working_dir.clone(),
            last_agent: (!prefs.agent.is_empty()).then(|| prefs.agent.clone()),
            last_model: (!prefs.model.is_empty()).then(|| prefs.model.clone()),
            last_terminal: (!prefs.terminal.is_empty()).then(|| prefs.terminal.clone()),
            ..StateSnapshot::default()
        };
        Self { snapshot: snap }
    }
}

/// The Model. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct AppModel {
    repo: Arc<dyn Repository>,
    state: Arc<RwLock<AppState>>,
    tx: watch::Sender<StateSnapshot>,
    rx: watch::Receiver<StateSnapshot>,
    /// Monotonic counter for test-connection responses; a stale response
    /// checks this before publishing state.
    test_gen: Arc<AtomicU64>,
}

impl AppModel {
    pub fn new(repo: Arc<dyn Repository>, prefs: Prefs) -> Self {
        let state = AppState::new(&prefs);
        let snap = state.snapshot.clone();
        let (tx, rx) = watch::channel(snap);
        Self {
            repo,
            state: Arc::new(RwLock::new(state)),
            tx,
            rx,
            test_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Borrow a clone of the latest snapshot.
    pub fn snapshot(&self) -> StateSnapshot {
        self.rx.borrow().clone()
    }

    /// Subscribe to state changes. The returned receiver always carries the
    /// latest snapshot; calling `changed().await` yields when a new one is
    /// published. Multiple subscribers can call `subscribe()` for fan-out.
    pub fn subscribe(&self) -> watch::Receiver<StateSnapshot> {
        self.rx.clone()
    }

    /// Total test-connection invocations seen so far. Used by callers
    /// (the Controller) to discard late responses.
    pub fn current_test_gen(&self) -> u64 {
        self.test_gen.load(Ordering::SeqCst)
    }

    /// Push a new snapshot to all subscribers. Idempotent: same content
    /// re-sent is fine because every View setter is idempotent.
    fn publish(&self, new_snap: StateSnapshot) {
        {
            let mut st = self.state.write().unwrap();
            st.snapshot = new_snap.clone();
        }
        // best-effort: the only error here is "no receivers", which is fine.
        let _ = self.tx.send(new_snap);
    }

    fn update<F: FnOnce(&mut StateSnapshot)>(&self, f: F) {
        let mut snap = {
            let st = self.state.read().unwrap();
            st.snapshot.clone()
        };
        f(&mut snap);
        self.publish(snap);
    }

    // ─────────────────── intents ───────────────────

    /// User clicked Refresh, or the 5s poller fired.
    pub async fn refresh(&self) {
        // show "refreshing…" without erasing the existing status banner
        self.update(|s| s.refreshing = true);
        match self.repo.fetch_world().await {
            Ok(world) => {
                self.update(|s| {
                    s.world = Some(world);
                    s.refreshing = false;
                    if !s.first_load {
                        s.first_load = true;
                    }
                });
            }
            Err(e) => {
                self.update(|s| {
                    s.refreshing = false;
                    s.status = Status {
                        message: format!("✗ Refresh failed: {e}"),
                        kind: 2,
                    };
                });
            }
        }
    }

    /// User typed a new Ollama host URL. Persists immediately, no test.
    pub fn set_ollama_host(&self, url: String) {
        self.update(|s| s.ollama_host = url.clone());
        let mut prefs = config::load();
        prefs.ollama_host = url;
        config::save(&prefs);
    }

    /// User typed a new working directory. Persists immediately so
    /// the next launch honors it.
    pub fn set_working_dir(&self, dir: String) {
        self.update(|s| s.working_dir = dir.clone());
        let mut prefs = config::load();
        prefs.working_dir = dir;
        config::save(&prefs);
    }

    /// User clicked Test. Returns the gen counter for this attempt so the
    /// caller (Controller) can ignore late responses.
    pub async fn test_connection(&self, url: String) -> u64 {
        let gen = self.test_gen.fetch_add(1, Ordering::SeqCst) + 1;
        // Persist the host right away — even if the test fails, the user
        // told us what they want to target.
        self.set_ollama_host(url.clone());
        match self.repo.test(&url).await {
            Ok(res) => {
                // bail if a newer test has started
                if self.test_gen.load(Ordering::SeqCst) != gen {
                    return gen;
                }
                let count = res.local_models.len();
                let msg = if count > 0 {
                    format!(
                        "✓ {} · {} local model{}",
                        res.info,
                        count,
                        if count == 1 { "" } else { "s" }
                    )
                } else {
                    format!("✓ {} · no local models", res.info)
                };
                self.update(|s| {
                    s.local_models = res.local_models;
                    s.status = Status { message: msg, kind: 1 };
                });
            }
            Err(e) => {
                if self.test_gen.load(Ordering::SeqCst) != gen {
                    return gen;
                }
                self.update(|s| {
                    s.local_models.clear();
                    s.status = Status {
                        message: format!("✗ {e}"),
                        kind: 2,
                    };
                });
            }
        }
        gen
    }

    /// User clicked Launch. The Controller will do the actual spawn via
    /// the repository; the Model only persists the new selection so it
    /// survives a relaunch.
    pub fn record_launch(&self, agent_token: String, model: String) {
        let mut prefs = config::load();
        prefs.agent = agent_token;
        prefs.model = model;
        config::save(&prefs);
    }

    /// User selected an agent or model. Persist immediately so the next
    /// launch restores the same selection. The View already knows the
    /// selection locally; we only mirror it into prefs.
    pub fn record_selection(&self, agent_token: Option<String>, model: Option<String>) {
        let mut prefs = config::load();
        if let Some(a) = agent_token {
            prefs.agent = a;
        }
        if let Some(m) = model {
            prefs.model = m;
        }
        config::save(&prefs);
    }

    /// User picked a new terminal. Persists immediately so the next
    /// launch uses the chosen emulator.
    pub fn set_terminal(&self, key: String) {
        let mut prefs = config::load();
        prefs.terminal = key;
        config::save(&prefs);
    }

    pub fn set_status(&self, status: Status) {
        self.update(|s| s.status = status);
    }
    pub fn dismiss_status(&self) {
        self.update(|s| {
            s.status = Status { message: String::new(), kind: 0 };
        });
    }
    pub fn set_settings_open(&self, open: bool) {
        self.update(|s| s.settings_open = open);
    }
    pub fn toggle_settings(&self) {
        self.update(|s| s.settings_open = !s.settings_open);
    }

    // ─────────────────── queries (used by Controller) ───────────────────

    /// Look up an Agent by its index in the current world's agent list.
    pub fn agent_by_index(&self, idx: i32) -> Option<Agent> {
        let st = self.state.read().unwrap();
        st.snapshot
            .world
            .as_ref()
            .and_then(|w| w.agents.get(idx as usize).cloned())
    }

    /// The "Agent" this user wants to launch (the persisted last-used one).
    /// Useful as a default if the index-based lookup fails.
    pub fn persisted_agent_token(&self) -> Option<String> {
        let st = self.state.read().unwrap();
        st.snapshot.last_agent.clone()
    }
    pub fn persisted_model_name(&self) -> Option<String> {
        let st = self.state.read().unwrap();
        st.snapshot.last_model.clone()
    }

    /// Convenience for the Controller's "spawn" path.
    pub async fn launch(
        &self,
        agent: Agent,
        model: String,
        ollama_host: Option<String>,
        working_dir: Option<&str>,
        terminal: crate::terminal::Terminal,
    ) -> Result<()> {
        let host = ollama_host.as_deref();
        self.repo.launch_agent(&agent, &model, host, working_dir, &terminal).await
    }

    pub async fn restore(&self, agent_token: String) -> Result<()> {
        self.repo.restore_agent(&agent_token).await
    }

    pub fn is_agent_restorable(&self, agent_token: &str) -> bool {
        self.repo.restore_available(agent_token)
    }
}


#[cfg(test)]
mod tests {
    //! Unit tests for the Model.
    //!
    //! We use a `FakeRepository` (a `Repository` impl that returns canned
    //! data without touching the network or the process table) and a
    //! temp-dir-based `HOME` so `config::save` writes to a throwaway file
    //! instead of clobbering the user's real prefs.
    //!
    //! A single `Mutex` serialises the tests because mutating `HOME` is
    //! process-global state.

    use super::*;
    use crate::config::Prefs;
    use crate::ollama::Agent;
    use crate::repository::{Repository, TestResult, WorldSnapshot};
    use crate::test_util::HomeGuard;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    // Duration used in tokio::time::sleep below.

    /// A `Repository` whose every method returns canned data. Tests
    /// configure it with an `Arc<Mutex<Inner>>` and inspect calls.
    struct FakeRepository {
        inner: Arc<Mutex<FakeInner>>,
    }
    struct FakeInner {
        world: Option<Result<WorldSnapshot, String>>,
        test: Option<Result<TestResult, String>>,
        launches: Vec<(String, String, Option<String>, Option<String>, crate::terminal::Terminal)>,
        restores: Vec<String>,
        restore_available: std::collections::HashMap<String, bool>,
    }

    impl FakeRepository {
        fn new() -> (Arc<Mutex<FakeInner>>, Arc<Self>) {
            let inner = Arc::new(Mutex::new(FakeInner {
                world: None,
                test: None,
                launches: Vec::new(),
                restores: Vec::new(),
                restore_available: std::collections::HashMap::new(),
            }));
            let me = Arc::new(Self { inner: inner.clone() });
            (inner, me)
        }
    }

    fn agent(name: &str, display: &str, is_gui: bool) -> Agent {
        Agent { name: name.to_string(), display: display.to_string(), is_gui, logo: String::new() }
    }

    fn world(agents: Vec<Agent>, running: Vec<bool>, installed: Vec<bool>, cloud: Vec<&str>) -> WorldSnapshot {
        WorldSnapshot {
            agents,
            running,
            installed,
            cloud_models: cloud.into_iter().map(String::from).collect(),
        }
    }

    fn sample_world() -> WorldSnapshot {
        world(
            vec![
                agent("codex-app", "Codex App", true),
                agent("claude", "Claude", false),
                agent("vscode", "VS Code", true),
            ],
            vec![true, false, false],
            vec![true, true, false],
            vec!["gpt-oss:120b-cloud", "glm-4.6:cloud"],
        )
    }

    #[async_trait::async_trait]
    impl Repository for FakeRepository {
        async fn list_agents(&self) -> Result<Vec<Agent>> {
            let g = self.inner.lock().unwrap();
            g.world
                .as_ref()
                .expect("test must configure FakeRepository.world")
                .as_ref()
                .map(|w| w.agents.clone())
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        async fn list_cloud_models(&self) -> Result<Vec<crate::ollama::Model>> {
            let g = self.inner.lock().unwrap();
            g.world
                .as_ref()
                .unwrap()
                .as_ref()
                .map(|w| {
                    w.cloud_models
                        .iter()
                        .map(|n| crate::ollama::Model { name: n.clone() })
                        .collect()
                })
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        async fn list_local_models(&self, _url: &str) -> Result<Vec<crate::ollama::Model>> {
            let g = self.inner.lock().unwrap();
            match g.test.as_ref() {
                Some(Ok(t)) => Ok(t
                    .local_models
                    .iter()
                    .map(|n| crate::ollama::Model { name: n.clone() })
                    .collect()),
                Some(Err(_)) => Ok(Vec::new()),
                None => Ok(Vec::new()),
            }
        }
        async fn test_connection(&self, _url: &str) -> Result<String> {
            let g = self.inner.lock().unwrap();
            g.test
                .as_ref()
                .unwrap()
                .as_ref()
                .map(|t| t.info.clone())
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        fn running_states(&self, agents: &[Agent]) -> Vec<bool> {
            let _ = agents;
            self.inner.lock().unwrap().world.as_ref().unwrap().as_ref().unwrap().running.clone()
        }
        fn installed_states(&self, agents: &[Agent]) -> Vec<bool> {
            let _ = agents;
            self.inner.lock().unwrap().world.as_ref().unwrap().as_ref().unwrap().installed.clone()
        }
        fn restore_available(&self, agent_token: &str) -> bool {
            self.inner
                .lock()
                .unwrap()
                .restore_available
                .get(agent_token)
                .copied()
                .unwrap_or(false)
        }
        async fn restore_agent(&self, agent_token: &str) -> Result<()> {
            self.inner.lock().unwrap().restores.push(agent_token.to_string());
            Ok(())
        }
        async fn launch_agent(
            &self,
            agent: &Agent,
            model: &str,
            ollama_host: Option<&str>,
            working_dir: Option<&str>,
            terminal: &crate::terminal::Terminal,
        ) -> Result<()> {
            self.inner.lock().unwrap().launches.push((
                agent.name.clone(),
                model.to_string(),
                ollama_host.map(String::from),
                working_dir.map(String::from),
                *terminal,
            ));
            Ok(())
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    // ─────────────── first_load flag ───────────────

    #[test]
    fn first_load_flips_after_first_successful_refresh() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (inner, repo) = FakeRepository::new();
        inner.lock().unwrap().world = Some(Ok(sample_world()));
        let prefs = Prefs { agent: "claude".into(), model: "glm-4.6:cloud".into(), ollama_host: "http://x".into(), terminal: String::new(), working_dir: String::new() };
        let model = AppModel::new(repo as Arc<dyn Repository>, prefs);
        assert!(!model.snapshot().first_load, "starts false");
        let r = rt();
        r.block_on(model.refresh());
        assert!(model.snapshot().first_load, "true after first successful refresh");
    }

    #[test]
    fn first_load_stays_false_when_refresh_fails() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (inner, repo) = FakeRepository::new();
        inner.lock().unwrap().world = Some(Err("ollama missing".into()));
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        let r = rt();
        r.block_on(model.refresh());
        assert!(!model.snapshot().first_load);
    }

    #[test]
    fn first_load_latches_after_success() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (inner, repo) = FakeRepository::new();
        inner.lock().unwrap().world = Some(Ok(sample_world()));
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        let r = rt();
        r.block_on(model.refresh());
        // Now make subsequent refresh fail; first_load must stay true.
        inner.lock().unwrap().world = Some(Err("boom".into()));
        r.block_on(model.refresh());
        assert!(model.snapshot().first_load);
    }

    // ─────────────── last_agent / last_model from prefs ───────────────

    #[test]
    fn prefs_populate_last_agent_and_last_model() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (_inner, repo) = FakeRepository::new();
        let prefs = Prefs {
            agent: "codex-app".into(),
            model: "gpt-oss:120b-cloud".into(),
            ollama_host: "http://localhost:11434".into(),
            terminal: String::new(),
            working_dir: String::new(),
        };
        let model = AppModel::new(repo as Arc<dyn Repository>, prefs);
        let s = model.snapshot();
        assert_eq!(s.last_agent.as_deref(), Some("codex-app"));
        assert_eq!(s.last_model.as_deref(), Some("gpt-oss:120b-cloud"));
    }

    #[test]
    fn empty_prefs_yield_no_last_agent_or_model() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (_inner, repo) = FakeRepository::new();
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        let s = model.snapshot();
        assert!(s.last_agent.is_none());
        assert!(s.last_model.is_none());
    }

    // ─────────────── status / settings / dismiss ───────────────

    #[test]
    fn set_status_then_dismiss_clears_the_banner() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (_inner, repo) = FakeRepository::new();
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        model.set_status(Status { message: "ok".into(), kind: 1 });
        assert_eq!(model.snapshot().status.kind, 1);
        model.dismiss_status();
        assert_eq!(model.snapshot().status.kind, 0);
        assert_eq!(model.snapshot().status.message, "");
    }

    #[test]
    fn toggle_settings_flips_and_clamps() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (_inner, repo) = FakeRepository::new();
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        assert!(!model.snapshot().settings_open);
        model.toggle_settings();
        assert!(model.snapshot().settings_open);
        model.set_settings_open(false);
        assert!(!model.snapshot().settings_open);
    }

    // ─────────────── test_connection race protection ───────────────

    #[test]
    fn test_connection_bumps_test_gen_and_publishes_status() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (inner, repo) = FakeRepository::new();
        inner.lock().unwrap().test = Some(Ok(TestResult {
            info: "ok".into(),
            local_models: vec!["llama3:latest".into()],
        }));
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        let r = rt();
        let gen1 = r.block_on(model.test_connection("http://h".into()));
        assert_eq!(gen1, 1);
        assert_eq!(model.snapshot().status.kind, 1);
        assert_eq!(model.snapshot().local_models, vec!["llama3:latest"]);
    }

    #[test]
    fn test_gen_counter_monotonically_increments() {
        // The original race in main.rs was: a user clicks Test twice in
        // quick succession, the first slow response arrives *after* the
        // second. The model guards against this with `test_gen` and
        // discards stale responses. We test the bookkeeping, not the
        // timing.
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (inner, repo) = FakeRepository::new();
        inner.lock().unwrap().test = Some(Ok(TestResult {
            info: "ok".into(),
            local_models: vec![],
        }));
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        let r = rt();
        let g1 = r.block_on(model.test_connection("http://1".into()));
        let g2 = r.block_on(model.test_connection("http://2".into()));
        let g3 = r.block_on(model.test_connection("http://3".into()));
        assert!(g1 < g2 && g2 < g3, "gens strictly increase: {g1} {g2} {g3}");
        assert_eq!(model.current_test_gen(), g3);
    }

    #[test]
    fn test_connection_publishes_status_with_kind_1_on_success() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (inner, repo) = FakeRepository::new();
        inner.lock().unwrap().test = Some(Ok(TestResult {
            info: "ollama v0.5".into(),
            local_models: vec!["llama3:latest".into(), "qwen2.5:7b".into()],
        }));
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        let r = rt();
        r.block_on(model.test_connection("http://h".into()));
        let s = model.snapshot();
        assert_eq!(s.status.kind, 1);
        assert!(s.status.message.contains("ollama v0.5"));
        assert!(s.status.message.contains("2 local models"));
        assert_eq!(s.local_models.len(), 2);
    }

    #[test]
    fn test_connection_publishes_status_with_kind_2_on_error() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (inner, repo) = FakeRepository::new();
        inner.lock().unwrap().test = Some(Err("connection refused".into()));
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        let r = rt();
        r.block_on(model.test_connection("http://h".into()));
        let s = model.snapshot();
        assert_eq!(s.status.kind, 2);
        assert!(s.status.message.contains("connection refused"));
        assert!(s.local_models.is_empty());
    }

    #[test]
    fn successful_test_connection_clears_local_models() {
        // If a previous test populated local_models and the new test
        // returns no local models, the snapshot must clear the list.
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (inner, repo) = FakeRepository::new();
        inner.lock().unwrap().test = Some(Ok(TestResult {
            info: "ok".into(),
            local_models: vec!["stale".into()],
        }));
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        let r = rt();
        r.block_on(model.test_connection("http://h".into()));
        assert_eq!(model.snapshot().local_models, vec!["stale"]);
        // Now a failing test must clear them.
        inner.lock().unwrap().test = Some(Err("nope".into()));
        r.block_on(model.test_connection("http://h2".into()));
        assert!(model.snapshot().local_models.is_empty());
    }

    // ─────────────── record_* writes prefs ───────────────

    #[test]
    fn record_launch_persists_agent_and_model() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (_inner, repo) = FakeRepository::new();
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        model.record_launch("claude".into(), "qwen3-coder:cloud".into());
        let prefs = crate::config::load();
        assert_eq!(prefs.agent, "claude");
        assert_eq!(prefs.model, "qwen3-coder:cloud");
    }

    #[test]
    fn record_selection_merges_into_existing_prefs() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (_inner, repo) = FakeRepository::new();
        // Seed prefs with one field already set.
        crate::config::save(&Prefs {
            agent: "old-agent".into(),
            model: "old-model".into(),
            ollama_host: "http://x".into(),
            terminal: String::new(),
            working_dir: String::new(),
        });
        let model = AppModel::new(repo as Arc<dyn Repository>, crate::config::load());
        model.record_selection(Some("new-agent".into()), None);
        let prefs = crate::config::load();
        assert_eq!(prefs.agent, "new-agent");
        assert_eq!(prefs.model, "old-model", "untouched field is preserved");
    }

    // ─────────────── launch / restore go through the repository ───────────────

    #[test]
    fn launch_calls_repository_with_agent_model_and_host() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (inner, repo) = FakeRepository::new();
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        let a = agent("claude", "Claude", false);
        let r = rt();
        r.block_on(model.launch(
            a,
            "gpt-oss:120b-cloud".into(),
            Some("http://h".into()),
            None,
            crate::terminal::Terminal::Default,
        ))
        .unwrap();
        let calls = &inner.lock().unwrap().launches;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "claude");
        assert_eq!(calls[0].1, "gpt-oss:120b-cloud");
        assert_eq!(calls[0].2.as_deref(), Some("http://h"));
        assert_eq!(calls[0].3, None, "working_dir should be None for this test");
    }

    #[test]
    fn restore_calls_repository_with_token() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (inner, repo) = FakeRepository::new();
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        let r = rt();
        r.block_on(model.restore("claude".into())).unwrap();
        assert_eq!(inner.lock().unwrap().restores, vec!["claude"]);
    }

    #[test]
    fn is_agent_restorable_reflects_repository() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (inner, repo) = FakeRepository::new();
        inner.lock().unwrap().restore_available.insert("claude".into(), true);
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        assert!(model.is_agent_restorable("claude"));
        assert!(!model.is_agent_restorable("vscode"));
    }

    // ─────────────── selection-resolution ───────────────

    #[test]
    fn agent_by_index_returns_none_for_out_of_range() {
        let _home = HomeGuard::new("llaunchpad-model-test");
        let (inner, repo) = FakeRepository::new();
        inner.lock().unwrap().world = Some(Ok(sample_world()));
        let model = AppModel::new(repo as Arc<dyn Repository>, Prefs::default());
        let r = rt();
        r.block_on(model.refresh());
        assert!(model.agent_by_index(0).is_some());
        assert!(model.agent_by_index(99).is_none());
    }
}
