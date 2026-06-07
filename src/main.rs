//! Composition root.
//!
//! Wires the three MVC layers and runs the Slint event loop.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod controller;
mod model;
mod ollama;
mod repository;
mod terminal;
mod slint_generated;
mod test_util;
mod view;

use controller::AppController;
use model::AppModel;
use repository::OllamaRepository;
use slint::{Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
slint::include_modules!();

use ollama::{
    installed_states, launch_agent, list_agents, list_cloud_models, list_local_models,
    pick_directory, restore_agent, restore_available, running_states, test_connection, Agent,
};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use terminal::Terminal;
use view::SlintAppView;

/// Map a cloud model name to its provider slug (matches ProviderLogo in app.slint).
fn provider_for_model(name: &str) -> &'static str {
    let n = name.split(':').next().unwrap_or(name);
    // OpenAI skipped "o2"; add it here if they ever release one.
    if n.starts_with("gpt-") || n.starts_with("o1") || n.starts_with("o2") || n.starts_with("o3") || n.starts_with("o4") {
        "openai"
    } else if n.starts_with("gemini") {
        "gemini"
    } else if n.starts_with("gemma") {
        "gemma"
    } else if n.starts_with("mistral") || n.starts_with("ministral") || n.starts_with("devstral") {
        "mistral"
    } else if n.starts_with("deepseek") {
        "deepseek"
    } else if n.starts_with("qwen") {
        "qwen"
    } else if n.starts_with("glm") {
        "zhipu"
    } else if n.starts_with("kimi") || n.starts_with("moonshot") {
        "moonshot"
    } else if n.starts_with("nemotron") {
        "nvidia"
    } else if n.starts_with("minimax") {
        "minimax"
    } else {
        "ollama"
    }
}

/// Map an agent launch token to its logo key (matches AgentBadge in app.slint).
/// Returns "" for unknown agents so the initials badge is used as fallback.
fn logo_for_agent(name: &str) -> &'static str {
    match name {
        "claude" | "claude-code"                        => "claude-code",
        "codex-app" | "codex-desktop" | "codex-gui"
        | "codex"                                       => "codex",
        "opencode"                                      => "opencode",
        "hermes" | "hermes-agent"                       => "hermes",
        "openclaw"                                      => "openclaw",
        "cursor"                                        => "cursor",
        "windsurf"                                      => "windsurf",
        "copilot" | "github-copilot"                    => "copilot",
        "cline"                                         => "cline",
        "amp"                                           => "amp",
        "goose"                                         => "goose",
        "vscode" | "code"                               => "vscode",
        _                                               => "",
    }
}

