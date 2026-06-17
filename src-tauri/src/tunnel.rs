use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use tauri::Emitter;
use tokio::io::copy_bidirectional;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};

use crate::model::{PasswordResponse, TunnelConfig, TunnelState};
use crate::store;

// Shared state maps, accessible from both tunnel tasks and commands
pub type StateMap = Arc<StdRwLock<HashMap<String, TunnelState>>>;
pub type PasswordSenders = Arc<StdMutex<HashMap<String, oneshot::Sender<PasswordResponse>>>>;

pub enum TunnelControl {
    Stop,
    Reconnect,
}

// ─── SSH client handler ───────────────────────────────────────────────────────

pub(crate) struct SshClientHandler {
    disconnect_tx: Option<oneshot::Sender<()>>,
}

/// When the russh connection task exits (keepalive failure, EOF, etc.),
/// it drops the handler. We use Drop to signal the tunnel task.
impl Drop for SshClientHandler {
    fn drop(&mut self) {
        if let Some(tx) = self.disconnect_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[async_trait::async_trait]
impl russh::client::Handler for SshClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh_keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // TOFU: trust on first use. TODO: verify against known_hosts.
        Ok(true)
    }
}

// ─── State helpers ────────────────────────────────────────────────────────────

pub(crate) fn update_state(state_map: &StateMap, id: &str, state: TunnelState) {
    if let Ok(mut map) = state_map.write() {
        map.insert(id.to_string(), state);
    }
}

pub(crate) fn emit_state(app: &tauri::AppHandle, id: &str, state: &TunnelState) {
    let _ = app.emit(
        "tunnel://state-changed",
        serde_json::json!({ "id": id, "state": state }),
    );
}

// ─── Main tunnel loop ─────────────────────────────────────────────────────────

/// Spawned once per tunnel. Handles connect → auth → forward → disconnect → reconnect.
pub async fn run_tunnel(
    config: TunnelConfig,
    mut control_rx: mpsc::Receiver<TunnelControl>,
    state_map: StateMap,
    password_senders: PasswordSenders,
    app: tauri::AppHandle,
) {
    let id = config.id.clone();
    let mut backoff_secs: u64 = 1;

    loop {
        update_state(&state_map, &id, TunnelState::Connecting);
        emit_state(&app, &id, &TunnelState::Connecting);

        let result = connect_and_forward(
            &config,
            &mut control_rx,
            &state_map,
            &password_senders,
            &app,
        )
        .await;

        match result {
            Ok(()) => {
                // Clean stop via TunnelControl::Stop
                update_state(&state_map, &id, TunnelState::Disconnected);
                emit_state(&app, &id, &TunnelState::Disconnected);
                break;
            }
            Err(e) => {
                if !config.auto_reconnect {
                    let msg = e.to_string();
                    update_state(&state_map, &id, TunnelState::Failed(msg.clone()));
                    emit_state(&app, &id, &TunnelState::Failed(msg));
                    break;
                }

                // Check for an explicit Stop before sleeping
                if let Ok(TunnelControl::Stop) = control_rx.try_recv() {
                    update_state(&state_map, &id, TunnelState::Disconnected);
                    emit_state(&app, &id, &TunnelState::Disconnected);
                    break;
                }

                tracing::info!(
                    "Tunnel {} error: {}. Reconnecting in {}s",
                    id, e, backoff_secs
                );
                update_state(&state_map, &id, TunnelState::Reconnecting);
                emit_state(&app, &id, &TunnelState::Reconnecting);

                let delay = Duration::from_secs(backoff_secs);
                backoff_secs = (backoff_secs * 2).min(30);

                // Sleep, but allow early wake-up via Reconnect / Stop control
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    msg = control_rx.recv() => {
                        match msg {
                            Some(TunnelControl::Stop) | None => {
                                update_state(&state_map, &id, TunnelState::Disconnected);
                                emit_state(&app, &id, &TunnelState::Disconnected);
                                break;
                            }
                            Some(TunnelControl::Reconnect) => {
                                // Skip sleep, reconnect immediately
                                backoff_secs = 1;
                            }
                        }
                    }
                }
            }
        }
    }
}

// ─── Connect + forward ────────────────────────────────────────────────────────

