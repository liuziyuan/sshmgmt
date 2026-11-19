use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use tauri::Emitter;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};

use crate::model::{ForwardKind, Hop, PasswordResponse, TunnelConfig, TunnelState};
use crate::socks;
use crate::store;

// Shared state maps, accessible from both tunnel tasks and commands
pub type StateMap = Arc<StdRwLock<HashMap<String, TunnelState>>>;
pub type PasswordSenders = Arc<StdMutex<HashMap<String, oneshot::Sender<PasswordResponse>>>>;

pub enum TunnelControl {
    Stop,
    Reconnect,
}

// ─── SSH client handler ───────────────────────────────────────────────────────

/// A server-initiated `forwarded-tcpip` connection, produced when a `-R`
/// listener on the server receives a connection. Handed from the SSH read loop
/// to the tunnel task through an unbounded channel.
pub(crate) struct ForwardedConn {
    pub listen_addr: String,
    pub listen_port: u16,
    pub channel: russh::Channel<russh::client::Msg>,
}

pub(crate) struct SshClientHandler {
    disconnect_tx: Option<oneshot::Sender<()>>,
    /// Present only on connections of the FINAL hop when the tunnel has
    /// Remote rules: tcpip_forward is requested on that hop and
    /// forwarded-tcpip callbacks arrive on that same connection. Intermediate
    /// hops always carry None.
    forward_tx: Option<mpsc::UnboundedSender<ForwardedConn>>,
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

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        // Runs inside the connection's read loop: hand the channel off and
        // return immediately. Returning Err here would interrupt the whole
        // connection (russh propagates it through `?`), so every failure is
        // swallowed. russh has already confirmed the channel; dropping it
        // closed the server-side connection.
        let Ok(listen_port) = u16::try_from(connected_port) else {
            return Ok(());
        };
        if let Some(tx) = &self.forward_tx {
            let _ = tx.send(ForwardedConn {
                listen_addr: connected_address.to_string(),
                listen_port,
                channel,
            });
        }
        Ok(())
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

/// An authenticated jump chain plus what the forwarding layer needs.
/// `chain` is ordered entry hop → final hop; all forwarding uses the final
/// handle. `disconnect_rxs` holds one raw signal per hop. `forward_rx` is Some
/// only when the tunnel has Remote rules (sender lives on the final hop).
struct EstablishedSession {
    chain: Vec<russh::client::Handle<SshClientHandler>>,
    disconnect_rxs: Vec<oneshot::Receiver<()>>,
    forward_rx: Option<mpsc::UnboundedReceiver<ForwardedConn>>,
}

// ─── Remote-forward support (-R) ──────────────────────────────────────────────

/// Max time to wait for the server to acknowledge a tcpip-forward request.
/// russh has no built-in timeout; without this a black-holed session would
/// leave the tunnel stuck on "Connecting" forever.
const TCPIP_FORWARD_TIMEOUT: Duration = Duration::from_secs(15);

/// Max time to wait for a cancel_tcpip_forward acknowledgement. Cleanup must
/// not hang: closing the connection revokes server-side listeners anyway.
const TCPIP_CANCEL_TIMEOUT: Duration = Duration::from_secs(3);

/// A failed tcpip-forward request, before user-facing classification.
enum TcpipForwardErr {
    TimedOut,
    Russh(russh::Error),
}

/// A successfully requested server-side listener. `listen_addr`/`listen_port`
/// are what the server reports back, i.e. what incoming forwarded-tcpip
/// connections carry; `target_*` is the local service to pump to.
struct RemoteRoute {
    listen_addr: String,
    listen_port: u16,
    target_host: String,
    target_port: u16,
}

/// Join handles for tasks owned by one connection attempt. Drop aborts them:
/// every early-return path must stop listeners/dispatchers instead of
/// detaching them (M1 — a leaked listener keeps a port occupied and turns
/// every later reconnect into a bind failure).
struct TaskSet(Vec<tokio::task::JoinHandle<()>>);

impl TaskSet {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn push(&mut self, handle: tokio::task::JoinHandle<()>) {
        self.0.push(handle);
    }