/// Up to two uppercase initials from the agent label.
fn initials(display: &str) -> String {
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

/// Stable color slot (0..PALETTE_LEN) derived from the agent token.
const PALETTE_LEN: i32 = 13;
fn color_index(token: &str) -> i32 {
    let sum: u32 = token.bytes().map(|b| b as u32).sum();
    (sum % PALETTE_LEN as u32) as i32
}

fn make_agent_items(agents: &[Agent], running: &[bool], installed: &[bool]) -> Vec<AgentItem> {
    agents
        .iter()
        .enumerate()
        .map(|(i, a)| AgentItem {
            name: a.name.clone().into(),
            display: a.display.clone().into(),
            is_gui: a.is_gui,
            running: running.get(i).copied().unwrap_or(false),
            installed: installed.get(i).copied().unwrap_or(true),
            restorable: restore_available(&a.name),
            initials: initials(&a.display).into(),
            color_index: color_index(&a.name),
            logo: logo_for_agent(&a.name).into(),
        })
        .collect()
}

thread_local! {
    /// The live view handle, set just before `view.run()` and read by
    /// the Slint timer. Slint timers run on the UI thread, so a
    /// thread_local is the right scope.
    static VIEW: RefCell<Option<Rc<SlintAppView>>> = const { RefCell::new(None) };
/// Merge local model names (first, teal) followed by cloud model names.
/// Deduplicates: if a local model name also appears in cloud, the local entry wins.
fn make_model_items(local: &[String], cloud: &[String]) -> Vec<ModelItem> {
    let mut items: Vec<ModelItem> = local
        .iter()
        .map(|n| ModelItem {
            name: n.as_str().into(),
            is_local: true,
            provider: "ollama".into(),
        })
        .collect();
    for n in cloud {
        if !local.iter().any(|l| l == n) {
            items.push(ModelItem {
                name: n.as_str().into(),
                is_local: false,
                provider: provider_for_model(n).into(),
            });
        }
    }
    items
}

fn cloud_names_from_ui(ui: &AppWindow) -> Vec<String> {
    (0..ui.get_models().row_count())
        .filter_map(|i| {
            let m = ui.get_models().row_data(i)?;
            if !m.is_local { Some(m.name.to_string()) } else { None }
        })
        .collect()
}

fn selected_model_name(ui: &AppWindow) -> Option<String> {
    let idx = ui.get_sel_model_index();
    if idx >= 0 {
        ui.get_models().row_data(idx as usize).map(|m| m.name.to_string())
    } else {
        None
    }
}

/// Replace the UI model list, re-resolving the selected index by name.
/// Indices are invalidated whenever the list is rebuilt (local models sit at
/// the front, so dropping them shifts cloud entries); resolving by name keeps
/// the highlight on the same model, or clears it (-1) if that model is gone.
fn set_models_preserving_selection(ui: &AppWindow, items: Vec<ModelItem>) {
    let prev = selected_model_name(ui);
    let new_idx = prev
        .as_deref()
        .and_then(|n| items.iter().position(|m| m.name == n))
        .map(|i| i as i32)
        .unwrap_or(-1);
    ui.set_models(ModelRc::new(VecModel::from(items)));
    ui.set_sel_model_index(new_idx);
}

/// Fetch agents, their running + installed state, and the cloud model list.
async fn fetch_all() -> anyhow::Result<(Vec<Agent>, Vec<bool>, Vec<bool>, Vec<String>)> {
    let agents = list_agents().await?;
    let models = list_cloud_models()
        .await?
        .into_iter()
        .map(|m| m.name)
        .collect::<Vec<_>>();
    let agents_for_scan = agents.clone();
    let (running, installed) = tokio::task::spawn_blocking(move || {
        let r = running_states(&agents_for_scan);
        let i = installed_states(&agents_for_scan);
        (r, i)
    })
    .await?;
    Ok((agents, running, installed, models))
}

fn main() -> anyhow::Result<()> {
    // 1. tokio runtime
    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();
    let handle = rt.handle().clone();

    let ui = AppWindow::new()?;
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    let agents_store: Arc<Mutex<Vec<Agent>>> = Arc::new(Mutex::new(Vec::new()));
    let prefs = Arc::new(config::load());
    let prefs_applied = Arc::new(AtomicBool::new(false));

    // shared local models (fetched after a successful connection test)
    let local_models: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    // monotonic counter — incremented on each Test click so stale responses are discarded
    let test_gen: Arc<std::sync::atomic::AtomicU64> =
        Arc::new(std::sync::atomic::AtomicU64::new(0));

    // restore ollama_host + working_dir from prefs
    ui.set_ollama_host(prefs.ollama_host.clone().into());
    ui.set_working_dir(prefs.working_dir.clone().into());

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
    // ---- launch / relaunch ----
    {
        let store = agents_store.clone();
        let ui_weak = ui.as_weak();
        ui.on_launch(move |idx, model| {
            let agent = store.lock().unwrap().get(idx as usize).cloned();
            let model = model.to_string();
            let (host, working_dir) = ui_weak
                .upgrade()
                .map(|ui| {
                    (
                        ui.get_ollama_host().to_string(),
                        ui.get_working_dir().to_string(),
                    )
                })
                .unwrap_or_default();
            if let Some(a) = &agent {
                config::save(&config::Prefs {
                    agent: a.name.clone(),
                    model: model.clone(),
                    ollama_host: host.clone(),
                    working_dir: working_dir.clone(),
                });
            }
            let ui_weak = ui_weak.clone();
            std::thread::spawn(move || {
                let host_opt = if host.is_empty() { None } else { Some(host.as_str()) };
                let dir_opt = if working_dir.is_empty() { None } else { Some(working_dir.as_str()) };
                let (msg, kind) = match agent {
                    Some(a) => match launch_agent(&a, &model, host_opt, dir_opt) {
                        Ok(()) => (format!("✓ {} launched · {}", a.display, model), 1),
                        Err(e) => (format!("✗ {e}"), 2),
                    },
                    None => ("✗ Invalid agent".to_string(), 2),
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_status(msg.into());
                        ui.set_status_kind(kind);
                    }
                });
            });
        });
    }

    // 6. Wire the Slint callbacks.
    view.attach_controller(Arc::downgrade(&controller_dyn));

    // 6b. Populate the terminal dropdown with the platform-specific
    //     terminals that are actually installed on this machine.
    {
        let items: Vec<slint_generated::TerminalItem> = terminal::available()
            .into_iter()
            .map(|t| slint_generated::TerminalItem {
                key: SharedString::from(t.key()),
                label: SharedString::from(t.label()),
            })
            .collect();
        let key = config::load().terminal;
        let idx = terminal::index_of(&key) as i32;
        if let Some(ui) = view.ui_weak().upgrade() {
            ui.set_terminals(ModelRc::new(VecModel::from(items)));
            ui.set_sel_terminal_index(idx);
        }
        // The persisted key may not match the default "" that the
        // Controller has never seen — coerce it through Terminal::from_key
        // so a saved "iterm2" is recognised even if the user's iTerm.app
        // is currently missing (we still record the choice for next run).
        let _ = Terminal::from_key(&key);
    }
    // ---- directory picker ----
    {
        let ui_weak = ui.as_weak();
        ui.on_pick_directory(move || {
            let ui_weak = ui_weak.clone();
            // seed the dialog at the current value so re-browsing starts there
            let start = ui_weak
                .upgrade()
                .map(|ui| ui.get_working_dir().to_string())
                .unwrap_or_default();
            // the native dialog blocks until dismissed — run it off the UI thread
            std::thread::spawn(move || {
                let start_opt = if start.is_empty() { None } else { Some(start.as_str()) };
                if let Some(dir) = pick_directory(start_opt) {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_weak.upgrade() {
                            ui.set_working_dir(dir.into());
                        }
                    });
                }
            });
        });
    }

    // ---- shared refresh routine ----
    // last_lists: (agent_signatures, cloud_model_names) — only push UI updates on change
    let do_refresh: Arc<dyn Fn() + Send + Sync> = {
        let store = agents_store.clone();
        let ui_weak = ui.as_weak();
        let prefs = prefs.clone();
        let prefs_applied = prefs_applied.clone();
        let local_models = local_models.clone();
        let last_lists: Arc<Mutex<(Vec<String>, Vec<String>)>> =
            Arc::new(Mutex::new((Vec::new(), Vec::new())));
        Arc::new(move || {
            let store = store.clone();
            let ui_weak = ui_weak.clone();
            let prefs = prefs.clone();
            let prefs_applied = prefs_applied.clone();
            let local_models = local_models.clone();
            let last_lists = last_lists.clone();
            tokio::spawn(async move {
                {
                    let uw = ui_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = uw.upgrade() {
                            ui.set_refreshing(true);
                        }
                    });
                }
                match fetch_all().await {
                    Ok((agents, running, installed, cloud_names)) => {
                        // sort: agents with logos first, no-logo agents last (stable)
                        let mut order: Vec<usize> = (0..agents.len()).collect();
                        order.sort_by_key(|&i| if logo_for_agent(&agents[i].name).is_empty() { 1i32 } else { 0i32 });
                        let agents: Vec<Agent> = order.iter().map(|&i| agents[i].clone()).collect();
                        let running: Vec<bool> = order.iter().map(|&i| running[i]).collect();
                        let installed: Vec<bool> = order.iter().map(|&i| installed[i]).collect();
                        *store.lock().unwrap() = agents.clone();
                        let items = make_agent_items(&agents, &running, &installed);

                        // use a content-hash so we only repaint on real changes
                        let agent_sig: Vec<String> = items
                            .iter()
                            .map(|it| {
                                format!(
                                    "{}|{}|{}|{}",
                                    it.name, it.running, it.restorable, it.installed
                                )
                            })
                            .collect();

                        let (agents_changed, models_changed) = {
                            let mut g = last_lists.lock().unwrap();
                            let ac = g.0 != agent_sig;
                            let mc = g.1 != cloud_names;
                            if ac { g.0 = agent_sig; }
                            if mc { g.1 = cloud_names.clone(); }
                            (ac, mc)
                        };

                        let local_snap = local_models.lock().unwrap().clone();

                        // restore last-used selection once, after the lists are known
                        let first_apply = !prefs_applied.swap(true, Ordering::SeqCst);
                        let restore_sel = if first_apply {
                            let ai = agents
                                .iter()
                                .position(|a| a.name == prefs.agent)
                                .map(|i| i as i32);
                            let merged = make_model_items(&local_snap, &cloud_names);
                            let mi = merged
                                .iter()
                                .position(|m| m.name == prefs.model.as_str())
                                .map(|i| i as i32);
                            Some((ai, mi))
                        } else {
                            None
                        };

                        let agent_names: Vec<SharedString> = agents
                            .iter()
                            .map(|a| a.display.as_str().into())
                            .collect();
                        let model_items = make_model_items(&local_snap, &cloud_names);

                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                if agents_changed {
                                    ui.set_agents(ModelRc::new(VecModel::from(items)));
                                    ui.set_agent_names(ModelRc::new(VecModel::from(agent_names)));
                                }
                                if models_changed {
                                    ui.set_models(ModelRc::new(VecModel::from(model_items)));
                                }
                                if let Some((ai, mi)) = restore_sel {
                                    if let Some(ai) = ai {
                                        ui.set_sel_agent_index(ai);
                                    }
                                    if let Some(mi) = mi {
                                        ui.set_sel_model_index(mi);
                                    }
                                }
                                ui.set_refreshing(false);
                            }
                        });
                    }
                    Err(e) => {
                        let msg = format!("✗ Refresh failed: {e}");
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_status(msg.into());
                                ui.set_status_kind(2);
                                ui.set_refreshing(false);
                            }
                        });
                    }
                }
            });
        })
    };

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
