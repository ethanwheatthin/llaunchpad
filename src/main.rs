#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod ollama;

slint::include_modules!();

use ollama::{list_agents, list_cloud_models, launch_agent, running_states, Agent};
use slint::{ModelRc, SharedString, VecModel};
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

    // ---- launch / relaunch ----
    {
        let store = agents_store.clone();
        let ui_weak = ui.as_weak();
        ui.on_launch(move |idx, model| {
            let agent = store.lock().unwrap().get(idx as usize).cloned();
            let model = model.to_string();
            // persist last-used selection
            if let Some(a) = &agent {
                config::save(&config::Prefs {
                    agent: a.name.clone(),
                    model: model.clone(),
                });
            }
            let ui_weak = ui_weak.clone();
            // launch is blocking (process spawn + quit delay); use a thread
            std::thread::spawn(move || {
                let (msg, kind) = match agent {
                    Some(a) => match launch_agent(&a, &model) {
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
        let last_lists: Arc<Mutex<(Vec<String>, Vec<String>)>> =
            Arc::new(Mutex::new((Vec::new(), Vec::new())));
        Arc::new(move || {
            let store = store.clone();
            let ui_weak = ui_weak.clone();
            let prefs = prefs.clone();
            let prefs_applied = prefs_applied.clone();
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
                    Ok((agents, running, models)) => {
                        *store.lock().unwrap() = agents.clone();
                        let items = make_agent_items(&agents, &running);
                        let names_str: Vec<String> =
                            agents.iter().map(|a| a.display.clone()).collect();

                        // only push the combo lists when they actually change,
                        // otherwise replacing the model resets the user's selection
                        let lists_changed = {
                            let mut g = last_lists.lock().unwrap();
                            let changed = g.0 != names_str || g.1 != models;
                            if changed {
                                *g = (names_str.clone(), models.clone());
                            }
                            changed
                        };

                        // restore last-used selection once, after the lists are known
                        let first_apply = !prefs_applied.swap(true, Ordering::SeqCst);
                        let restore = if first_apply {
                            let ai = agents.iter().position(|a| a.name == prefs.agent).map(|i| i as i32);
                            let mi = models.iter().position(|m| *m == prefs.model).map(|i| i as i32);
                            Some((ai, mi))
                        } else {
                            None
                        };

                        let names: Vec<SharedString> =
                            names_str.into_iter().map(Into::into).collect();
                        let model_items: Vec<SharedString> =
                            models.into_iter().map(Into::into).collect();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                // running state refreshes every cycle (cheap, no combo reset)
                                ui.set_agents(ModelRc::new(VecModel::from(items)));
                                if lists_changed {
                                    ui.set_agent_names(ModelRc::new(VecModel::from(names)));
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
