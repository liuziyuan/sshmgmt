//! Minimal SOCKS5 server-side protocol helpers for `ssh -D` dynamic forwards.
//!
//! Scope (A6): CONNECT only, no authentication, IPv4 / domain / IPv6 address
//! types, BIND and UDP ASSOCIATE rejected. Every function here is pure (no
//! IO) so the protocol edge cases are unit-testable; the accept loop and the
//! SSH channel plumbing live in `serve_dynamic` (wired from tunnel.rs).

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as TokioMutex, Notify};

/// SOCKS protocol version this server speaks.
const SOCKS_VERSION: u8 = 0x05;

/// Auth method "no authentication required".
const METHOD_NO_AUTH: u8 = 0x00;

/// Result of parsing the client greeting (`VER NMETHODS METHODS...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Greeting {
    /// Not enough bytes yet — read more and retry.
    NeedMore,
    /// Client offered no method we support (we only accept 0x00).
    NoAcceptableAuth,
    /// Client offered 0x00; proceed to the request phase.
    Ready,
}

/// Parse a SOCKS5 client greeting. A non-0x05 version is treated as
/// "no acceptable auth" — we reply `05 FF` and close, which is what a
/// version-mismatched client can actually interpret.
pub fn parse_greeting(buf: &[u8]) -> Greeting {
    if buf.len() < 2 {
        return Greeting::NeedMore;
    }
    if buf[0] != SOCKS_VERSION {
        return Greeting::NoAcceptableAuth;
    }
    let n = buf[1] as usize;
    if buf.len() < 2 + n {
        return Greeting::NeedMore;
    }
    if buf[2..2 + n].contains(&METHOD_NO_AUTH) {
        Greeting::Ready
    } else {
        Greeting::NoAcceptableAuth
    }
}

/// Build the method-selection reply (`VER METHOD`).
pub fn method_reply(acceptable: bool) -> [u8; 2] {
    [SOCKS_VERSION, if acceptable { METHOD_NO_AUTH } else { 0xFF }]
}

/// SOCKS command requested by the client. Only `Connect` is served; the
/// other two are parsed so the caller can reject them with a proper REP code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocksCommand {
    Connect,
    Bind,
    UdpAssociate,
}

/// Destination address as sent by the client. Domains are kept **verbatim**
/// (never resolved locally) so the jump host performs the DNS lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocksHost {
    V4(Ipv4Addr),
    Domain(String),
    V6(Ipv6Addr),
}

impl fmt::Display for SocksHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SocksHost::V4(a) => write!(f, "{}", a),
            // Raw domain, no local resolution.
            SocksHost::Domain(d) => write!(f, "{}", d),
            SocksHost::V6(a) => write!(f, "{}", a),
        }
    }
}

/// A fully parsed CONNECT/BIND/UDP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocksRequest {
    pub command: SocksCommand,
    pub host: SocksHost,
    pub port: u16,
}

/// Result of parsing a SOCKS5 request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    NeedMore,
    Invalid(&'static str),
    Ok(SocksRequest),
}

/// Parse a SOCKS5 request (`VER CMD RSV ATYP DST.ADDR DST.PORT`).
pub fn parse_request(buf: &[u8]) -> Request {
    if buf.len() < 4 {
        return Request::NeedMore;
    }
    if buf[0] != SOCKS_VERSION {
        return Request::Invalid("bad version");
    }
    let command = match buf[1] {
        0x01 => SocksCommand::Connect,
        0x02 => SocksCommand::Bind,
        0x03 => SocksCommand::UdpAssociate,
        _ => return Request::Invalid("unknown command"),
    };
    if buf[2] != 0x00 {
        return Request::Invalid("bad reserved byte");
    }

    let (host, addr_len) = match buf[3] {
        0x01 => {
            if buf.len() < 4 + 4 + 2 {
                return Request::NeedMore;
            }
            let mut octets = [0u8; 4];
            octets.copy_from_slice(&buf[4..8]);
            (SocksHost::V4(Ipv4Addr::from(octets)), 4)
        }
        0x04 => {
            if buf.len() < 4 + 16 + 2 {
                return Request::NeedMore;
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[4..20]);
            (SocksHost::V6(Ipv6Addr::from(octets)), 16)
        }
        0x03 => {
            if buf.len() < 5 {
                return Request::NeedMore;
            }
            let len = buf[4] as usize;
            if buf.len() < 5 + len + 2 {
                return Request::NeedMore;
            }
            let domain = match std::str::from_utf8(&buf[5..5 + len]) {
                Ok(d) => d.to_string(),
                Err(_) => return Request::Invalid("domain is not valid utf-8"),
            };
            if domain.is_empty() {
                return Request::Invalid("empty domain");
            }
            (SocksHost::Domain(domain), 1 + len)
        }
        _ => return Request::Invalid("bad address type"),
    };

    let port_at = 4 + addr_len;
    let port = u16::from_be_bytes([buf[port_at], buf[port_at + 1]]);
    Request::Ok(SocksRequest { command, host, port })
}

