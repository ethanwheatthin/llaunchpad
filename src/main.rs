//! Composition root.
//!
//! Wires the three MVC layers (Model, View, Controller) and runs the
//! Slint event loop. The heavy lifting lives in:
//!   - `crate::model`     — canonical state + tokio workers (poller, mirror)
//!   - `crate::view`      — Slint window, ViewCommand drain, UI builders
//!   - `crate::controller` — intent handlers (launch, restore, test, settings)
//!   - `crate::terminal`  — per-OS terminal selection (used at launch time)
//!
//! This file's job is to instantiate those pieces, connect them, and
//! `view.run()`. Anything that grows beyond that should move into one
//! of the layers.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod controller;
mod model;
mod ollama;
mod repository;
mod slint_generated;
mod terminal;
mod test_util;
mod view;

use crate::ollama::logos::provider_for_model;
use crate::ollama::Agent;
use crate::slint_generated::{AgentItem, ModelItem, TerminalItem};
use crate::terminal::Terminal;
use crate::view::SlintAppView;
use controller::AppController;
use model::AppModel;
use repository::OllamaRepository;
use slint::{Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

thread_local! {
    /// The live view handle, set just before `view.run()` and read by
    /// the Slint timer. Slint timers run on the UI thread, so a
    /// thread_local is the right scope.
    static VIEW: RefCell<Option<Rc<SlintAppView>>> = const { RefCell::new(None) };
}

fn main() -> anyhow::Result<()> {
    // 1. tokio runtime
    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();
    let handle = rt.handle().clone();

    // 2. Repository (could be swapped for a fake in tests).
    let repo: Arc<dyn repository::Repository> = Arc::new(OllamaRepository);

    // 3. Model — owns the canonical state.
    let prefs = config::load();
    let model = AppModel::new(repo, prefs);

    // 4. View — owns the Slint window.
    let view: Rc<SlintAppView> = SlintAppView::new();
    let sink = view.sink();
    let view_state = view.view_state();

    // 5. Controller.
    let controller = AppController::new(model, sink, view_state);
    controller.install_weak();
    let controller_dyn: Arc<dyn view::Controller> = controller.clone();

    // 6. Wire the Slint callbacks.
    view.attach_controller(Arc::downgrade(&controller_dyn));

    // 6a. Restore the persisted agent/model/host/working_dir/terminal
    //     *before* the poller fires so the first snapshot applies the
    //     user's last-used values.
    {
        let ui_weak = view.ui_weak();
        let key = config::load().terminal;
        let idx = terminal::index_of(&key) as i32;
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_terminals(make_terminal_items());
            ui.set_sel_terminal_index(idx);
        }
    }

    // 6b. Pre-populate the static (non-async) pieces: agent / model
    //     lists are filled by the controller's mirror loop once the
    //     first snapshot lands, but the terminal dropdown is a
    //     one-shot list of OS-candidates.
    fn make_terminal_items() -> ModelRc<TerminalItem> {
        let items: Vec<TerminalItem> = terminal::available()
            .into_iter()
            .map(|t| TerminalItem {
                key: SharedString::from(t.key()),
                label: SharedString::from(t.label()),
            })
            .collect();
        ModelRc::new(VecModel::from(items))
    }

    // 7. Poller + mirror loop.
    controller.start(&handle);

    // 8. Slint timer drains the ViewSink every 16ms (~60Hz). The
    //    closure runs on the UI thread, so the thread_local is in scope.
    VIEW.with(|v| *v.borrow_mut() = Some(view.clone()));
    {
        let timer = Timer::default();
        timer.start(TimerMode::Repeated, Duration::from_millis(16), || {
            VIEW.with(|v| {
                if let Some(view) = v.borrow().as_ref() {
                    view.tick();
                }
            });
        });
        std::mem::forget(timer);
    }

    // 9. Run the UI event loop.
    view.run()?;
    Ok(())
}
