//! Minimal SOCKS5 server-side protocol helpers for `ssh -D` dynamic forwards.
//!
//! Scope (A6): CONNECT only, no authentication, IPv4 / domain / IPv6 address
//! types, BIND and UDP ASSOCIATE rejected. Every function here is pure (no
//! IO) so the protocol edge cases are unit-testable; the accept loop and the
//! SSH channel plumbing live in `serve_dynamic` (wired from tunnel.rs).

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

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
    NetworkUnreachable = 0x03,
    HostUnreachable = 0x04,
    ConnectionRefused = 0x05,
    TtlExpired = 0x06,
    CommandNotSupported = 0x07,
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
}
