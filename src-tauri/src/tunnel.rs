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

/// Emit a transient banner notice to the UI (success / warn / error).
pub(crate) fn emit_notice(app: &tauri::AppHandle, id: &str, level: &str, message: &str) {
    let _ = app.emit(
        "tunnel://notice",
        serde_json::json!({ "id": id, "level": level, "message": message }),
    );
}

// ─── Connect result classification ────────────────────────────────────────────

/// Why a connection attempt ended. Fatal errors (auth failures) stop the tunnel
/// with a red Failed state and do NOT trigger auto-reconnect; Retriable errors
/// (network/session drops) go through the normal backoff-reconnect path.
enum ConnectError {
    /// Do not reconnect. Carries the user-facing message shown on the red light.
    Fatal(String),
    /// Reconnect with backoff (if auto_reconnect is on).
    Retriable(anyhow::Error),
}

/// Result of authenticating one session. `pubkey_uploaded` is None when no
/// upload was requested, Some(true/false) for success/failure of the upload.
struct AuthOutcome {
    authed: bool,
    pubkey_uploaded: Option<bool>,
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
            Err(ConnectError::Fatal(msg)) => {
                // Auth failure etc. — stop with a red Failed light, no reconnect.
                tracing::warn!("Tunnel {} fatal: {}", id, msg);
                update_state(&state_map, &id, TunnelState::Failed(msg.clone()));
                emit_state(&app, &id, &TunnelState::Failed(msg));
                break;
            }
            Err(ConnectError::Retriable(e)) => {
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
) -> Result<(), ConnectError> {
    let ssh_config = Arc::new(russh::client::Config {
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        ..Default::default()
    });

    // Phase 1: connect and try local key files only, on their own connection.
    // Some servers (esp. AD/Kerberos-integrated jump hosts) set a very low
    // MaxAuthTries; russh's RSA retry-with-multiple-signature-hashes (see
    // try_publickey_auth) alone can burn through that budget, and if we then
    // continued on the SAME connection the password attempt below would never
    // even get a chance — the server would have already disconnected us.
    let (mut session, mut disconnect_rx) = connect_jump_host(ssh_config.clone(), config).await?;

    let mut pubkey_authed = false;
    for key_path in key_paths(config.identity_file.as_deref()) {
        let p = std::path::Path::new(&key_path);
        if !p.exists() {
            continue;
        }
        match russh_keys::load_secret_key(p, None) {
            Ok(kp) => {
                if try_publickey_auth(&mut session, &config.jump_user, kp, &key_path, "jump")
                    .await
                    .map_err(ConnectError::Retriable)?
                {
                    pubkey_authed = true;
                    break;
                }
            }
            Err(e) => tracing::debug!("Cannot load key {}: {}", key_path, e),
        }
    }

    let outcome = if pubkey_authed {
        AuthOutcome { authed: true, pubkey_uploaded: None }
    } else {
        // Phase 2: reconnect with a brand new TCP+SSH handshake before trying
        // password / keyboard-interactive auth. Whether phase 1 merely
        // refused every key or was disconnected outright by the server, this
        // guarantees the password attempt gets a clean MaxAuthTries budget —
        // exactly like running a second, separate `ssh` invocation would.
        let (session2, disconnect_rx2) = connect_jump_host(ssh_config, config).await?;
        session = session2;
        disconnect_rx = disconnect_rx2;

        password_phase(
            &mut session,
            Some(config.jump_user.as_str()),
            &config.jump_host,
            config.jump_port,
            "jump",
            &config.id,
            password_senders,
            app,
        )
        .await
        .map_err(ConnectError::Retriable)?
    };

    if !outcome.authed {
        // Wrong jump credentials — stop, don't loop the password prompt.
        return Err(ConnectError::Fatal(format!(
            "跳板机 {}@{} 认证失败（用户名或密码错误）",
            config.jump_user, config.jump_host
        )));
    }
    if outcome.pubkey_uploaded == Some(true) {
        emit_notice(app, &config.id, "success",
            &format!("已上传公钥到跳板机 {}，下次免密连接", config.jump_host));
    } else if outcome.pubkey_uploaded == Some(false) {
        emit_notice(app, &config.id, "warn",
            &format!("公钥上传到跳板机 {} 失败（密码仍可用）", config.jump_host));
    }

    // Second layer: every forward whose target is an SSH host (:22) MUST
    // authenticate — green means both hops are truly established. A failure here
    // is fatal (red, no reconnect). This is a connection-time action only; data
    // forwarding below still goes over the single-hop direct-tcpip path.
    for forward in &config.forwards {
        if forward.remote_port != 22 {
            continue;
        }
        let outcome = setup_second_layer(&session, forward, &config.id, password_senders, app).await?;
        if !outcome.authed {
            let _ = session
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
            return Err(ConnectError::Fatal(format!(
                "目标主机 {} 认证失败（用户名或密码错误）",
                forward.remote_host
            )));
        }
        if outcome.pubkey_uploaded == Some(true) {
            emit_notice(app, &config.id, "success",
                &format!("已上传公钥到目标主机 {}，下次免密连接", forward.remote_host));
        } else if outcome.pubkey_uploaded == Some(false) {
            emit_notice(app, &config.id, "warn",
                &format!("公钥上传到目标主机 {} 失败（密码仍可用）", forward.remote_host));
        }
    }

    // Both hops verified — now green.
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
            .map_err(|e| ConnectError::Retriable(anyhow!("Cannot bind {}: {}", bind, e)))?;

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
            Some(TunnelControl::Reconnect) => Err(ConnectError::Retriable(anyhow!("Reconnect requested"))),
        },
        _ = &mut drx => Err(ConnectError::Retriable(anyhow!("SSH session closed"))),
        _ = session_error.notified() => Err(ConnectError::Retriable(anyhow!("SSH channel error"))),
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

/// Open a fresh TCP+SSH connection to the jump host. Called once for the
/// key-based auth attempt and, on a separate call, for the password /
/// keyboard-interactive attempt — each connection gets its own MaxAuthTries
/// budget from the server, mirroring how two independent `ssh` invocations
/// would behave.
async fn connect_jump_host(
    ssh_config: Arc<russh::client::Config>,
    config: &TunnelConfig,
) -> Result<(russh::client::Handle<SshClientHandler>, oneshot::Receiver<()>), ConnectError> {
    let (disconnect_tx, disconnect_rx) = oneshot::channel::<()>();
    let handler = SshClientHandler {
        disconnect_tx: Some(disconnect_tx),
    };
    let session = russh::client::connect(
        ssh_config,
        (config.jump_host.as_str(), config.jump_port),
        handler,
    )
    .await
    .map_err(|e| ConnectError::Retriable(anyhow!("SSH connect failed: {}", e)))?;
    Ok((session, disconnect_rx))
}

// ─── Authentication ───────────────────────────────────────────────────────────

/// Try public-key auth with one loaded key.
///
/// For RSA keys, modern servers reject the legacy `ssh-rsa` (SHA-1) signature
/// and require `rsa-sha2-256/512`. russh's `authenticate_publickey` defaults to
/// the key's own hash (SHA-1 for a freshly loaded RSA key), which such servers
/// deny — even though the key is authorized (openssh negotiates rsa-sha2
/// automatically). So for RSA we retry with SHA2_512 then SHA2_256 before the
/// original. Non-RSA keys are used as-is.
async fn try_publickey_auth(
    session: &mut russh::client::Handle<SshClientHandler>,
    user: &str,
    kp: russh_keys::key::KeyPair,
    key_path: &str,
    layer: &str,
) -> Result<bool> {
    use russh_keys::key::{KeyPair, SignatureHash};

    // Build the ordered list of key variants to try.
    let mut candidates: Vec<KeyPair> = Vec::new();
    if matches!(kp, KeyPair::RSA { .. }) {
        for hash in [SignatureHash::SHA2_512, SignatureHash::SHA2_256] {
            if let Some(variant) = kp.with_signature_hash(hash) {
                candidates.push(variant);
            }
        }
    }
    candidates.push(kp); // original hash last (SHA-1 for RSA, native otherwise)

    for variant in candidates {
        if let Ok(true) = session
            .authenticate_publickey(user, Arc::new(variant))
            .await
        {
            tracing::info!("Auth OK via key {} ({})", key_path, layer);
            return Ok(true);
        }
    }
    Ok(false)
}

/// Try `password` auth first; if the server rejects it outright, fall back to
/// `keyboard-interactive` using the same password as the answer to every
/// prompt. Some jump hosts (common in AD/Kerberos-integrated environments)
/// disable the `password` method entirely and only offer `keyboard-
/// interactive` — which is what an interactive terminal shows as an ordinary
/// "Password:" prompt, so `ssh(1)` falls back to it transparently, but a
/// library that only speaks `password` (like a bare call to
/// `authenticate_password`) gets flatly rejected.
async fn try_password_or_kbi(
    session: &mut russh::client::Handle<SshClientHandler>,
    user: &str,
    password: &str,
    layer: &str,
) -> Result<bool> {
    if let Ok(true) = session.authenticate_password(user, password).await {
        return Ok(true);
    }
    match authenticate_keyboard_interactive(session, user, password).await {
        Ok(true) => {
            tracing::info!("Auth OK via keyboard-interactive ({})", layer);
            Ok(true)
        }
        Ok(false) => Ok(false),
        Err(e) => {
            tracing::debug!("keyboard-interactive attempt failed ({}): {}", layer, e);
            Ok(false)
        }
    }
}

/// Drive a full keyboard-interactive exchange, answering every prompt the
/// server sends with `password` (the common case: a single "Password:"
/// prompt with echo off). Bails out after a handful of rounds to avoid
/// looping forever against a misbehaving server.
async fn authenticate_keyboard_interactive(
    session: &mut russh::client::Handle<SshClientHandler>,
    user: &str,
    password: &str,
) -> Result<bool> {
    use russh::client::KeyboardInteractiveAuthResponse as KbResponse;

    let mut response = session
        .authenticate_keyboard_interactive_start(user, None)
        .await?;

    for _ in 0..8 {
        match response {
            KbResponse::Success => return Ok(true),
            KbResponse::Failure => return Ok(false),
            KbResponse::InfoRequest { prompts, .. } => {
                let answers = vec![password.to_string(); prompts.len()];
                response = session
                    .authenticate_keyboard_interactive_respond(answers)
                    .await?;
            }
        }
    }
    Ok(false)
}

/// Authenticate a session against `host:port` using password / keyboard-
/// interactive only — public-key attempts, if any, must already have
/// happened on a *separate* connection (see `connect_and_forward`'s two-phase
/// jump-host auth) so they can't burn through the server's MaxAuthTries
/// budget before this ever runs.
///
/// - `user`: known username (jump host). `None` means the username is unknown
///   and must be entered by the user in the prompt (second-layer target host).
/// - `layer`: "jump" | "target", forwarded to the UI so it can label the prompt.
///
/// Tries in order: saved keychain password → prompt the user. On a
/// successful prompt, optionally uploads the local public key.
#[allow(clippy::too_many_arguments)]
async fn password_phase(
    session: &mut russh::client::Handle<SshClientHandler>,
    user: Option<&str>,
    host: &str,
    port: u16,
    layer: &str,
    id: &str,
    password_senders: &PasswordSenders,
    app: &tauri::AppHandle,
) -> Result<AuthOutcome> {
    let need_username = user.is_none();

    // Saved password in keychain — only when we already know the username.
    if let Some(user) = user {
        if let Some(pw) = store::get_password(user, host, port) {
            if try_password_or_kbi(session, user, &pw, layer).await? {
                tracing::info!("Auth OK via saved password ({})", layer);
                return Ok(AuthOutcome { authed: true, pubkey_uploaded: None });
            }
        }
    }

    // Prompt user.
    let (tx, rx) = oneshot::channel::<PasswordResponse>();
    {
        password_senders.lock().unwrap().insert(id.to_string(), tx);
    }
    let prompt = match user {
        Some(u) => format!("Password for {}@{}", u, host),
        None => format!("Login to target host {}", host),
    };
    let _ = app.emit(
        "tunnel://password-required",
        serde_json::json!({
            "id": id,
            "prompt": prompt,
            "layer": layer,
            "host": host,
            "needUsername": need_username,
        }),
    );

    match tokio::time::timeout(Duration::from_secs(300), rx).await {
        Ok(Ok(resp)) => {
            // Effective username: config username, or the one the user typed.
            let effective_user = match user {
                Some(u) => u.to_string(),
                None => match resp.username.as_deref().map(str::trim) {
                    Some(u) if !u.is_empty() => u.to_string(),
                    _ => {
                        tracing::warn!("No username provided for target host {}", host);
                        return Ok(AuthOutcome { authed: false, pubkey_uploaded: None });
                    }
                },
            };

            if try_password_or_kbi(session, &effective_user, &resp.password, layer).await? {
                if resp.save {
                    let _ = store::set_password(&effective_user, host, port, &resp.password);
                }
                let mut uploaded: Option<bool> = None;
                if let Some(pub_path) = resp.pubkey_path.as_deref() {
                    match read_pubkey_file(pub_path) {
                        Some(pubkey) => {
                            if let Err(e) = append_authorized_key(session, &pubkey).await {
                                tracing::warn!("Upload pubkey to {} failed: {}", host, e);
                                uploaded = Some(false);
                            } else {
                                tracing::info!("Uploaded {} to {} ({})", pub_path, host, layer);
                                uploaded = Some(true);
                            }
                        }
                        None => {
                            tracing::warn!("Cannot read pubkey file {}; skip upload", pub_path);
                            uploaded = Some(false);
                        }
                    }
                }
                return Ok(AuthOutcome { authed: true, pubkey_uploaded: uploaded });
            }
        }
        Ok(Err(_)) => tracing::warn!("Password channel dropped"),
        Err(_) => {
            tracing::warn!("Password prompt timed out");
            password_senders.lock().unwrap().remove(id);
        }
    }

    Ok(AuthOutcome { authed: false, pubkey_uploaded: None })
}

/// Establish a one-shot second-layer SSH session to the target host through the
/// jump host, authenticate, and optionally upload the public key. The session is
/// closed immediately — data forwarding stays on the single-hop direct-tcpip path.
async fn setup_second_layer(
    jump: &russh::client::Handle<SshClientHandler>,
    forward: &crate::model::ForwardSpec,
    id: &str,
    password_senders: &PasswordSenders,
    app: &tauri::AppHandle,
) -> Result<AuthOutcome, ConnectError> {
    // Open a direct-tcpip channel from the jump host to target:22.
    let channel = jump
        .channel_open_direct_tcpip(&forward.remote_host, forward.remote_port as u32, "127.0.0.1", 0)
        .await
        .map_err(|e| ConnectError::Retriable(anyhow!("open channel to {}:22 failed: {}", forward.remote_host, e)))?;
    let stream = channel.into_stream();

    let ssh_config = Arc::new(russh::client::Config::default());
    let (tx, _rx) = oneshot::channel::<()>();
    let handler = SshClientHandler { disconnect_tx: Some(tx) };

    let mut session = russh::client::connect_stream(ssh_config, stream, handler)
        .await
        .map_err(|e| ConnectError::Retriable(anyhow!("second-layer SSH connect failed: {}", e)))?;

    // Username unknown → prompt (need_username=true). Port fixed at 22.
    let outcome = password_phase(
        &mut session,
        None,
        &forward.remote_host,
        forward.remote_port,
        "target",
        id,
        password_senders,
        app,
    )
    .await
    .map_err(ConnectError::Retriable)?;

    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;

    Ok(outcome)
}

/// Read a specific .pub file's text, trimmed. None if unreadable/empty or the
/// path is not a .pub under ~/.ssh (guard against arbitrary reads).
fn read_pubkey_file(path: &str) -> Option<String> {
    let p = std::path::Path::new(path);
    // Only allow reading .pub files inside ~/.ssh.
    let ssh_dir = dirs::home_dir()?.join(".ssh");
    if p.extension().and_then(|e| e.to_str()) != Some("pub") {
        return None;
    }
    if p.parent() != Some(ssh_dir.as_path()) {
        return None;
    }
    let content = std::fs::read_to_string(p).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// List local public keys under ~/.ssh, preferring ed25519/ecdsa/rsa order,
/// then any other *.pub. `has_private` is true when the paired private key
/// (same name without .pub) exists.
pub(crate) fn list_public_keys() -> Vec<crate::model::PublicKeyInfo> {
    use crate::model::PublicKeyInfo;
    let Some(ssh_dir) = dirs::home_dir().map(|h| h.join(".ssh")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&ssh_dir) else {
        return Vec::new();
    };

    let mut keys: Vec<PublicKeyInfo> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pub") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let content = content.trim().to_string();
        if content.is_empty() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        // Paired private key = same path without the .pub extension.
        let priv_path = path.with_extension("");
        let has_private = priv_path.exists();
        keys.push(PublicKeyInfo {
            path: path.to_string_lossy().to_string(),
            name,
            content,
            has_private,
        });
    }

    // Preferred order first, then the rest alphabetically.
    let rank = |name: &str| match name {
        "id_ed25519.pub" => 0,
        "id_ecdsa.pub" => 1,
        "id_rsa.pub" => 2,
        _ => 3,
    };
    keys.sort_by(|a, b| {
        rank(&a.name)
            .cmp(&rank(&b.name))
            .then_with(|| a.name.cmp(&b.name))
    });
    keys
}

/// Append `pubkey` to the remote host's ~/.ssh/authorized_keys if not present.
async fn append_authorized_key(
    session: &russh::client::Handle<SshClientHandler>,
    pubkey: &str,
) -> Result<()> {
    let shell_cmd = format!(
        r#"mkdir -p ~/.ssh && chmod 700 ~/.ssh && grep -qxF '{key}' ~/.ssh/authorized_keys 2>/dev/null || echo '{key}' >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys && echo OK"#,
        key = pubkey.replace('\'', r"'\''")
    );

    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| anyhow!("open session channel: {}", e))?;
    channel
        .exec(true, shell_cmd.as_str())
        .await
        .map_err(|e| anyhow!("exec failed: {}", e))?;

    let mut stdout = Vec::new();
    while let Some(msg) = channel.wait().await {
        use russh::ChannelMsg;
        match msg {
            ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => {
                if exit_status != 0 {
                    return Err(anyhow!("remote command exit status {}", exit_status));
                }
            }
            ChannelMsg::Eof => break,
            _ => {}
        }
    }

    let output = String::from_utf8_lossy(&stdout);
    if !output.trim().contains("OK") {
        return Err(anyhow!("unexpected output: {}", output.trim()));
    }
    Ok(())
}

fn key_paths(identity_file: Option<&str>) -> Vec<String> {
    if let Some(f) = identity_file {
        return vec![f.to_string()];
    }
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    ["id_ed25519", "id_rsa", "id_ecdsa", "id_ed25519_sk", "id_ecdsa_sk"]
        .iter()
        .map(|name| home.join(".ssh").join(name).to_string_lossy().to_string())
        .collect()
}
