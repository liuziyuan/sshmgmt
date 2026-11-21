use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::sync::Mutex as TokioMutex;

use crate::model::{Msg, TunnelConfig, TunnelInfo, TunnelState};
use crate::manager::TunnelManager;
use crate::parser::parse_ssh_command;
use crate::probe::{self, PortStatus};
use crate::store;
use crate::transfer;
use crate::tunnel;

pub type AppManager = TokioMutex<TunnelManager>;

/// Wrap a low-level anyhow error at the command boundary. The detail is
/// English technical text (IO/keychain errors); the frontend renders the
/// commandFailed template around it.
fn command_failed(e: anyhow::Error) -> Msg {
    Msg::CommandFailed {
        detail: e.to_string(),
    }
}

// Environment names are case-normalized to uppercase so "qa" and "QA" can
// never become two separate environments. An empty/whitespace value means
// "not set".
fn normalize_env(env: Option<String>) -> Option<String> {
    env.map(|e| {
        let t = e.trim();
        if t.is_empty() { None } else { Some(t.to_uppercase()) }
    })
    .flatten()
}

// ─── Query ────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_tunnels(mgr: State<'_, AppManager>) -> Result<Vec<TunnelInfo>, Msg> {
    Ok(mgr.lock().await.list_tunnels())
}

#[tauri::command]
pub async fn parse_command(raw: String) -> Result<TunnelConfig, Msg> {
    parse_ssh_command(&raw, None)
}

/// Structured startup-migration warning, if any.
#[tauri::command]
pub async fn get_startup_warning() -> Option<Msg> {
    store::startup_warning()
}

// ─── CRUD ─────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn add_tunnel(
    raw_command: String,
    name: Option<String>,
    group: Option<String>,
    environment: Option<String>,
    mgr: State<'_, AppManager>,
) -> Result<TunnelConfig, Msg> {
    let mut config = parse_ssh_command(&raw_command, name)?;
    config.group = group.map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
    config.environment = normalize_env(environment);
    let mut m = mgr.lock().await;
    m.add_config(config.clone());
    store::save_tunnels(m.configs()).map_err(command_failed)?;
    Ok(config)
}

#[tauri::command]
pub async fn update_tunnel(
    config: TunnelConfig,
    mgr: State<'_, AppManager>,
) -> Result<(), Msg> {
    let mut m = mgr.lock().await;
    // Normalize user-supplied fields the same way add_tunnel does, so an edit
    // can't reintroduce a lowercase environment or an untrimmed group.
    let mut config = config;
    config.group = config.group.map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
    config.environment = normalize_env(config.environment);
    if !m.update_config(config) {
        return Err(Msg::TunnelNotFound);
    }
    store::save_tunnels(m.configs()).map_err(command_failed)
}

#[tauri::command]
pub async fn delete_tunnel(
    id: String,
    mgr: State<'_, AppManager>,
) -> Result<(), Msg> {
    let mut m = mgr.lock().await;
    m.stop_tunnel(&id);
    if !m.remove_config(&id) {
        return Err(Msg::TunnelNotFound);
    }
    store::save_tunnels(m.configs()).map_err(command_failed)
}

// ─── Connection control ───────────────────────────────────────────────────────

