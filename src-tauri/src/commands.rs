use tauri::State;
use tokio::sync::Mutex as TokioMutex;

use crate::model::{TunnelConfig, TunnelInfo, TunnelState};
use crate::manager::TunnelManager;
use crate::parser::parse_ssh_command;
use crate::probe::{self, PortStatus};
use crate::store;
use crate::tunnel;

pub type AppManager = TokioMutex<TunnelManager>;

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
    config.group = group;
    config.environment = environment;
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