    /// Abort every registered task and forget them. Idempotent; a second call
    /// (e.g. from Drop after the normal cleanup path) is a no-op.
    fn abort_all(&mut self) {
        for handle in self.0.drain(..) {
            handle.abort();
        }
    }
}

impl Drop for TaskSet {
    fn drop(&mut self) {
        self.abort_all();
    }
}

/// Map a failed tcpip-forward request to the tunnel error taxonomy (B3).
/// A server-side refusal is permanent for this config (port taken or remote
/// forwarding disabled) → Fatal, no reconnect. Session-level failures and
/// timeouts are network-class → Retriable.
fn classify_tcpip_forward_err(port: u16, err: TcpipForwardErr) -> ConnectError {
    match err {
        TcpipForwardErr::TimedOut => ConnectError::Retriable(anyhow!(
            "申请远端转发监听超时（端口 {}，{}s）",
            port,
            TCPIP_FORWARD_TIMEOUT.as_secs()
        )),
        TcpipForwardErr::Russh(russh::Error::RequestDenied) => ConnectError::Fatal(format!(
            "远端端口 {} 被服务器拒绝（可能被占用或未开放转发），已停止",
            port
        )),
        TcpipForwardErr::Russh(e) => {
            ConnectError::Retriable(anyhow!("申请远端转发端口 {} 失败: {}", port, e))
        }
    }
}

/// Best-effort cancel of the listeners already granted during this attempt.
/// Uses the actual (server-reported) port, which matters for `-R 0`.
async fn cancel_remote_forwards(
    session: &russh::client::Handle<SshClientHandler>,
    applied: &[(String, u32)],
) {
    for (addr, port) in applied {
        match tokio::time::timeout(
            TCPIP_CANCEL_TIMEOUT,
            session.cancel_tcpip_forward(addr.as_str(), *port),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("cancel_tcpip_forward {}:{} failed: {}", addr, port, e),
            Err(_) => tracing::warn!("cancel_tcpip_forward {}:{} timed out", addr, port),
        }
    }
}

/// Pump one forwarded connection to its local target. Errors only close this
/// one connection; they never signal session death (handler Drop handles that).
async fn pump_forwarded(conn: ForwardedConn, host: String, port: u16) {
    let mut tcp = match TcpStream::connect((host.as_str(), port)).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "Remote forward: cannot connect to local target {}:{}: {}",
                host,
                port,
                e
            );
            return; // dropping `conn` closes the server-side channel
        }
    };
    let mut ssh_stream = conn.channel.into_stream();
    if let Err(e) = copy_bidirectional(&mut tcp, &mut ssh_stream).await {
        tracing::debug!("remote forward pump ended: {}", e);
    }
}

/// Abort this attempt's tasks, cancel granted server listeners, close the
/// session, and hand back the bind error. Used when any listener fails to
/// bind after other tasks were already spawned (M1).
async fn cleanup_failed_bind(
    tasks: &mut TaskSet,
    session: &Arc<TokioMutex<russh::client::Handle<SshClientHandler>>>,
    applied: &[(String, u32)],
    bind: &str,
    err: std::io::Error,
    intermediates: &[russh::client::Handle<SshClientHandler>],
) -> ConnectError {
    tasks.abort_all();
    {
        let sess = session.lock().await;
        cancel_remote_forwards(&sess, applied).await;
        let _ = sess
            .disconnect(russh::Disconnect::ByApplication, "", "en")
            .await;
    }
    disconnect_chain_reverse(intermediates).await;
    ConnectError::Retriable(anyhow!("Cannot bind {}: {}", bind, err))
}