#[tauri::command]
pub async fn connect_tunnel(
    id: String,
    mgr: State<'_, AppManager>,
    app: tauri::AppHandle,
) -> Result<(), Msg> {
    // Fast path: if the app already manages this tunnel, just refresh / nudge it.
    let forwards = {
        let mut m = mgr.lock().await;
        if m.is_running(&id) {
            match m.current_state(&id) {
                // Already connected → re-emit so the UI shows green, nothing to do.
                TunnelState::Connected => m.reemit_state(&id, &app),
                // Stuck / failed → kick a reconnect.
                _ => {
                    m.reconnect_tunnel(&id, &app);
                }
            }
            return Ok(());
        }
        m.get_config(&id)
            .ok_or(Msg::TunnelNotFound)?
            .forwards
            .clone()
    };

    // Not managed by the app: probe the local ports before binding, so an already
    // established tunnel (possibly a manual `ssh -L` in a terminal) is handled
    // gracefully instead of looping forever on a failed bind.
    let status = probe::probe_forwards(&forwards).await;

    let mut m = mgr.lock().await;
    match status {
        // Ports free → start normally.
        PortStatus::Free => {
            m.start_tunnel(&id, &app);
        }
        // External tunnel is healthy → show green and monitor it.
        PortStatus::Working => {
            m.mark_external(&id, &app);
        }
        // External tunnel is squatting the port but broken → free it, then start.
        // Only local listener ports are killed: a Remote rule's port is on the
        // server side and may legitimately coincide with an unrelated local
        // process (B5).
        PortStatus::Broken => {
            for port in probe::local_listen_ports(&forwards) {
                probe::kill_port_listeners(port);
            }
            m.start_tunnel(&id, &app);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn disconnect_tunnel(
    id: String,
    mgr: State<'_, AppManager>,
) -> Result<(), Msg> {
    mgr.lock().await.stop_tunnel(&id);
    Ok(())
}

#[tauri::command]
pub async fn reconnect_tunnel(
    id: String,
    mgr: State<'_, AppManager>,
    app: tauri::AppHandle,
) -> Result<(), Msg> {
    mgr.lock().await.reconnect_tunnel(&id, &app);
    Ok(())
}

#[tauri::command]
pub async fn reconnect_all(
    mgr: State<'_, AppManager>,
    app: tauri::AppHandle,
) -> Result<(), Msg> {
    mgr.lock().await.reconnect_all(&app);
    Ok(())
}

// ─── Import / Export ──────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ImportSummary {
    pub imported: usize,
    pub skipped: usize,
    pub skipped_names: Vec<String>,
    /// Non-fatal keychain write failures (configs were still imported),
    /// as structured messages the frontend renders in its language.
    pub warnings: Vec<Msg>,
}

/// Write every tunnel config (plus keychain secrets, plaintext) to `path`.
/// The path comes from the frontend dialog plugin; the file never touches JS.
#[tauri::command]
pub async fn export_tunnels(
    path: String,
    mgr: State<'_, AppManager>,
) -> Result<usize, Msg> {
    let m = mgr.lock().await;
    let file = transfer::build_export(m.configs());
    let count = file.tunnels.len();
    transfer::write_export(std::path::Path::new(&path), &file)?;
    Ok(count)
}

/// Merge tunnels from an export file into the local list. Conflicting tunnels
/// (same id or name) are skipped entirely — including their secrets, so a
/// local keychain entry is never overwritten by an import. Keychain write
/// failures are collected into `warnings` instead of failing the import.
#[tauri::command]
pub async fn import_tunnels(
    path: String,
    mgr: State<'_, AppManager>,
) -> Result<ImportSummary, Msg> {
    let file = transfer::read_import(std::path::Path::new(&path))?;
    let mut m = mgr.lock().await;
    let (mut to_import, skipped) = transfer::split_new(m.configs(), &file.tunnels);

    let mut warnings = Vec::new();
    for t in &mut to_import {
        let c = &mut t.config;
        // Configs from older files have no hops; synthesize them before any
        // secret is written so keychain accounts match the runtime chain.
        c.ensure_hops();
        // Imported files may predate env normalization — fold them in here
        // too, so an old export can't reintroduce "qa"/"prod" entries.
        c.environment = normalize_env(c.environment.clone());
        c.group = c.group.clone().map(|g| g.trim().to_string()).filter(|g| !g.is_empty());

        let hops = c.resolved_hops();
        // Only write Some secrets — null means "not exported", never "delete".
        if t.secret.hop_passwords.is_empty() {
            // Legacy export file: `password` belongs to the first hop.
            if let (Some(pw), Some(first)) = (&t.secret.password, hops.first()) {
                if let Err(e) = store::set_password(&first.user, &first.host, first.port, pw) {
                    warnings.push(Msg::ImportKeychainFailed {
                        name: c.name.clone(),
                        detail: e.to_string(),
                    });
                }
            }
        } else {
            for (i, pw) in t.secret.hop_passwords.iter().enumerate() {
                let Some(pw) = pw else { continue };
                match hops.get(i) {
                    Some(hop) => {
                        if let Err(e) =
                            store::set_password(&hop.user, &hop.host, hop.port, pw)
                        {
                            warnings.push(Msg::ImportHopKeychainFailed {
                                name: c.name.clone(),
                                hop_index: i + 1,
                                hop_total: hops.len(),
                                detail: e.to_string(),
                            });
                        }
                    }
                    None => tracing::warn!(
                        "hop_passwords[{}] out of range for tunnel {} ({} hops)",
                        i,
                        c.name,
                        hops.len()
                    ),
                }
            }
        }
        if let (Some(u), Some(first)) = (&t.secret.target_user, hops.first()) {
            if let Err(e) = store::set_target_user(&first.host, first.port, u) {
                warnings.push(Msg::ImportTargetUserKeychainFailed {
                    name: c.name.clone(),
                    detail: e.to_string(),
                });
            }
        }
        m.add_config(c.clone());
    }
    if !to_import.is_empty() {
        store::save_tunnels(m.configs()).map_err(command_failed)?;
    }
    // Bring the group ordering along too when the export file carries it.
    if let Some(order) = &file.group_order {
        if !order.is_empty() {
            store::save_group_order(order).map_err(command_failed)?;
        }
    }
    Ok(ImportSummary {
        imported: to_import.len(),
        skipped: skipped.len(),
        skipped_names: skipped.iter().map(|t| t.config.name.clone()).collect(),
        warnings,
    })
}

// ─── Group ordering ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_group_order() -> Result<Vec<String>, Msg> {
    Ok(store::load_group_order())
}

#[tauri::command]
pub async fn set_group_order(order: Vec<String>) -> Result<(), Msg> {
    store::save_group_order(&order).map_err(command_failed)
}

#[tauri::command]
pub async fn get_collapsed_groups() -> Result<Vec<String>, Msg> {
    Ok(store::load_collapsed_groups())
}

#[tauri::command]
pub async fn set_collapsed_groups(groups: Vec<String>) -> Result<(), Msg> {
    store::save_collapsed_groups(&groups).map_err(command_failed)
}

// ─── Password ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn submit_password(
    id: String,
    password: String,
    save: bool,
    username: Option<String>,
    pubkey_path: Option<String>,
    mgr: State<'_, AppManager>,
) -> Result<(), Msg> {
    let m = mgr.lock().await;
    if !m.submit_password(&id, password, save, username, pubkey_path) {
        return Err(Msg::NoPendingPassword);
    }
    Ok(())
}

