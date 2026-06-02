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
}

impl Default for StateSnapshot {
    fn default() -> Self {
        Self {
            ollama_host: config::Prefs::default().ollama_host,
            local_models: Vec::new(),
            world: None,
            status: Status { message: String::new(), kind: 0 },
            refreshing: false,
            settings_open: false,
            first_load: false,
            last_agent: None,
            last_model: None,
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
            last_agent: (!prefs.agent.is_empty()).then(|| prefs.agent.clone()),
            last_model: (!prefs.model.is_empty()).then(|| prefs.model.clone()),
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
    pub async fn launch(&self, agent: Agent, model: String, ollama_host: Option<String>) -> Result<()> {
        let host = ollama_host.as_deref();
        self.repo.launch_agent(&agent, &model, host).await
    }

    pub async fn restore(&self, agent_token: String) -> Result<()> {
        self.repo.restore_agent(&agent_token).await
    }

    pub fn is_agent_restorable(&self, agent_token: &str) -> bool {
        self.repo.restore_available(agent_token)
    }
}
