// selected_* methods on ViewState + on_* handlers on Controller may be wired later.
#![allow(dead_code)]
//! View layer.
//!
//! Single-responsibility: turn a stream of `ViewCommand`s into visible
//! state on a Slint `AppWindow`. The View is owned by the main thread,
//! runs no async code, and exposes a small `tick()` that the Slint
//! timer drives every frame. The cross-thread side is the `ViewSink`,
//! a clonable `mpsc::UnboundedSender<ViewCommand>` the Controller and
//! Model mirror loops can push to from any thread.

use crate::model::{StateSnapshot, WorldSnapshot};
use crate::slint_generated::{AgentItem, AppWindow, ModelItem};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::rc::Rc;
use std::sync::Mutex;
use tokio::sync::mpsc;

/// One marshalled setter the View will apply on the next tick.
#[derive(Clone, Debug)]
pub enum ViewCommand {
    ApplySnapshot(StateSnapshot),
    SetStatus { message: String, kind: i32 },
}

/// Clonable, cross-thread handle to the View. Send from a tokio worker;
/// drained on the UI thread by `SlintAppView::tick`.
#[derive(Clone)]
pub struct ViewSink {
    tx: mpsc::UnboundedSender<ViewCommand>,
}

impl ViewSink {
    pub fn apply_snapshot(&self, snap: StateSnapshot) {
        let _ = self.tx.send(ViewCommand::ApplySnapshot(snap));
    }
    pub fn set_status(&self, message: String, kind: i32) {
        let _ = self.tx.send(ViewCommand::SetStatus { message, kind });
    }
}

/// Read-only view-state (selected agent/model, ollama host) used by
/// the Controller when it needs to look something up. Implementations
/// are passed by the main-thread View to the Controller and are
/// only ever called from the UI thread.
pub trait ViewState: Send + Sync {
    fn ollama_host(&self) -> String;
    fn selected_agent_token(&self) -> Option<String>;
    fn selected_model_name(&self) -> Option<String>;
}

/// Controller callbacks the View invokes when the user interacts with
/// the UI. All are non-async; the Controller can spawn tokio work if
/// it needs to.
pub trait Controller: Send + Sync {
    fn on_launch(&self, agent_idx: i32, model: String);
    fn on_restore(&self, agent_idx: i32);
    fn on_refresh(&self);
    fn on_test_connection(&self, url: String);
    fn on_dismiss_status(&self);
    fn on_toggle_settings(&self);
    fn on_close_settings(&self);
    fn on_selection_changed(&self, agent: Option<String>, model: Option<String>);
    fn on_ollama_host_edited(&self, url: String);
}

// ───────────────────── helpers ─────────────────────

pub fn initials(display: &str) -> String {
    let words: Vec<&str> = display
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    match words.as_slice() {
        [] => "?".to_string(),
        [one] => one.chars().take(2).collect::<String>().to_uppercase(),
        [a, b, ..] => format!(
            "{}{}",
            a.chars().next().unwrap_or('?'),
            b.chars().next().unwrap_or('?')
        )
        .to_uppercase(),
    }
}

const PALETTE_LEN: i32 = 13;
pub fn color_index(token: &str) -> i32 {
    let sum: u32 = token.bytes().map(|b| b as u32).sum();
    (sum % PALETTE_LEN as u32) as i32
}

// ───────────────────── SlintAppView ─────────────────────

pub struct SlintAppView {
    ui: AppWindow,
    controller: std::sync::Mutex<Option<std::sync::Weak<dyn Controller>>>,
    rx: Mutex<mpsc::UnboundedReceiver<ViewCommand>>,
    sink: ViewSink,
    last: Mutex<Option<StateSnapshot>>,
    last_agents_sig: Mutex<Vec<String>>,
    last_models_sig: Mutex<Vec<String>>,
    /// Last selection / host URL the View observed, used to detect
    /// user-driven changes and notify the Controller. The Controller
    /// then persists to prefs.
    last_user: Mutex<UserSelection>,
    /// Whether the View has already consumed the first-load prefs
    /// (i.e. the user has either accepted the persisted selection or
    /// the View has no persisted selection to restore). After this is
    /// `true`, the View never falls back to prefs on a refresh; the
    /// current Slint selection (or "no selection") is the source of
    /// truth.
    first_load_applied: Mutex<bool>,
}

