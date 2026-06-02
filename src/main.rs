//! Composition root.
//!
//! Wires the three MVC layers and runs the Slint event loop.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod controller;
mod model;
mod ollama;
mod repository;
mod slint_generated;
mod view;

use controller::AppController;
use model::AppModel;
use repository::OllamaRepository;
use slint::{Timer, TimerMode};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use view::SlintAppView;

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