/// Resolve once when ANY hop's session ends. Each receiver is watched in its
/// own JoinSet task; the JoinSet is dropped (aborting the watchers) as soon as
/// the first completes, so this task never leaks watchers. A receiver
/// resolving with `Err` means its sender was dropped — that hop's handler is
/// gone, which is equally a session end. The tunnel treats one aggregate
/// signal exactly like the single disconnect signal of v1.4.
async fn forward_first_signal(rxs: Vec<oneshot::Receiver<()>>, done: oneshot::Sender<()>) {
    let mut watchers = tokio::task::JoinSet::new();
    for rx in rxs {
        watchers.spawn(async move {
            let _ = rx.await;
        });
    }
    if watchers.join_next().await.is_some() {
        let _ = done.send(());
    }
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
    // Rules whose kind this version does not understand (written by a newer
    // app) are preserved in the config but cannot run. If nothing can run at
    // all, fail loudly and stop: a red light is honest, a fake green is not.
    if config.forwards.iter().all(|f| !f.is_supported()) {
        return Err(ConnectError::Fatal(
            "该隧道没有可用的转发规则（可能来自更高版本），请升级应用".into(),
        ));
    }

    // Establishing the session (TCP connect, jump-host key/password auth, and
    // any second-layer target-host auth) can block for a long time — most
    // notably the password/keyboard-interactive prompt, which waits up to 5
    // minutes for the user. Nothing inside `establish_session` polls
    // `control_rx`, so without racing it here a Stop/Reconnect sent while the
    // tunnel is still "Connecting" (yellow) would just sit in the channel and
    // only take effect once the connection attempt happened to finish or
    // time out — the Disconnect button would appear to do nothing. Racing
    // establishment against `control_rx` makes Stop/Reconnect take effect
    // immediately at any point up through "Connected".
    let EstablishedSession {
        mut chain,
        disconnect_rxs,
        forward_rx,
    } = tokio::select! {
        res = establish_session(config, password_senders, app) => res?,
        ctrl = control_rx.recv() => {
            // Abort: `establish_session`'s future (and everything it was
            // awaiting — the TCP connect, an SSH session, a password wait)
            // is dropped right here. Also clear any pending password prompt
            // for this tunnel so a still-open modal isn't left waiting on a
            // channel nobody will ever answer.
            password_senders.lock().unwrap().remove(&config.id);
            return match ctrl {
                Some(TunnelControl::Stop) | None => Ok(()),
                Some(TunnelControl::Reconnect) => {
                    Err(ConnectError::Retriable(anyhow!("Reconnect requested")))
                }
            };
        }
    };

    // ── Ask the server for every -R listener before turning green. Green
    // means "the server accepts connections on every requested port"; a
    // partial success would be a lie. Requests run in rule order on the FINAL
    // hop of the chain (the only connection that can receive forwarded-tcpip).
    // On any failure, cancel the already-granted listeners, tear the chain
    // down farthest-first (which also drops the handler tasks), then return
    // the classified error.
    let final_idx = match chain.len().checked_sub(1) {
        Some(idx) => idx,
        None => return Err(ConnectError::Fatal("隧道缺少跳板配置".into())),
    };
    let mut applied: Vec<(String, u32)> = Vec::new();
    let mut routes: Vec<RemoteRoute> = Vec::new();
    let mut allocated_dynamic: Vec<(String, u16)> = Vec::new();
    for forward in &config.forwards {
        if forward.kind != ForwardKind::Remote {
            continue;
        }
        // None → the safe loopback default; Some("") keeps OpenSSH's
        // "all interfaces" spelling and is passed through verbatim.
        let addr = forward
            .remote_bind
            .clone()
            .unwrap_or_else(|| "localhost".to_string());
        let requested = forward.listen_port();
        let result = {
            // The final hop is the forwarding outlet; the mutable borrow is
            // scoped to the request so the chain stays usable for cleanup.
            let final_handle = &mut chain[final_idx];
            tokio::time::timeout(
                TCPIP_FORWARD_TIMEOUT,
                final_handle.tcpip_forward(addr.as_str(), requested as u32),
            )
            .await
        };

        let failure = match result {
            Ok(Ok(allocated)) => {
                // -R 0: the server reports the port it picked; otherwise the
                // reply carries 0 and our requested port is authoritative.
                let actual = if requested != 0 {
                    requested as u32
                } else {
                    allocated
                };
                applied.push((addr.clone(), actual));
                match u16::try_from(actual) {
                    Ok(actual_port) => {
                        if requested == 0 {
                            allocated_dynamic.push((addr.clone(), actual_port));
                        }
                        routes.push(RemoteRoute {
                            listen_addr: addr.clone(),
                            listen_port: actual_port,
                            target_host: forward.remote_host.clone(),
                            target_port: forward.remote_port,
                        });
                        None
                    }
                    // Not representable in a u16 — treat as an internal error
                    // but still cancel the grant we just received.
                    Err(_) => Some(ConnectError::Retriable(anyhow!(
                        "服务器分配的远端端口超出范围: {}",
                        actual
                    ))),
                }
            }
            Ok(Err(e)) => Some(classify_tcpip_forward_err(
                requested,
                TcpipForwardErr::Russh(e),
            )),
            Err(_) => Some(classify_tcpip_forward_err(
                requested,
                TcpipForwardErr::TimedOut,
            )),
        };

        if let Some(err) = failure {
            cancel_remote_forwards(&chain[final_idx], &applied).await;
            disconnect_chain_reverse(&chain).await;
            return Err(err);
        }
    }

    // Both hops verified and every server-side listener granted — now green.
    update_state(state_map, &config.id, TunnelState::Connected);
    emit_state(app, &config.id, &TunnelState::Connected);

    // Announce server-assigned ports only after every rule succeeded, so a
    // later failure can never leave a misleading "port X" notice behind.
    for (addr, port) in &allocated_dynamic {
        let shown_addr = if addr.is_empty() { "0.0.0.0" } else { addr.as_str() };
        emit_notice(
            app,
            &config.id,
            "success",
            &format!("服务器已分配远端端口 {}:{}", shown_addr, port),
        );
    }

    // The final hop is the sole forwarding outlet (-L/-D channels, -R
    // dispatch, :22 checks all already used it only above). Keep the middle
    // hops for ordered teardown.
    let final_handle = chain.pop().expect("establish_session returns a non-empty chain");
    let intermediates = chain;
    let session = Arc::new(TokioMutex::new(final_handle));

    // session_error_notify: any listener task signals session death here
    let session_error = Arc::new(tokio::sync::Notify::new());
    let mut tasks = TaskSet::new();

    // Any hop's disconnect signal wakes the tunnel exactly once. Spawned
    // outside the establishment race (invariant 1) and aborted with the rest.
    let (disconnect_tx, mut disconnect_rx) = oneshot::channel::<()>();
    tasks.push(tokio::spawn(forward_first_signal(
        disconnect_rxs,
        disconnect_tx,
    )));

    for forward in &config.forwards {
        let bind_addr = if config.bind_all { "0.0.0.0" } else { "127.0.0.1" };

        match &forward.kind {
            ForwardKind::Local => {
                let bind = format!("{}:{}", bind_addr, forward.local_port);

                let listener = match TcpListener::bind(&bind).await {
                    Ok(l) => l,
                    Err(e) => {
                        return Err(
                            cleanup_failed_bind(
                                &mut tasks,
                                &session,
                                &applied,
                                &bind,
                                e,
                                &intermediates,
                            )
                            .await,
                        )
                    }
                };

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

                tasks.push(h);
            }
            ForwardKind::Dynamic => {
                // Dynamic rules get a local SOCKS5 proxy instead of a fixed
                // target: each client picks its destination at handshake time.
                // Spawned outside the establishment race like the -L listener;
                // its handle joins the same abort list.
                let bind = format!("{}:{}", bind_addr, forward.listen_port());

                let listener = match TcpListener::bind(&bind).await {
                    Ok(l) => l,
                    Err(e) => {
                        return Err(
                            cleanup_failed_bind(
                                &mut tasks,
                                &session,
                                &applied,
                                &bind,
                                e,
                                &intermediates,
                            )
                            .await,
                        )
                    }
                };

                tracing::info!("SOCKS5 proxy listening on {}", bind);

                let session = session.clone();
                let notify = session_error.clone();
                let h = tokio::spawn(socks::serve_dynamic(listener, session, notify));
                tasks.push(h);
            }
            // Remote rules create no local listener; the server-side listener
            // is requested before "Connected" and dispatched by the -R task.
            ForwardKind::Remote => {}
            ForwardKind::Unknown(raw) => {
                tracing::warn!(
                    "Skipping unsupported forward rule kind {:?} on tunnel {}",
                    raw,
                    config.id
                );
            }
        }
    }

    // Dispatch server-pushed forwarded-tcpip connections to their local
    // targets. Spawned after the establishment race (invariant 1) and owned by
    // TaskSet. Per-connection pumps are independent tasks: the dispatch loop
    // never waits on IO beyond the channel itself.
    if let Some(mut rx) = forward_rx {
        let routes = Arc::new(routes);
        let h = tokio::spawn(async move {
            while let Some(conn) = rx.recv().await {
                match routes.iter().find(|r| {
                    r.listen_addr == conn.listen_addr && r.listen_port == conn.listen_port
                }) {
                    Some(route) => {
                        let host = route.target_host.clone();
                        let port = route.target_port;
                        tokio::spawn(pump_forwarded(conn, host, port));
                    }
                    // Address normalization drift or a grant we no longer
                    // track: drop the channel rather than guess a target.
                    None => tracing::warn!(
                        "forwarded-tcpip for unknown listener {}:{}",
                        conn.listen_addr,
                        conn.listen_port
                    ),
                }
            }
        });
        tasks.push(h);
    }

    // Wait: Stop/Reconnect control | any-hop disconnect | listener error
    let result = tokio::select! {
        msg = control_rx.recv() => match msg {
            Some(TunnelControl::Stop) | None => Ok(()),
            Some(TunnelControl::Reconnect) => Err(ConnectError::Retriable(anyhow!("Reconnect requested"))),
        },
        _ = &mut disconnect_rx => Err(ConnectError::Retriable(anyhow!("SSH session closed"))),
        _ = session_error.notified() => Err(ConnectError::Retriable(anyhow!("SSH channel error"))),
    };

    // Cleanup: stop accepting (listeners + dispatch + signal watcher), cancel
    // the server-side listeners on the final hop using their actual ports
    // (crucial for -R 0), disconnect the final hop, then the middle hops
    // farthest-first. Cancelling is best-effort: closing the session revokes
    // server-side listeners regardless.
    tasks.abort_all();
    {
        let sess = session.lock().await;
        cancel_remote_forwards(&sess, &applied).await;
        let _ = sess.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    }
    disconnect_chain_reverse(&intermediates).await;

    result
}