#[derive(Clone, Default)]
struct UserSelection {
    sel_agent: i32,
    sel_model: i32,
    ollama_host: String,
}

impl SlintAppView {
    pub fn new() -> Rc<Self> {
        let ui = AppWindow::new().expect("failed to create AppWindow");
        ui.set_version(env!("CARGO_PKG_VERSION").into());
        let (tx, rx) = mpsc::unbounded_channel();
        let sink = ViewSink { tx };
        Rc::new(Self {
            ui,
            controller: std::sync::Mutex::new(None),
            rx: Mutex::new(rx),
            sink,
            last: Mutex::new(None),
            last_agents_sig: Mutex::new(Vec::new()),
            last_models_sig: Mutex::new(Vec::new()),
            last_user: Mutex::new(UserSelection::default()),
            first_load_applied: Mutex::new(false),
        })
    }

    /// Cross-thread handle that workers use to push view commands.
    pub fn sink(&self) -> ViewSink {
        self.sink.clone()
    }

    /// Borrow a `ViewState` snapshot for the Controller. Cheap; the
    /// returned trait object is `Send + Sync` so it can be moved into
    /// a tokio task.
    pub fn view_state(&self) -> Box<dyn ViewState> {
        let weak = self.ui.as_weak();
        Box::new(SlintViewState { ui_weak: weak })
    }

