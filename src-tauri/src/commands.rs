use serde::Serialize;
use tauri::State;
use tokio::sync::Mutex as TokioMutex;

use crate::model::{TunnelConfig, TunnelInfo, TunnelState};
use crate::manager::TunnelManager;
use crate::parser::parse_ssh_command;
use crate::probe::{self, PortStatus};
use crate::store;
use crate::transfer;
use crate::tunnel;

pub type AppManager = TokioMutex<TunnelManager>;

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
pub async fn list_tunnels(mgr: State<'_, AppManager>) -> Result<Vec<TunnelInfo>, String> {
    Ok(mgr.lock().await.list_tunnels())
}

#[tauri::command]
pub async fn parse_command(raw: String) -> Result<TunnelConfig, String> {
    parse_ssh_command(&raw, None).map_err(|e| e.to_string())
}

// ─── CRUD ─────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn add_tunnel(
    raw_command: String,
    name: Option<String>,
    group: Option<String>,
    environment: Option<String>,
    mgr: State<'_, AppManager>,
) -> Result<TunnelConfig, String> {
    let mut config = parse_ssh_command(&raw_command, name).map_err(|e| e.to_string())?;
    config.group = group.map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
    config.environment = normalize_env(environment);
    let mut m = mgr.lock().await;
    m.add_config(config.clone());
    store::save_tunnels(m.configs()).map_err(|e| e.to_string())?;
    Ok(config)
}

#[tauri::command]
pub async fn update_tunnel(
    config: TunnelConfig,
    mgr: State<'_, AppManager>,
) -> Result<(), String> {
    let mut m = mgr.lock().await;
    // Normalize user-supplied fields the same way add_tunnel does, so an edit
    // can't reintroduce a lowercase environment or an untrimmed group.
    let mut config = config;
    config.group = config.group.map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
    config.environment = normalize_env(config.environment);
    if !m.update_config(config) {
        return Err("Tunnel not found".into());
    }
    store::save_tunnels(m.configs()).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_tunnel(
    id: String,
    mgr: State<'_, AppManager>,
) -> Result<(), String> {
    let mut m = mgr.lock().await;
    m.stop_tunnel(&id);
    if !m.remove_config(&id) {
        return Err("Tunnel not found".into());
    }
    store::save_tunnels(m.configs()).map_err(|e| e.to_string())
}

// ─── Connection control ───────────────────────────────────────────────────────

#[tauri::command]
pub async fn connect_tunnel(
    id: String,
    mgr: State<'_, AppManager>,
    app: tauri::AppHandle,
) -> Result<(), String> {
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
            .ok_or_else(|| "Tunnel not found".to_string())?
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
        PortStatus::Broken => {
            for f in &forwards {
                probe::kill_port_listeners(f.local_port);
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
) -> Result<(), String> {
    mgr.lock().await.stop_tunnel(&id);
    Ok(())
}

#[tauri::command]
pub async fn reconnect_tunnel(
    id: String,
    mgr: State<'_, AppManager>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    mgr.lock().await.reconnect_tunnel(&id, &app);
    Ok(())
}

#[tauri::command]
pub async fn reconnect_all(
    mgr: State<'_, AppManager>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    mgr.lock().await.reconnect_all(&app);
    Ok(())
}

// ─── Import / Export ──────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ImportSummary {
    pub imported: usize,
    pub skipped: usize,
    pub skipped_names: Vec<String>,
    /// Non-fatal keychain write failures (configs were still imported).
    pub warnings: Vec<String>,
}

/// Write every tunnel config (plus keychain secrets, plaintext) to `path`.
/// The path comes from the frontend dialog plugin; the file never touches JS.
#[tauri::command]
pub async fn export_tunnels(
    path: String,
    mgr: State<'_, AppManager>,
) -> Result<usize, String> {
    let m = mgr.lock().await;
    let file = transfer::build_export(m.configs());
    let count = file.tunnels.len();
    transfer::write_export(std::path::Path::new(&path), &file).map_err(|e| e.to_string())?;
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
) -> Result<ImportSummary, String> {
    let file = transfer::read_import(std::path::Path::new(&path)).map_err(|e| e.to_string())?;
    let mut m = mgr.lock().await;
    let (mut to_import, skipped) = transfer::split_new(m.configs(), &file.tunnels);

    let mut warnings = Vec::new();
    for t in &mut to_import {
        let c = &mut t.config;
        // Imported files may predate env normalization — fold them in here
        // too, so an old export can't reintroduce "qa"/"prod" entries.
        c.environment = normalize_env(c.environment.clone());
        c.group = c.group.clone().map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
        // Only write Some secrets — null means "not exported", never "delete".
        if let Some(pw) = &t.secret.password {
            if let Err(e) = store::set_password(&c.jump_user, &c.jump_host, c.jump_port, pw) {
                warnings.push(format!("「{}」密码写入钥匙串失败: {}", c.name, e));
            }
        }
        if let Some(u) = &t.secret.target_user {
            if let Err(e) = store::set_target_user(&c.jump_host, c.jump_port, u) {
                warnings.push(format!("「{}」用户名写入钥匙串失败: {}", c.name, e));
            }
        }
        m.add_config(c.clone());
    }
    if !to_import.is_empty() {
        store::save_tunnels(m.configs()).map_err(|e| e.to_string())?;
    }
    // Bring the group ordering along too when the export file carries it.
    if let Some(order) = &file.group_order {
        if !order.is_empty() {
            store::save_group_order(order).map_err(|e| e.to_string())?;
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
pub async fn get_group_order() -> Result<Vec<String>, String> {
    Ok(store::load_group_order())
}

#[tauri::command]
pub async fn set_group_order(order: Vec<String>) -> Result<(), String> {
    store::save_group_order(&order).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_collapsed_groups() -> Result<Vec<String>, String> {
    Ok(store::load_collapsed_groups())
}

#[tauri::command]
pub async fn set_collapsed_groups(groups: Vec<String>) -> Result<(), String> {
    store::save_collapsed_groups(&groups).map_err(|e| e.to_string())
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
) -> Result<(), String> {
    let m = mgr.lock().await;
    if !m.submit_password(&id, password, save, username, pubkey_path) {
        return Err("No pending password request for this tunnel".into());
    }
    Ok(())
}

/// List the local public keys under ~/.ssh so the user can pick which one to
/// upload. Ordered ed25519 → ecdsa → rsa → others.
#[tauri::command]
pub async fn list_public_keys() -> Result<Vec<crate::model::PublicKeyInfo>, String> {
    Ok(tunnel::list_public_keys())
}

// ─── Keychain helpers ─────────────────────────────────────────────────────────

#[tauri::command]
pub async fn delete_saved_password(
    id: String,
    mgr: State<'_, AppManager>,
) -> Result<(), String> {
    let m = mgr.lock().await;
    if let Some(c) = m.get_config(&id) {
        store::delete_password(&c.jump_user, &c.jump_host, c.jump_port)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