/// Connect and authenticate every hop of the jump chain, then (for `:22`
/// Local targets) the second-layer login check. Hops are established serially:
/// the entry hop over direct TCP, each later hop over a fresh direct-tcpip
/// channel on the previous hop. Every hop keeps its own two-phase auth (keys
/// on one connection, password/keyboard-interactive on a fresh one) so each
/// attempt gets an independent MaxAuthTries budget.
///
/// Returns the authenticated chain (entry → final) plus one raw disconnect
/// signal per hop. Deliberately does NOT touch `state_map` / emit `Connected` —
/// the caller does that only after this future wins its race against
/// `control_rx` and every -R listener is granted.
async fn establish_session(
    config: &TunnelConfig,
    password_senders: &PasswordSenders,
    app: &tauri::AppHandle,
) -> Result<EstablishedSession, ConnectError> {
    let ssh_config = Arc::new(russh::client::Config {
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        ..Default::default()
    });

    let hops = config.resolved_hops();
    if hops.is_empty() {
        return Err(ConnectError::Fatal("隧道缺少跳板配置".into()));
    }
    let total = hops.len();

    // Only tunnels with Remote rules need the forwarded-tcpip channel, and
    // only the FINAL hop carries its sender: tcpip_forward is requested there
    // and forwarded-tcpip arrives on that same connection.
    let (forward_tx, forward_rx) = if config
        .forwards
        .iter()
        .any(|f| f.kind == ForwardKind::Remote)
    {
        let (tx, rx) = mpsc::unbounded_channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let mut chain: Vec<russh::client::Handle<SshClientHandler>> = Vec::new();
    let mut disconnect_rxs: Vec<oneshot::Receiver<()>> = Vec::new();

    for (idx, hop) in hops.iter().enumerate() {
        let is_final = idx + 1 == total;
        let hop_forward_tx = if is_final { forward_tx.clone() } else { None };
        let hop_ctx = if total > 1 {
            Some((idx + 1, total))
        } else {
            None
        };

        // Phase 1: a fresh connection that only tries local key files. The
        // entry hop connects directly; later hops open a channel over the
        // already-authenticated chain.
        let opened =
            open_hop_connection(chain.last(), hop, ssh_config.clone(), hop_forward_tx.clone())
                .await;
        let (mut session, mut disconnect_rx) = match opened {
            Ok(v) => v,
            Err(e) => {
                disconnect_chain_reverse(&chain).await;
                return Err(e);
            }
        };

        let mut pubkey_authed = false;
        for key_path in key_paths(config.identity_file.as_deref()) {
            let p = std::path::Path::new(&key_path);
            if !p.exists() {
                continue;
            }
            match russh_keys::load_secret_key(p, None) {
                Ok(kp) => {
                    match try_publickey_auth(&mut session, &hop.user, kp, &key_path, "jump").await
                    {
                        Ok(true) => {
                            pubkey_authed = true;
                            break;
                        }
                        Ok(false) => {}
                        Err(e) => {
                            let _ = session
                                .disconnect(russh::Disconnect::ByApplication, "", "en")
                                .await;
                            disconnect_chain_reverse(&chain).await;
                            return Err(ConnectError::Retriable(e));
                        }
                    }
                }
                Err(e) => tracing::debug!("Cannot load key {}: {}", key_path, e),
            }
        }

        let outcome = if pubkey_authed {
            AuthOutcome {
                authed: true,
                pubkey_uploaded: None,
            }
        } else {
            // Phase 2: retire the key-only connection and open a brand-new one
            // (fresh TCP+SSH for the entry hop, a new channel through the same
            // previous hops otherwise) so the password attempt gets a clean
            // MaxAuthTries budget. Whether phase 1 merely refused every key or
            // was disconnected outright, this mirrors a second ssh invocation.
            let _ = session
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
            match open_hop_connection(chain.last(), hop, ssh_config.clone(), hop_forward_tx).await
            {
                Ok((s2, rx2)) => {
                    session = s2;
                    disconnect_rx = rx2;
                }
                Err(e) => {
                    disconnect_chain_reverse(&chain).await;
                    return Err(e);
                }
            }

            match password_phase(
                &mut session,
                Some(hop.user.as_str()),
                &hop.host,
                hop.port,
                "jump",
                &config.id,
                password_senders,
                app,
                hop_ctx,
            )
            .await
            {
                Ok(o) => o,
                Err(e) => {
                    let _ = session
                        .disconnect(russh::Disconnect::ByApplication, "", "en")
                        .await;
                    disconnect_chain_reverse(&chain).await;
                    return Err(ConnectError::Retriable(e));
                }
            }
        };

        if !outcome.authed {
            // Wrong credentials on this hop — stop, don't loop the prompt.
            let _ = session
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
            disconnect_chain_reverse(&chain).await;
            return Err(ConnectError::Fatal(hop_auth_failure_msg(
                idx,
                total,
                &hop.user,
                &hop.host,
            )));
        }
        match outcome.pubkey_uploaded {
            Some(true) => {
                let msg = if total == 1 {
                    format!("已上传公钥到跳板机 {}，下次免密连接", hop.host)
                } else {
                    format!(
                        "已上传公钥到第 {}/{} 跳 {}，下次免密连接",
                        idx + 1,
                        total,
                        hop.host
                    )
                };
                emit_notice(app, &config.id, "success", &msg);
            }
            Some(false) => {
                let msg = if total == 1 {
                    format!("公钥上传到跳板机 {} 失败（密码仍可用）", hop.host)
                } else {
                    format!(
                        "公钥上传到第 {}/{} 跳 {} 失败（密码仍可用）",
                        idx + 1,
                        total,
                        hop.host
                    )
                };
                emit_notice(app, &config.id, "warn", &msg);
            }
            None => {}
        }

        chain.push(session);
        disconnect_rxs.push(disconnect_rx);
    }

    // Second layer: every Local forward whose target is an SSH host (:22)
    // must authenticate through the final hop — green means both the chain and
    // the target are truly reachable. A failure here is fatal (red, no
    // reconnect). This is a connection-time action only; data forwarding still
    // goes over the final hop's direct-tcpip path.
    let final_user = hops.last().expect("hops is non-empty").user.clone();
    for forward in &config.forwards {
        // Only Local rules have a fixed target to pre-authenticate. Dynamic
        // rules pick their destination per SOCKS connection, and Remote rules'
        // target is a local service, so the kind check is explicit.
        if forward.kind != ForwardKind::Local || forward.remote_port != 22 {
            continue;
        }
        let final_handle = chain.last().expect("chain is non-empty");
        let outcome = match setup_second_layer(
            final_handle,
            config,
            forward,
            &final_user,
            password_senders,
            app,
        )
        .await
        {
            Ok(o) => o,
            Err(e) => {
                let _ = final_handle
                    .disconnect(russh::Disconnect::ByApplication, "", "en")
                    .await;
                disconnect_chain_reverse(&chain[..chain.len() - 1]).await;
                return Err(e);
            }
        };
        if !outcome.authed {
            let _ = final_handle
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
            disconnect_chain_reverse(&chain[..chain.len() - 1]).await;
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

    Ok(EstablishedSession {
        chain,
        disconnect_rxs,
        forward_rx,
    })
}

/// Max time to wait for the initial TCP+SSH handshake to the jump host.
/// Without this, an unreachable/black-holed jump host would leave the tunnel
/// stuck on "Connecting" (yellow) indefinitely instead of failing/retrying —
/// the two-phase auth race in `connect_and_forward` still lets the user
/// disconnect manually while stuck, but this avoids relying on that.
const JUMP_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Open a fresh TCP+SSH connection to one hop. Called once for the key-based
/// auth attempt and, on a separate call, for the password /
/// keyboard-interactive attempt — each connection gets its own MaxAuthTries
/// budget from the server, mirroring how two independent `ssh` invocations
/// would behave.
async fn connect_jump_host(
    ssh_config: Arc<russh::client::Config>,
    hop: &Hop,
    forward_tx: Option<mpsc::UnboundedSender<ForwardedConn>>,
) -> Result<(russh::client::Handle<SshClientHandler>, oneshot::Receiver<()>), ConnectError> {
    let (disconnect_tx, disconnect_rx) = oneshot::channel::<()>();
    let handler = SshClientHandler {
        disconnect_tx: Some(disconnect_tx),
        forward_tx,
    };
    let connect_fut = russh::client::connect(
        ssh_config,
        (hop.host.as_str(), hop.port),
        handler,
    );
    let session = tokio::time::timeout(JUMP_CONNECT_TIMEOUT, connect_fut)
        .await
        .map_err(|_| {
            ConnectError::Retriable(anyhow!(
                "连接跳板机 {}:{} 超时（{}s）",
                hop.host,
                hop.port,
                JUMP_CONNECT_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|e| ConnectError::Retriable(anyhow!("SSH connect failed: {}", e)))?;
    Ok((session, disconnect_rx))
}

/// Open one hop's first (key-auth) connection: direct TCP for the entry hop,
/// a fresh direct-tcpip channel through the already-authenticated prev hop
/// otherwise.
async fn open_hop_connection(
    prev: Option<&russh::client::Handle<SshClientHandler>>,
    hop: &Hop,
    ssh_config: Arc<russh::client::Config>,
    forward_tx: Option<mpsc::UnboundedSender<ForwardedConn>>,
) -> Result<(russh::client::Handle<SshClientHandler>, oneshot::Receiver<()>), ConnectError> {
    match prev {
        None => connect_jump_host(ssh_config, hop, forward_tx).await,
        Some(prev) => open_hop_session(prev, hop, ssh_config, forward_tx).await,
    }
}

/// Open a new SSH session to `hop` through the authenticated `prev`
/// connection: a direct-tcpip channel over `prev`, then a client handshake on
/// that channel stream. The whole open+handshake is bounded by
/// `JUMP_CONNECT_TIMEOUT`; keepalive is on (shared Config) so a later death
/// of this hop is detected by its own connection.
async fn open_hop_session(
    prev: &russh::client::Handle<SshClientHandler>,
    hop: &Hop,
    ssh_config: Arc<russh::client::Config>,
    forward_tx: Option<mpsc::UnboundedSender<ForwardedConn>>,
) -> Result<(russh::client::Handle<SshClientHandler>, oneshot::Receiver<()>), ConnectError> {
    let (disconnect_tx, disconnect_rx) = oneshot::channel::<()>();
    let handler = SshClientHandler {
        disconnect_tx: Some(disconnect_tx),
        forward_tx,
    };
    let host = hop.host.clone();
    let port = hop.port;
    let connect_fut = async move {
        let channel = prev
            .channel_open_direct_tcpip(&host, port as u32, "127.0.0.1", 0)
            .await
            .map_err(|e| {
                ConnectError::Retriable(anyhow!(
                    "open channel to {}:{} failed: {}",
                    host,
                    port,
                    e
                ))
            })?;
        let stream = channel.into_stream();
        russh::client::connect_stream(ssh_config, stream, handler)
            .await
            .map_err(|e| {
                ConnectError::Retriable(anyhow!(
                    "SSH connect through previous hop to {}:{} failed: {}",
                    host,
                    port,
                    e
                ))
            })
    };
    let session = tokio::time::timeout(JUMP_CONNECT_TIMEOUT, connect_fut)
        .await
        .map_err(|_| {
            ConnectError::Retriable(anyhow!(
                "连接跳板 {}:{} 超时（{}s）",
                hop.host,
                hop.port,
                JUMP_CONNECT_TIMEOUT.as_secs()
            ))
        })??;
    Ok((session, disconnect_rx))
}

/// Best-effort disconnect of every hop, farthest first. Closing the far end
/// before the near end avoids the nearer hops logging misleading
/// "connection lost" errors for a teardown we initiated.
async fn disconnect_chain_reverse(chain: &[russh::client::Handle<SshClientHandler>]) {
    for handle in chain.iter().rev() {
        let _ = handle
            .disconnect(russh::Disconnect::ByApplication, "", "en")
            .await;
    }
}

/// User-facing auth-failure text for a hop. Single-hop text is byte-identical
/// to v1.4; multi-hop names the hop that failed (C2).
fn hop_auth_failure_msg(index: usize, total: usize, user: &str, host: &str) -> String {
    if total <= 1 {
        format!("跳板机 {}@{} 认证失败（用户名或密码错误）", user, host)
    } else {
        format!(
            "第 {}/{} 跳 {}@{} 认证失败（用户名或密码错误）",
            index + 1,
            total,
            user,
            host
        )
    }
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

/// Build the `tunnel://password-required` payload. Single-hop (hop=None) has
/// exactly the v1.4 key set; multi-hop adds 1-based `hopIndex`/`hopTotal` so
/// the UI can label which hop is asking (C2).
fn password_payload(
    id: &str,
    prompt: &str,
    layer: &str,
    host: &str,
    need_username: bool,
    hop: Option<(usize, usize)>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "id": id,
        "prompt": prompt,
        "layer": layer,
        "host": host,
        "needUsername": need_username,
    });
    if let Some((index, total)) = hop {
        payload["hopIndex"] = serde_json::json!(index);
        payload["hopTotal"] = serde_json::json!(total);
    }
    payload
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
    hop: Option<(usize, usize)>,
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
    let payload = password_payload(id, &prompt, layer, host, need_username, hop);
    let _ = app.emit("tunnel://password-required", payload);

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
                // Target host (username was prompted): remember the username so
                // the next connection can attempt public-key auth without a prompt.
                if need_username {
                    let _ = store::set_target_user(host, port, &effective_user);
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
///
/// Auth strategy mirrors the jump host: try local private keys first (on their
/// own connection, so a low MaxAuthTries can't starve the password attempt), and
/// only fall back to a password prompt on a fresh connection if every key fails.
/// The public-key attempt needs a username; we use the remembered one (from a
/// prior successful login) or derive a default from the jump username by
/// stripping the "@domain" realm (e.g. "liu.zy@pg.com" → "liu.zy").
async fn setup_second_layer(
    jump: &russh::client::Handle<SshClientHandler>,
    config: &TunnelConfig,
    forward: &crate::model::ForwardSpec,
    jump_user: &str,
    password_senders: &PasswordSenders,
    app: &tauri::AppHandle,
) -> Result<AuthOutcome, ConnectError> {
    let id = &config.id;
    let host = &forward.remote_host;
    let port = forward.remote_port;

    // Open a fresh SSH session to target:port over a new direct-tcpip channel.
    // Each call gets its own MaxAuthTries budget, just like the jump-host phases.
    async fn open_target_session(
        jump: &russh::client::Handle<SshClientHandler>,
        host: &str,
        port: u16,
    ) -> Result<russh::client::Handle<SshClientHandler>, ConnectError> {
        let channel = jump
            .channel_open_direct_tcpip(host, port as u32, "127.0.0.1", 0)
            .await
            .map_err(|e| ConnectError::Retriable(anyhow!("open channel to {}:{} failed: {}", host, port, e)))?;
        let stream = channel.into_stream();
        let ssh_config = Arc::new(russh::client::Config::default());
        let (tx, _rx) = oneshot::channel::<()>();
        let handler = SshClientHandler {
            disconnect_tx: Some(tx),
            forward_tx: None,
        };
        russh::client::connect_stream(ssh_config, stream, handler)
            .await
            .map_err(|e| ConnectError::Retriable(anyhow!("second-layer SSH connect failed: {}", e)))
    }

    // Candidate username for the pubkey attempt: remembered first, else derived
    // from the final hop's username by stripping the "@domain" realm.
    let candidate_user = store::get_target_user(host, port).or_else(|| {
        let ju = jump_user;
        let derived = ju.rsplit_once('@').map(|(u, _)| u).unwrap_or(ju);
        if derived.is_empty() { None } else { Some(derived.to_string()) }
    });

    // Phase 1: public-key auth on its own connection.
    if let Some(user) = candidate_user {
        let mut session = open_target_session(jump, host, port).await?;
        let mut pubkey_authed = false;
        for key_path in key_paths(config.identity_file.as_deref()) {
            let p = std::path::Path::new(&key_path);
            if !p.exists() {
                continue;
            }
            match russh_keys::load_secret_key(p, None) {
                Ok(kp) => {
                    if try_publickey_auth(&mut session, &user, kp, &key_path, "target")
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
        if pubkey_authed {
            let _ = session
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
            return Ok(AuthOutcome { authed: true, pubkey_uploaded: None });
        }
        let _ = session
            .disconnect(russh::Disconnect::ByApplication, "", "en")
            .await;
    }

    // Phase 2: password prompt on a brand-new connection (clean auth budget).
    // Username unknown → prompt (need_username=true). Port fixed at 22.
    let mut session = open_target_session(jump, host, port).await?;
    let outcome = password_phase(
        &mut session,
        None,
        host,
        port,
        "target",
        id,
        password_senders,
        app,
        None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    // ─── tcpip-forward error classification (B3) ─────────────────────────────

    #[test]
    fn request_denied_is_fatal_with_port() {
        match classify_tcpip_forward_err(8080, TcpipForwardErr::Russh(russh::Error::RequestDenied))
        {
            ConnectError::Fatal(msg) => {
                assert!(msg.contains("8080"), "message should name the port: {}", msg)
            }
            ConnectError::Retriable(e) => panic!("expected Fatal, got Retriable: {}", e),
        }
    }

    #[test]
    fn disconnect_error_is_retriable() {
        assert!(matches!(
            classify_tcpip_forward_err(8080, TcpipForwardErr::Russh(russh::Error::Disconnect)),
            ConnectError::Retriable(_)
        ));
    }

    #[test]
    fn timeout_is_retriable() {
        assert!(matches!(
            classify_tcpip_forward_err(8080, TcpipForwardErr::TimedOut),
            ConnectError::Retriable(_)
        ));
    }

    // ─── TaskSet (M1) ────────────────────────────────────────────────────────

    /// Sets an observable flag when the owning task's future is dropped, which
    /// is what aborting a tokio task does.
    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    async fn wait_for_flag(flag: &AtomicBool) -> bool {
        for _ in 0..100 {
            if flag.load(Ordering::SeqCst) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    fn spawn_flagged_task(flag: &Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
        let flag = flag.clone();
        tokio::spawn(async move {
            let _guard = DropFlag(flag);
            std::future::pending::<()>().await;
        })
    }

    #[tokio::test]
    async fn taskset_drop_aborts_registered_tasks() {
        let flag = Arc::new(AtomicBool::new(false));
        {
            let mut tasks = TaskSet::new();
            tasks.push(spawn_flagged_task(&flag));
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert!(
                !flag.load(Ordering::SeqCst),
                "task should still be running before the set is dropped"
            );
        }
        assert!(
            wait_for_flag(&flag).await,
            "dropping the TaskSet must abort its tasks"
        );
    }

    #[tokio::test]
    async fn taskset_abort_all_is_idempotent() {
        let flag = Arc::new(AtomicBool::new(false));
        let mut tasks = TaskSet::new();
        tasks.push(spawn_flagged_task(&flag));
        // Let the task actually start (and construct its Drop guard) so the
        // assertions below observe cancellation, not "never polled".
        tokio::time::sleep(Duration::from_millis(30)).await;
        tasks.abort_all();
        tasks.abort_all();
        assert!(wait_for_flag(&flag).await);
    }

    // ─── hop auth message (C2) ──────────────────────────────────────────────

    #[test]
    fn hop_auth_failure_msg_single_hop_matches_v14_text() {
        assert_eq!(
            hop_auth_failure_msg(0, 1, "u", "h"),
            "跳板机 u@h 认证失败（用户名或密码错误）"
        );
    }

    #[test]
    fn hop_auth_failure_msg_multi_hop_names_the_hop() {
        let msg = hop_auth_failure_msg(1, 3, "u2", "h2");
        assert!(msg.contains("第 2/3 跳"), "msg was: {}", msg);
        assert!(msg.contains("u2@h2"), "msg was: {}", msg);
    }

    // ─── disconnect aggregation (C3) ────────────────────────────────────────

    #[tokio::test]
    async fn forward_first_signal_fires_on_any_hop() {
        let (tx1, rx1) = oneshot::channel::<()>();
        let (_tx2, rx2) = oneshot::channel::<()>();
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(forward_first_signal(vec![rx1, rx2], done_tx));

        tx1.send(()).unwrap();
        let fired = tokio::time::timeout(Duration::from_secs(1), done_rx).await;
        assert!(matches!(fired, Ok(Ok(()))), "aggregate signal should fire");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn forward_first_signal_fires_on_sender_drop() {
        let (tx1, rx1) = oneshot::channel::<()>();
        // Keep the second hop's sender alive so only the dropped one fires.
        let (_tx2, rx2) = oneshot::channel::<()>();
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(forward_first_signal(vec![rx1, rx2], done_tx));

        // Dropping one hop's sender means that hop's handler is gone.
        drop(tx1);
        let fired = tokio::time::timeout(Duration::from_secs(1), done_rx).await;
        assert!(matches!(fired, Ok(Ok(()))), "sender drop should fire signal");
        task.await.unwrap();
    }

    // ─── password payload hop context (C2) ──────────────────────────────────

    #[test]
    fn password_payload_single_hop_has_no_hop_fields() {
        let v = password_payload("id", "prompt", "jump", "host", false, None);

        assert_eq!(v["id"], "id");
        assert_eq!(v["prompt"], "prompt");
        assert_eq!(v["layer"], "jump");
        assert_eq!(v["host"], "host");
        assert_eq!(v["needUsername"], false);
        assert!(v.get("hopIndex").is_none());
        assert!(v.get("hopTotal").is_none());
        // Exact key set matches v1.4 (no extra fields).
        assert_eq!(v.as_object().unwrap().len(), 5);
    }

    #[test]
    fn password_payload_multi_hop_carries_index_and_total() {
        let v = password_payload("id", "prompt", "jump", "h2", false, Some((2, 3)));

        assert_eq!(v["hopIndex"], 2);
        assert_eq!(v["hopTotal"], 3);
        assert_eq!(v["host"], "h2");
    }
}
