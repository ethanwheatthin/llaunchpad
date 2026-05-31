#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod ollama;

slint::include_modules!();

use ollama::{list_agents, list_cloud_models, list_local_models, test_connection, launch_agent, running_states, Agent};
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

fn make_agent_items(agents: &[Agent], running: &[bool]) -> Vec<AgentItem> {
    agents
        .iter()
        .enumerate()
        .map(|(i, a)| AgentItem {
            name: a.name.clone().into(),
            display: a.display.clone().into(),
            is_gui: a.is_gui,
            running: running.get(i).copied().unwrap_or(false),
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

/// Fetch agents, their running state, and the cloud model list.
async fn fetch_all() -> anyhow::Result<(Vec<Agent>, Vec<bool>, Vec<String>)> {
    let agents = list_agents().await?;
    let models = list_cloud_models()
        .await?
        .into_iter()
        .map(|m| m.name)
        .collect::<Vec<_>>();
    let agents_for_scan = agents.clone();
    let running = tokio::task::spawn_blocking(move || running_states(&agents_for_scan)).await?;
    Ok((agents, running, models))
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

    // restore ollama_host from prefs
    ui.set_ollama_host(prefs.ollama_host.clone().into());

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
        ui.on_test_connection(move |url| {
            let url = url.to_string();
            let ui_weak = ui_weak.clone();
            let local_models = local_models.clone();
            tokio::spawn(async move {
                match test_connection(&url).await {
                    Ok(info) => {
                        // fetch local models from the confirmed-live server
                        let fetched = list_local_models(&url).await
                            .unwrap_or_default()
                            .into_iter()
                            .map(|m| m.name)
                            .collect::<Vec<_>>();
                        let count = fetched.len();
                        *local_models.lock().unwrap() = fetched.clone();

                        let msg = if count > 0 {
                            format!("✓ {info} · {count} local model{}", if count == 1 { "" } else { "s" })
                        } else {
                            format!("✓ {info} · no local models pulled")
                        };

                        // push fresh model items to the UI (local first)
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                // read the current cloud model list from existing items
                                let cloud: Vec<String> = (0..ui.get_models().row_count())
                                    .filter_map(|i| {
                                        let m = ui.get_models().row_data(i)?;
                                        if !m.is_local { Some(m.name.to_string()) } else { None }
                                    })
                                    .collect();
                                let items = make_model_items(&fetched, &cloud);
                                ui.set_models(ModelRc::new(VecModel::from(items)));
                                ui.set_status(msg.into());
                                ui.set_status_kind(1);
                            }
                        });
                    }
                    Err(e) => {
                        // clear local models on connection failure
                        *local_models.lock().unwrap() = Vec::new();
                        let msg = format!("✗ {e}");
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
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
            // read the current host value from the UI
            let host = ui_weak
                .upgrade()
                .map(|ui| ui.get_ollama_host().to_string())
                .unwrap_or_default();
            // persist last-used selection including host
            if let Some(a) = &agent {
                config::save(&config::Prefs {
                    agent: a.name.clone(),
                    model: model.clone(),
                    ollama_host: host.clone(),
                });
            }
            let ui_weak = ui_weak.clone();
            // launch is blocking (process spawn + quit delay); use a thread
            std::thread::spawn(move || {
                let host_opt = if host.is_empty() { None } else { Some(host.as_str()) };
                let (msg, kind) = match agent {
                    Some(a) => match launch_agent(&a, &model, host_opt) {
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

    // ---- shared refresh routine ----
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
                    Ok((agents, running, cloud_names)) => {
                        *store.lock().unwrap() = agents.clone();
                        let items = make_agent_items(&agents, &running);
                        let agent_names_str: Vec<String> =
                            agents.iter().map(|a| a.display.clone()).collect();

                        // only push the combo lists when they actually change to avoid
                        // resetting the user's selection
                        let lists_changed = {
                            let mut g = last_lists.lock().unwrap();
                            let changed = g.0 != agent_names_str || g.1 != cloud_names;
                            if changed {
                                *g = (agent_names_str.clone(), cloud_names.clone());
                            }
                            changed
                        };

                        // snapshot local models while we hold the lock
                        let local_snap = local_models.lock().unwrap().clone();

                        // restore last-used selection once, after the lists are known
                        let first_apply = !prefs_applied.swap(true, Ordering::SeqCst);
                        let restore = if first_apply {
                            let ai = agents.iter().position(|a| a.name == prefs.agent).map(|i| i as i32);
                            // find the saved model name in the merged list
                            let merged = make_model_items(&local_snap, &cloud_names);
                            let mi = merged.iter().position(|m| m.name == prefs.model.as_str()).map(|i| i as i32);
                            Some((ai, mi))
                        } else {
                            None
                        };

                        let agent_names: Vec<SharedString> =
                            agent_names_str.into_iter().map(Into::into).collect();
                        let model_items = make_model_items(&local_snap, &cloud_names);

                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                // running state refreshes every cycle (cheap, no combo reset)
                                ui.set_agents(ModelRc::new(VecModel::from(items)));
                                if lists_changed {
                                    ui.set_agent_names(ModelRc::new(VecModel::from(agent_names)));
                                    ui.set_models(ModelRc::new(VecModel::from(model_items)));
                                }
                                if let Some((ai, mi)) = restore {
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