    /// Wire every `on_*` Slint callback to the Controller. The
    /// Controller is held weakly so dropping it leaves the Slint
    /// callbacks no-oping.
    pub fn attach_controller(self: &Rc<Self>, controller: std::sync::Weak<dyn Controller>) {
        *self.controller.lock().unwrap() = Some(controller.clone());
        // dismiss banner
        {
            let c = controller.clone();
            let weak = self.ui.as_weak();
            self.ui.on_dismiss(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_status("".into());
                    ui.set_status_kind(0);
                }
                if let Some(cc) = c.upgrade() {
                    cc.on_dismiss_status();
                }
            });
        }
        // toggle settings
        {
            let c = controller.clone();
            let weak = self.ui.as_weak();
            self.ui.on_toggle_settings(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_settings_open(!ui.get_settings_open());
                }
                if let Some(cc) = c.upgrade() {
                    cc.on_toggle_settings();
                }
            });
        }
        // close settings
        {
            let c = controller.clone();
            let weak = self.ui.as_weak();
            self.ui.on_close_settings(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_settings_open(false);
                }
                if let Some(cc) = c.upgrade() {
                    cc.on_close_settings();
                }
            });
        }
        // test connection
        {
            let c = controller.clone();
            self.ui.on_test_connection(move |url| {
                if let Some(cc) = c.upgrade() {
                    cc.on_test_connection(url.to_string());
                }
            });
        }
        // refresh
        {
            let c = controller.clone();
            self.ui.on_refresh(move || {
                if let Some(cc) = c.upgrade() {
                    cc.on_refresh();
                }
            });
        }
        // launch
        {
            let c = controller.clone();
            self.ui.on_launch(move |idx, model| {
                if let Some(cc) = c.upgrade() {
                    cc.on_launch(idx, model.to_string());
                }
            });
        }
        // restore
        {
            let c = controller.clone();
            self.ui.on_restore(move |idx| {
                if let Some(cc) = c.upgrade() {
                    cc.on_restore(idx);
                }
            });
        }
    }

    /// Run the Slint event loop. Blocks until the window closes.
    pub fn run(&self) -> anyhow::Result<()> {
        self.ui.run()?;
        Ok(())
    }

    /// Drain any pending `ViewCommand`s from the cross-thread channel
    /// and apply them to the live Slint window. Also detects user-driven
    /// selection / host changes and notifies the Controller. Must be
    /// called from the UI thread (e.g. from a `slint::Timer`).
    pub fn tick(&self) {
        let mut rx = self.rx.lock().unwrap();
        while let Ok(cmd) = rx.try_recv() {
            drop(rx);
            self.apply_command(cmd);
            rx = self.rx.lock().unwrap();
        }
        drop(rx);
        self.detect_user_changes();
    }

    fn detect_user_changes(&self) {
        let sa = self.ui.get_sel_agent_index();
        let sm = self.ui.get_sel_model_index();
        let host = self.ui.get_ollama_host().to_string();
        let mut last = self.last_user.lock().unwrap();
        let ctrl = self
            .controller
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|w| w.upgrade());
        if sa != last.sel_agent || sm != last.sel_model {
            let agent_name = if sa >= 0 {
                self.ui
                    .get_agents()
                    .row_data(sa as usize)
                    .map(|a| a.name.to_string())
            } else {
                None
            };
            let model_name = if sm >= 0 {
                self.ui
                    .get_models()
                    .row_data(sm as usize)
                    .map(|m| m.name.to_string())
            } else {
                None
            };
            last.sel_agent = sa;
            last.sel_model = sm;
            if let Some(c) = ctrl.as_ref() {
                c.on_selection_changed(agent_name, model_name);
            }
        }
        if host != last.ollama_host {
            last.ollama_host = host.clone();
            if let Some(c) = ctrl.as_ref() {
                c.on_ollama_host_edited(host);
            }
        }
    }

    fn apply_command(&self, cmd: ViewCommand) {
        match cmd {
            ViewCommand::ApplySnapshot(snap) => self.apply_snapshot(&snap),
            ViewCommand::SetStatus { message, kind } => {
                self.ui.set_status(message.into());
                self.ui.set_status_kind(kind);
            }
        }
    }

    fn apply_snapshot(&self, snap: &StateSnapshot) {
        let prev = self.last.lock().unwrap().clone();
        let world = snap.world.as_ref();
        let local_models = &snap.local_models;

        // ---- agents ----
        if let Some(w) = world {
            let items = build_agent_items(w);
            let sig: Vec<String> = items
                .iter()
                .map(|it| {
                    format!(
                        "{}|{}|{}|{}",
                        it.name, it.running, it.restorable, it.installed
                    )
                })
                .collect();
            let agents_changed = self.last_agents_sig.lock().unwrap().as_slice() != sig.as_slice();
            if agents_changed {
                *self.last_agents_sig.lock().unwrap() = sig;
                let names: Vec<SharedString> =
                    w.agents.iter().map(|a| a.display.as_str().into()).collect();
                self.ui.set_agents(ModelRc::new(VecModel::from(items)));
                self.ui.set_agent_names(ModelRc::new(VecModel::from(names)));
            }
        }

        // ---- models ----
        let cloud = world.map(|w| w.cloud_models.clone()).unwrap_or_default();
        let merged = build_model_items(local_models, &cloud);
        let sig: Vec<String> = merged
            .iter()
            .map(|m| format!("{}|{}", m.name, m.is_local))
            .collect();
        let models_changed = self.last_models_sig.lock().unwrap().as_slice() != sig.as_slice();
        if models_changed {
            *self.last_models_sig.lock().unwrap() = sig;
            self.ui.set_models(ModelRc::new(VecModel::from(merged.clone())));
        }

        // ---- selection preservation ----
        //
        // The Model publishes a one-shot `snap.first_load` flag for the
        // very first snapshot that has a real world. The View uses it
        // exactly once to restore the persisted selection (last agent /
        // last model from prefs). After that, the current Slint
        // selection is the source of truth — the View never falls back
        // to prefs again, so the user's choice survives every refresh.
        //
        // If the user's chosen agent is no longer in the new world, we
        // clear the selection (-1) rather than re-applying prefs.
        let first_load_done = *self.first_load_applied.lock().unwrap();
        if snap.first_load && !first_load_done {
            // First ever apply: honour prefs if present.
            if let (Some(name), Some(w)) = (snap.last_agent.as_ref(), world) {
                if let Some(i) = w.agents.iter().position(|a| a.name == *name) {
                    self.ui.set_sel_agent_index(i as i32);
                }
            }
            if let Some(name) = snap.last_model.as_ref() {
                if let Some(i) = merged.iter().position(|m| m.name == *name) {
                    self.ui.set_sel_model_index(i as i32);
                }
            }
            *self.first_load_applied.lock().unwrap() = true;
        } else {
            // Subsequent refreshes: keep whatever the user (or the
            // initial apply) selected. If it's gone, clear the index.
            if let Some(w) = world {
                let i = self.ui.get_sel_agent_index();
                if i >= 0 {
                    let name = w.agents.get(i as usize).map(|a| a.name.clone());
                    match name {
                        Some(n) => {
                            if let Some(new_i) =
                                w.agents.iter().position(|a| a.name == n)
                            {
                                // only re-set if the index actually moved
                                // (otherwise this would be a no-op)
                                if new_i as i32 != i {
                                    self.ui.set_sel_agent_index(new_i as i32);
                                }
                            } else {
                                self.ui.set_sel_agent_index(-1);
                            }
                        }
                        None => {
                            // index out of range after a shorter list — clamp
                            if i >= w.agents.len() as i32 {
                                self.ui.set_sel_agent_index(-1);
                            }
                        }
                    }
                }
            }
            let i = self.ui.get_sel_model_index();
            if i >= 0 {
                let name = self
                    .ui
                    .get_models()
                    .row_data(i as usize)
                    .map(|m| m.name.to_string());
                if let Some(n) = name {
                    if let Some(new_i) = merged.iter().position(|m| m.name == n) {
                        if new_i as i32 != i {
                            self.ui.set_sel_model_index(new_i as i32);
                        }
                    } else {
                        self.ui.set_sel_model_index(-1);
                    }
                } else if i >= merged.len() as i32 {
                    self.ui.set_sel_model_index(-1);
                }
            }
        }

        // ---- simple props ----
        if prev.as_ref().map(|p| p.ollama_host.as_str()) != Some(snap.ollama_host.as_str()) {
            self.ui.set_ollama_host(snap.ollama_host.clone().into());
        }
        if prev.as_ref().map(|p| p.refreshing) != Some(snap.refreshing) {
            self.ui.set_refreshing(snap.refreshing);
        }
        if prev.as_ref().map(|p| p.status.message.as_str()) != Some(snap.status.message.as_str())
            || prev.as_ref().map(|p| p.status.kind) != Some(snap.status.kind)
        {
            self.ui.set_status(snap.status.message.clone().into());
            self.ui.set_status_kind(snap.status.kind);
        }
        if prev.as_ref().map(|p| p.settings_open) != Some(snap.settings_open) {
            self.ui.set_settings_open(snap.settings_open);
        }

        *self.last.lock().unwrap() = Some(snap.clone());
    }
}

