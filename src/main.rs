#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod ollama;

slint::include_modules!();

use ollama::{
    installed_states, launch_agent, list_agents, list_cloud_models, list_local_models,
    restore_agent, restore_available, running_states, test_connection, Agent,
};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
        })
        .collect()
}

/// Merge local model names (first, teal) followed by cloud model names.
/// Deduplicates: if a local model name also appears in cloud, the local entry wins.
fn make_model_items(local: &[String], cloud: &[String]) -> Vec<ModelItem> {
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
    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();

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

    // ---- dismiss banner ----
    {
        let ui_weak = ui.as_weak();
        ui.on_dismiss(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_status("".into());
                ui.set_status_kind(0);
            }
        });
    }

    // ---- test connection ----
    {
        let ui_weak = ui.as_weak();
        let local_models = local_models.clone();
        let test_gen = test_gen.clone();
        ui.on_test_connection(move |url| {
            let url = url.to_string();
            // persist the host immediately so it survives exit even without a launch
            let mut saved = config::load();
            saved.ollama_host = url.clone();
            config::save(&saved);

            let gen = test_gen.fetch_add(1, Ordering::SeqCst) + 1;
            let ui_weak = ui_weak.clone();
            let local_models = local_models.clone();
            let test_gen = test_gen.clone();
            tokio::spawn(async move {
                match test_connection(&url).await {
                    Ok(info) => {
                        match list_local_models(&url).await {
                            Ok(local_list) => {
                                let fetched: Vec<String> =
                                    local_list.into_iter().map(|m| m.name).collect();
                                let count = fetched.len();
                                *local_models.lock().unwrap() = fetched.clone();
                                let msg = if count > 0 {
                                    format!(
                                        "✓ {info} · {count} local model{}",
                                        if count == 1 { "" } else { "s" }
                                    )
                                } else {
                                    format!("✓ {info} · no local models")
                                };
                                let _ = slint::invoke_from_event_loop(move || {
                                    if test_gen.load(Ordering::SeqCst) != gen { return; }
                                    if let Some(ui) = ui_weak.upgrade() {
                                        let cloud = cloud_names_from_ui(&ui);
                                        let items = make_model_items(&fetched, &cloud);
                                        set_models_preserving_selection(&ui, items);
                                        ui.set_status(msg.into());
                                        ui.set_status_kind(1);
                                    }
                                });
                            }
                            Err(e) => {
                                *local_models.lock().unwrap() = Vec::new();
                                let msg = format!("✓ {info} · model list unavailable: {e}");
                                let _ = slint::invoke_from_event_loop(move || {
                                    if test_gen.load(Ordering::SeqCst) != gen { return; }
                                    if let Some(ui) = ui_weak.upgrade() {
                                        let cloud = cloud_names_from_ui(&ui);
                                        set_models_preserving_selection(
                                            &ui,
                                            make_model_items(&[], &cloud),
                                        );
                                        ui.set_status(msg.into());
                                        ui.set_status_kind(1);
                                    }
                                });
                            }
                        }
                    }
                    Err(e) => {
                        *local_models.lock().unwrap() = Vec::new();
                        let msg = format!("✗ {e}");
                        let _ = slint::invoke_from_event_loop(move || {
                            if test_gen.load(Ordering::SeqCst) != gen { return; }
                            if let Some(ui) = ui_weak.upgrade() {
                                let cloud = cloud_names_from_ui(&ui);
                                set_models_preserving_selection(
                                    &ui,
                                    make_model_items(&[], &cloud),
                                );
                                ui.set_status(msg.into());
                                ui.set_status_kind(2);
                            }
                        });
                    }
                }
            });
        });
    }

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

    // ---- restore ----
    {
        let store = agents_store.clone();
        let ui_weak = ui.as_weak();
        ui.on_restore(move |idx| {
            let agent = store.lock().unwrap().get(idx as usize).cloned();
            let ui_weak = ui_weak.clone();
            std::thread::spawn(move || {
                let (msg, kind) = match agent {
                    Some(a) => match restore_agent(&a.name) {
                        Ok(()) => (format!("✓ {} restored to its original profile", a.display), 1),
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

    // manual refresh button
    {
        let do_refresh = do_refresh.clone();
        ui.on_refresh(move || do_refresh());
    }

    // background poller every 5s (keeps agents + models fresh)
    {
        let do_refresh = do_refresh.clone();
        rt.spawn(async move {
            loop {
                do_refresh();
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    ui.run()?;
    Ok(())
}