/// List the local public keys under ~/.ssh so the user can pick which one to
/// upload. Ordered ed25519 → ecdsa → rsa → others.
#[tauri::command]
pub async fn list_public_keys() -> Result<Vec<crate::model::PublicKeyInfo>, Msg> {
    Ok(tunnel::list_public_keys())
}

// ─── Keychain helpers ─────────────────────────────────────────────────────────

#[tauri::command]
pub async fn delete_saved_password(
    id: String,
    mgr: State<'_, AppManager>,
) -> Result<(), Msg> {
    let m = mgr.lock().await;
    if let Some(c) = m.get_config(&id) {
        // Delete every hop's saved password; report the first failure after
        // attempting them all (single-hop keeps the old error semantics).
        let hops = c.resolved_hops();
        let mut first_error: Option<String> = None;
        for hop in &hops {
            if let Err(e) = store::delete_password(&hop.user, &hop.host, hop.port) {
                if first_error.is_none() {
                    first_error = Some(e.to_string());
                }
            }
        }
        if let Some(e) = first_error {
            return Err(Msg::CommandFailed { detail: e });
        }
    }
    Ok(())
}

// ─── Language UI support: tray rebuild / window title (G4) ───────────────────

/// Labels for the tray menu and tooltip, supplied by the frontend in the
/// user's language (D3: the backend never knows about languages). Menu item
/// ids stay "show"/"reconnect"/"quit" — the event dispatch matches on ids.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayLabels {
    pub show: String,
    pub reconnect_all: String,
    pub quit: String,
    pub tooltip: String,
}

/// Replace the tray menu and tooltip in place (same tray icon, same event
/// handlers). Tauri's set_menu/set_tooltip dispatch to the main thread, so
/// no explicit run_on_main_thread is needed here.
#[tauri::command]
pub async fn rebuild_tray(labels: TrayLabels, app: tauri::AppHandle) -> Result<(), Msg> {
    let tray = app.tray_by_id("main").ok_or(Msg::TrayMissing)?;
    let menu =
        crate::build_tray_menu(&app, &labels).map_err(|e| Msg::TrayUpdateFailed {
            detail: e.to_string(),
        })?;
    tray.set_menu(Some(menu)).map_err(|e| Msg::TrayUpdateFailed {
        detail: e.to_string(),
    })?;
    tray.set_tooltip(Some(labels.tooltip.as_str())).map_err(|e| {
        Msg::TrayUpdateFailed {
            detail: e.to_string(),
        }
    })?;
    Ok(())
}

/// Set the main window title in the current language. The static title in
/// tauri.conf.json ("portunnel") is only the pre-frontend fallback.
#[tauri::command]
pub async fn set_window_title(title: String, app: tauri::AppHandle) -> Result<(), Msg> {
    use tauri::Manager;
    let win = app
        .get_webview_window("main")
        .ok_or(Msg::MainWindowMissing)?;
    win.set_title(&title).map_err(|e| Msg::SetTitleFailed {
        detail: e.to_string(),
    })
}