// ───────────────────── ViewState impl ─────────────────────

struct SlintViewState {
    ui_weak: slint::Weak<AppWindow>,
}

impl ViewState for SlintViewState {
    fn ollama_host(&self) -> String {
        self.ui_weak
            .upgrade()
            .map(|ui| ui.get_ollama_host().to_string())
            .unwrap_or_default()
    }
    fn selected_agent_token(&self) -> Option<String> {
        let ui = self.ui_weak.upgrade()?;
        let i = ui.get_sel_agent_index();
        if i < 0 {
            return None;
        }
        ui.get_agents().row_data(i as usize).map(|a| a.name.to_string())
    }
    fn selected_model_name(&self) -> Option<String> {
        let ui = self.ui_weak.upgrade()?;
        let i = ui.get_sel_model_index();
        if i < 0 {
            return None;
        }
        ui.get_models().row_data(i as usize).map(|m| m.name.to_string())
    }
}

// ───────────────────── builders ─────────────────────

fn build_agent_items(w: &WorldSnapshot) -> Vec<AgentItem> {
    w.agents
        .iter()
        .enumerate()
        .map(|(i, a)| AgentItem {
            name: a.name.clone().into(),
            display: a.display.clone().into(),
            is_gui: a.is_gui,
            running: w.running.get(i).copied().unwrap_or(false),
            installed: w.installed.get(i).copied().unwrap_or(true),
            restorable: crate::ollama::restore_available(&a.name),
            initials: initials(&a.display).into(),
            color_index: color_index(&a.name),
        })
        .collect()
}

fn build_model_items(local: &[String], cloud: &[String]) -> Vec<ModelItem> {
    let mut items: Vec<ModelItem> = local
        .iter()
        .map(|n| ModelItem { name: n.as_str().into(), is_local: true })
        .collect();
    for n in cloud {
        if !local.iter().any(|l| l == n) {
            items.push(ModelItem { name: n.as_str().into(), is_local: false });
        }
    }
    items
}