async fn connect_and_forward(
    config: &TunnelConfig,
    control_rx: &mut mpsc::Receiver<TunnelControl>,
    state_map: &StateMap,
    password_senders: &PasswordSenders,
    app: &tauri::AppHandle,
) -> Result<()> {
    let ssh_config = Arc::new(russh::client::Config {
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        ..Default::default()
    });

    // The handler's Drop will fire disconnect_tx when the connection closes
    let (disconnect_tx, disconnect_rx) = oneshot::channel::<()>();
    let handler = SshClientHandler {
        disconnect_tx: Some(disconnect_tx),
    };

    let mut session = russh::client::connect(
        ssh_config,
        (config.jump_host.as_str(), config.jump_port),
        handler,
    )
    .await
    .map_err(|e| anyhow!("SSH connect failed: {}", e))?;

    let authed = authenticate(&mut session, config, password_senders, app).await?;
    if !authed {
        return Err(anyhow!("Authentication failed for {}@{}", config.jump_user, config.jump_host));
    }

    update_state(state_map, &config.id, TunnelState::Connected);
    emit_state(app, &config.id, &TunnelState::Connected);

    // Wrap session for sharing between listener tasks
    let session = Arc::new(TokioMutex::new(session));

    // session_error_notify: any listener task signals session death here
    let session_error = Arc::new(tokio::sync::Notify::new());
    let mut listener_handles = Vec::new();

    for forward in &config.forwards {
        let bind_addr = if config.bind_all { "0.0.0.0" } else { "127.0.0.1" };
        let bind = format!("{}:{}", bind_addr, forward.local_port);

        let listener = TcpListener::bind(&bind)
            .await
            .map_err(|e| anyhow!("Cannot bind {}: {}", bind, e))?;

        tracing::info!("Listening on {} → {}:{}", bind, forward.remote_host, forward.remote_port);

        let session = session.clone();
        let remote_host = forward.remote_host.clone();
        let remote_port = forward.remote_port;
        let notify = session_error.clone();

        let h = tokio::spawn(async move {
            loop {
                let (tcp_stream, peer) = match listener.accept().await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("accept() error: {}", e);
                        break;
                    }
                };
                tracing::debug!("New connection {} → {}:{}", peer, remote_host, remote_port);

                let session = session.clone();
                let rhost = remote_host.clone();
                let rport = remote_port;
                let notify = notify.clone();

                tokio::spawn(async move {
                    let ch_result = {
                        let sess = session.lock().await;
                        sess.channel_open_direct_tcpip(&rhost, rport as u32, "127.0.0.1", 0)
                            .await
                    };

                    match ch_result {
                        Ok(channel) => {
                            let mut ssh_stream = channel.into_stream();
                            let mut tcp = tcp_stream;
                            if let Err(e) = copy_bidirectional(&mut tcp, &mut ssh_stream).await {
                                tracing::debug!("pump ended: {}", e);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("channel_open_direct_tcpip error: {}", e);
                            notify.notify_one();
                        }
                    }
                });
            }
        });

        listener_handles.push(h);
    }

    // Wait: Stop/Reconnect control | SSH disconnect | listener error
    let mut drx = disconnect_rx;
    let result = tokio::select! {
        msg = control_rx.recv() => match msg {
            Some(TunnelControl::Stop) | None => Ok(()),
            Some(TunnelControl::Reconnect) => Err(anyhow!("Reconnect requested")),
        },
        _ = &mut drx => Err(anyhow!("SSH session closed")),
        _ = session_error.notified() => Err(anyhow!("SSH channel error")),
    };

    // Cleanup
    for h in &listener_handles {
        h.abort();
    }
    {
        let sess = session.lock().await;
        let _ = sess.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    }

    result
}

// ─── Authentication ───────────────────────────────────────────────────────────

async fn authenticate(
    session: &mut russh::client::Handle<SshClientHandler>,
    config: &TunnelConfig,
    password_senders: &PasswordSenders,
    app: &tauri::AppHandle,
) -> Result<bool> {
    let user = config.jump_user.as_str();

    // 1. Key files
    for key_path in key_paths(config) {
        let p = std::path::Path::new(&key_path);
        if !p.exists() {
            continue;
        }
        match russh_keys::load_secret_key(p, None) {
            Ok(kp) => {
                match session.authenticate_publickey(user, Arc::new(kp)).await {
                    Ok(true) => {
                        tracing::info!("Auth OK via key {}", key_path);
                        return Ok(true);
                    }
                    _ => {}
                }
            }
            Err(e) => tracing::debug!("Cannot load key {}: {}", key_path, e),
        }
    }

    // 2. Saved password in keychain
    if let Some(pw) = store::get_password(user, &config.jump_host, config.jump_port) {
        if let Ok(true) = session.authenticate_password(user, &pw).await {
            tracing::info!("Auth OK via saved password");
            return Ok(true);
        }
    }

    // 3. Prompt user
    let (tx, rx) = oneshot::channel::<PasswordResponse>();
    {
        password_senders.lock().unwrap().insert(config.id.clone(), tx);
    }
    let _ = app.emit(
        "tunnel://password-required",
        serde_json::json!({
            "id": &config.id,
            "prompt": format!("Password for {}@{}", user, config.jump_host),
        }),
    );

    match tokio::time::timeout(Duration::from_secs(300), rx).await {
        Ok(Ok(resp)) => {
            match session.authenticate_password(user, &resp.password).await {
                Ok(true) => {
                    if resp.save {
                        let _ = store::set_password(user, &config.jump_host, config.jump_port, &resp.password);
                    }
                    return Ok(true);
                }
                _ => {}
            }
        }
        Ok(Err(_)) => tracing::warn!("Password channel dropped"),
        Err(_) => {
            tracing::warn!("Password prompt timed out");
            password_senders.lock().unwrap().remove(&config.id);
        }
    }

    Ok(false)
}

fn key_paths(config: &TunnelConfig) -> Vec<String> {
    if let Some(ref f) = config.identity_file {
        return vec![f.clone()];
    }
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    ["id_ed25519", "id_rsa", "id_ecdsa", "id_ed25519_sk", "id_ecdsa_sk"]
        .iter()
        .map(|name| home.join(".ssh").join(name).to_string_lossy().to_string())
        .collect()
}

// ─── One-shot session for upload_pubkey ──────────────────────────────────────

/// Open an authenticated session without starting any port forwards.
/// Used for upload_pubkey and similar one-off operations.
pub(crate) async fn open_session(
    config: &TunnelConfig,
    password_senders: &PasswordSenders,
    app: &tauri::AppHandle,
) -> Result<russh::client::Handle<SshClientHandler>> {
    let ssh_config = Arc::new(russh::client::Config::default());
    let (tx, _rx) = oneshot::channel::<()>();
    let handler = SshClientHandler { disconnect_tx: Some(tx) };

    let mut session = russh::client::connect(
        ssh_config,
        (config.jump_host.as_str(), config.jump_port),
        handler,
    )
    .await
    .map_err(|e| anyhow!("SSH connect failed: {}", e))?;

    let ok = authenticate(&mut session, config, password_senders, app).await?;
    if !ok {
        return Err(anyhow!("Authentication failed"));
    }
    Ok(session)
}
