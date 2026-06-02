//! Controller layer.
//!
//! Glue between the Model and the View. Lives behind a `dyn Controller`
//! trait that the Slint callbacks call into. Owns the 5s background
//! poller and a tokio task that mirrors Model state into the ViewSink.
//! All setter calls on the View go through the sink, which the SlintAppView
//! drains on the UI thread via a timer tick.

use crate::model::{AppModel, StateSnapshot, Status};
use crate::view::{Controller, ViewSink, ViewState};
use std::sync::Arc;
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