/// SOCKS5 reply codes (RFC 1928 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Reply {
    Succeeded = 0x00,
    GeneralFailure = 0x01,
    NotAllowed = 0x02,
    // The four codes below are part of the protocol surface but never emitted
    // by this implementation (targets are resolved by the jump host, so local
    // network/host/TTL conditions do not arise, and unsupported address types
    // are reported as general failure). Kept so the enum mirrors RFC 1928.
    #[allow(dead_code)]
    NetworkUnreachable = 0x03,
    #[allow(dead_code)]
    HostUnreachable = 0x04,
    ConnectionRefused = 0x05,
    #[allow(dead_code)]
    TtlExpired = 0x06,
    CommandNotSupported = 0x07,
    #[allow(dead_code)]
    AddressTypeNotSupported = 0x08,
}

/// Build a request reply with an all-zero BND.ADDR / BND.PORT (ATYP = IPv4).
pub fn reply(rep: Reply) -> [u8; 10] {
    let mut out = [0u8; 10];
    out[0] = SOCKS_VERSION;
    out[1] = rep as u8;
    out[2] = 0x00;
    out[3] = 0x01; // BND.ADDR is 0.0.0.0, BND.PORT is 0
    out
}

/// Map an SSH channel-open error to a SOCKS reply code. Recognized variants
/// get a specific code; everything else falls back to general failure and is
/// logged, so a refused channel always produces a readable reply byte.
pub fn russh_error_to_reply(err: &russh::Error) -> Reply {
    use russh::ChannelOpenFailure;

    match err {
        russh::Error::RequestDenied => Reply::NotAllowed,
        russh::Error::ChannelOpenFailure(cause) => match cause {
            ChannelOpenFailure::AdministrativelyProhibited => Reply::NotAllowed,
            ChannelOpenFailure::ConnectFailed => Reply::ConnectionRefused,
            ChannelOpenFailure::ResourceShortage => Reply::GeneralFailure,
            ChannelOpenFailure::UnknownChannelType => Reply::CommandNotSupported,
            ChannelOpenFailure::Unknown => Reply::GeneralFailure,
        },
        other => {
            tracing::warn!("SOCKS direct-tcpip open failed: {}", other);
            Reply::GeneralFailure
        }
    }
}

/// How a failed `direct-tcpip` channel open should be handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelErrorKind {
    /// The SSH session is healthy; only this one target refused/failed.
    /// Fail the SOCKS request, never the tunnel.
    Target,
    /// The SSH session itself looks dead; the tunnel should reconnect.
    Session,
}

/// Classify a `channel_open_direct_tcpip` error.
///
/// A server-issued `CHANNEL_OPEN_FAILURE` (target refused, admin-prohibited,
/// resource shortage, unknown channel type) means the target is unreachable
/// or refused — the session is fine. Notifying the tunnel task in that case
/// would tear down every listener and force a reconnect (or a red light when
/// auto-reconnect is off) for a routine per-request failure such as a closed
/// target port, which is exactly the S1 review finding.
///
/// Only explicitly recognized session-level errors notify. Anything not
/// recognized defaults to `Target`: session death is independently detected
/// by the handler's Drop signal, so a missed notification cannot leave a dead
/// session unnoticed, while a false notification would kill a healthy tunnel.
pub fn classify_channel_error(err: &russh::Error) -> ChannelErrorKind {
    match err {
        russh::Error::Disconnect | russh::Error::SendError | russh::Error::HUP => {
            ChannelErrorKind::Session
        }
        _ => ChannelErrorKind::Target,
    }
}

// ─── Serving dynamic forwards ────────────────────────────────────────────────

/// Time budget for a client to finish the greeting + request handshake.
/// Port scanners and half-open proxy checks connect and say nothing; without
/// this they would pin a task (and a socket) per probe indefinitely.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Accept loop for one `-D` listener. Each client is served on its own task;
/// only a session-level channel error signals `session_error` — a target-level
/// failure (server refused that destination) fails just the SOCKS request.
///
/// Generic over the russh handler so this module stays decoupled from
/// tunnel.rs. The listener task is spawned by the caller outside the
/// establishment race and aborted in its cleanup list.
pub async fn serve_dynamic<H>(
    listener: TcpListener,
    session: Arc<TokioMutex<russh::client::Handle<H>>>,
    session_error: Arc<Notify>,
) where
    H: russh::client::Handler + 'static,
    H::Error: 'static,
{
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("SOCKS accept() error: {}", e);
                break;
            }
        };
        tracing::debug!("SOCKS connection from {}", peer);

        let session = session.clone();
        let notify = session_error.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, &session, &notify).await {
                tracing::debug!("SOCKS client {} ended: {}", peer, e);
            }
        });
    }
}

/// Drive one SOCKS5 client: greeting, request, channel open, then pump bytes.
async fn handle_client<H>(
    mut tcp: TcpStream,
    session: &Arc<TokioMutex<russh::client::Handle<H>>>,
    session_error: &Notify,
) -> Result<()>
where
    H: russh::client::Handler,
{
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;

    // Phase 1: greeting. Only the no-auth method is acceptable.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        match parse_greeting(&buf) {
            Greeting::NeedMore => {}
            Greeting::NoAcceptableAuth => {
                tcp.write_all(&method_reply(false)).await?;
                return Ok(());
            }
            Greeting::Ready => break,
        }
        let n = read_some(&mut tcp, &mut chunk, deadline).await?;
        if n == 0 {
            return Ok(()); // client closed before finishing the greeting
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    tcp.write_all(&method_reply(true)).await?;

    // Drop exactly the greeting bytes; any request bytes that arrived in the
    // same read (a client that pipelines) stay in the buffer.
    let greeting_len = 2 + buf[1] as usize;
    buf.drain(..greeting_len);

    // Phase 2: request. Keep reading until a complete request is buffered.
    let request = loop {
        match parse_request(&buf) {
            Request::NeedMore => {}
            other => break other,
        }
        let n = read_some(&mut tcp, &mut chunk, deadline).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let request = match request {
        Request::Ok(req) => req,
        Request::Invalid(reason) => {
            tracing::debug!("SOCKS request invalid: {}", reason);
            tcp.write_all(&reply(Reply::GeneralFailure)).await?;
            return Ok(());
        }
        Request::NeedMore => unreachable!("loop exits only on a non-NeedMore result"),
    };

    if request.command != SocksCommand::Connect {
        tracing::debug!("SOCKS command {:?} not supported", request.command);
        tcp.write_all(&reply(Reply::CommandNotSupported)).await?;
        return Ok(());
    }

    // Phase 3: open the direct-tcpip channel. Hold the session lock only for
    // the open call (same granularity as the -L path). The domain is passed
    // through verbatim so the jump host resolves it.
    let target = request.host.to_string();
    let open_result = {
        let sess = session.lock().await;
        sess.channel_open_direct_tcpip(&target, request.port as u32, "127.0.0.1", 0)
            .await
    };

    match open_result {
        Ok(channel) => {
            tcp.write_all(&reply(Reply::Succeeded)).await?;
            let mut ssh_stream = channel.into_stream();
            copy_bidirectional(&mut tcp, &mut ssh_stream).await?;
        }
        Err(e) => {
            tracing::warn!("SOCKS channel to {}:{} failed: {}", target, request.port, e);
            let _ = tcp.write_all(&reply(russh_error_to_reply(&e))).await;
            // A refused/unreachable target is not a dead session: fail only
            // this request. Only recognized session-level errors wake the
            // tunnel task (the -L path keeps its existing notify unchanged).
            if classify_channel_error(&e) == ChannelErrorKind::Session {
                session_error.notify_one();
            }
        }
    }

    Ok(())
}

/// Read into `buf`, bounded by the shared handshake `deadline`.
async fn read_some(tcp: &mut TcpStream, buf: &mut [u8], deadline: Instant) -> Result<usize> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        anyhow::bail!("SOCKS handshake timeout");
    }
    match tokio::time::timeout(remaining, tcp.read(buf)).await {
        Ok(r) => Ok(r?),
        Err(_) => anyhow::bail!("SOCKS handshake timeout"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain_request(domain: &str, port: u16) -> Vec<u8> {
        let mut buf = vec![0x05, 0x01, 0x00, 0x03, domain.len() as u8];
        buf.extend_from_slice(domain.as_bytes());
        buf.extend_from_slice(&port.to_be_bytes());
        buf
    }

    #[test]
    fn greeting_need_more_until_methods_arrive() {
        assert_eq!(parse_greeting(&[]), Greeting::NeedMore);
        assert_eq!(parse_greeting(&[0x05]), Greeting::NeedMore);
        assert_eq!(parse_greeting(&[0x05, 0x02, 0x00]), Greeting::NeedMore);
    }

    #[test]
    fn greeting_ready_when_no_auth_offered() {
        assert_eq!(parse_greeting(&[0x05, 0x01, 0x00]), Greeting::Ready);
        assert_eq!(parse_greeting(&[0x05, 0x03, 0x02, 0x00, 0x01]), Greeting::Ready);
    }

    #[test]
    fn greeting_rejects_without_no_auth() {
        assert_eq!(
            parse_greeting(&[0x05, 0x02, 0x01, 0x02]),
            Greeting::NoAcceptableAuth
        );
        // Wrong protocol version is rejected the same way.
        assert_eq!(
            parse_greeting(&[0x04, 0x01, 0x00]),
            Greeting::NoAcceptableAuth
        );
    }

    #[test]
    fn method_reply_bytes() {
        assert_eq!(method_reply(true), [0x05, 0x00]);
        assert_eq!(method_reply(false), [0x05, 0xFF]);
    }

    #[test]
    fn request_ipv4() {
        let buf = [0x05, 0x01, 0x00, 0x01, 10, 0, 0, 7, 0x1F, 0x90];
        assert_eq!(
            parse_request(&buf),
            Request::Ok(SocksRequest {
                command: SocksCommand::Connect,
                host: SocksHost::V4(Ipv4Addr::new(10, 0, 0, 7)),
                port: 8080,
            })
        );
    }

    #[test]
    fn request_domain_is_kept_verbatim() {
        let buf = domain_request("db.internal", 3306);
        match parse_request(&buf) {
            Request::Ok(req) => {
                assert_eq!(req.command, SocksCommand::Connect);
                assert_eq!(req.host, SocksHost::Domain("db.internal".to_string()));
                assert_eq!(req.host.to_string(), "db.internal");
                assert_eq!(req.port, 3306);
            }
            other => panic!("expected parsed request, got {:?}", other),
        }
    }

    #[test]
    fn request_ipv6() {
        let mut buf = vec![0x05, 0x01, 0x00, 0x04];
        buf.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        buf.extend_from_slice(&22u16.to_be_bytes());
        match parse_request(&buf) {
            Request::Ok(req) => {
                assert_eq!(req.host, SocksHost::V6(Ipv6Addr::LOCALHOST));
                assert_eq!(req.host.to_string(), "::1");
                assert_eq!(req.port, 22);
            }
            other => panic!("expected parsed request, got {:?}", other),
        }
    }

    #[test]
    fn request_short_buffers_need_more() {
        assert_eq!(parse_request(&[0x05, 0x01]), Request::NeedMore);
        // IPv4 header without the full address + port.
        assert_eq!(
            parse_request(&[0x05, 0x01, 0x00, 0x01, 10, 0, 0]),
            Request::NeedMore
        );
        // Domain length announced but body missing.
        assert_eq!(
            parse_request(&[0x05, 0x01, 0x00, 0x03, 0x04, b'a', b'b']),
            Request::NeedMore
        );
        // IPv6 without a full address + port.
        assert_eq!(
            parse_request(&[0x05, 0x01, 0x00, 0x04, 0, 0, 0, 0]),
            Request::NeedMore
        );
    }

    #[test]
    fn request_bad_version_or_atyp_is_invalid() {
        assert_eq!(
            parse_request(&[0x04, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0]),
            Request::Invalid("bad version")
        );
        assert_eq!(
            parse_request(&[0x05, 0x01, 0x00, 0x02, 0, 0, 0, 0, 0, 0]),
            Request::Invalid("bad address type")
        );
    }

    #[test]
    fn request_bind_and_udp_are_parsed_for_rejection() {
        let bind = [0x05, 0x02, 0x00, 0x01, 127, 0, 0, 1, 0, 80];
        match parse_request(&bind) {
            Request::Ok(req) => assert_eq!(req.command, SocksCommand::Bind),
            other => panic!("expected parsed request, got {:?}", other),
        }
        let udp = [0x05, 0x03, 0x00, 0x01, 127, 0, 0, 1, 0, 80];
        match parse_request(&udp) {
            Request::Ok(req) => assert_eq!(req.command, SocksCommand::UdpAssociate),
            other => panic!("expected parsed request, got {:?}", other),
        }
    }

    #[test]
    fn reply_bytes_and_codes() {
        assert_eq!(
            reply(Reply::Succeeded),
            [0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(reply(Reply::CommandNotSupported)[1], 0x07);
        assert_eq!(reply(Reply::AddressTypeNotSupported)[1], 0x08);
    }

    #[test]
    fn russh_errors_map_to_socks_replies() {
        use russh::ChannelOpenFailure;

        assert_eq!(
            russh_error_to_reply(&russh::Error::RequestDenied),
            Reply::NotAllowed
        );
        assert_eq!(
            russh_error_to_reply(&russh::Error::ChannelOpenFailure(
                ChannelOpenFailure::AdministrativelyProhibited
            )),
            Reply::NotAllowed
        );
        assert_eq!(
            russh_error_to_reply(&russh::Error::ChannelOpenFailure(
                ChannelOpenFailure::ConnectFailed
            )),
            Reply::ConnectionRefused
        );
        assert_eq!(
            russh_error_to_reply(&russh::Error::ChannelOpenFailure(
                ChannelOpenFailure::UnknownChannelType
            )),
            Reply::CommandNotSupported
        );
        // Unrecognized variants fall back to general failure.
        assert_eq!(
            russh_error_to_reply(&russh::Error::Disconnect),
            Reply::GeneralFailure
        );
    }

    #[test]
    fn channel_open_failure_is_target_level_not_session_death() {
        use russh::ChannelOpenFailure;

        // Every server-issued CHANNEL_OPEN_FAILURE means "this target refused
        // or is unreachable" — the SSH session must survive (S1 regression).
        for cause in [
            ChannelOpenFailure::ConnectFailed,
            ChannelOpenFailure::AdministrativelyProhibited,
            ChannelOpenFailure::ResourceShortage,
            ChannelOpenFailure::UnknownChannelType,
            ChannelOpenFailure::Unknown,
        ] {
            assert_eq!(
                classify_channel_error(&russh::Error::ChannelOpenFailure(cause)),
                ChannelErrorKind::Target,
                "cause: {:?}",
                cause
            );
        }
    }

    #[test]
    fn session_level_errors_wake_the_tunnel() {
        for err in [
            russh::Error::Disconnect,
            russh::Error::SendError,
            russh::Error::HUP,
        ] {
            assert_eq!(
                classify_channel_error(&err),
                ChannelErrorKind::Session,
                "err: {}",
                err
            );
        }
    }

    #[test]
    fn unrecognized_channel_errors_default_to_target_level() {
        // Fail only the request; real session death is still caught by the
        // handler's Drop signal, so a missed wake-up cannot hide a dead session.
        assert_eq!(
            classify_channel_error(&russh::Error::Version),
            ChannelErrorKind::Target
        );
    }
}
