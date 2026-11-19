use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::model::ForwardSpec;

/// Result of probing a local forward port.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PortStatus {
    /// Nothing listening — we can bind and start normally.
    Free,
    /// Something is listening and traffic flows (a healthy tunnel).
    Working,
    /// Something is listening but the connection dies immediately (a stale/broken tunnel).
    Broken,
}

/// Probe a single local port to decide whether a tunnel is already serving it.
///
/// Heuristic: TCP-connect to 127.0.0.1:port. If connect fails the port is Free.
/// If it connects, do one short read: an immediate EOF/reset means the listener
/// accepts but cannot forward (Broken); a banner or a read timeout (most services
/// wait for the client to speak first) means it is Working.
pub async fn probe_port(port: u16) -> PortStatus {
    let connect = timeout(Duration::from_secs(1), TcpStream::connect(("127.0.0.1", port))).await;

    let mut stream = match connect {
        Ok(Ok(s)) => s,
        _ => return PortStatus::Free,
    };

    let mut buf = [0u8; 1];
    match timeout(Duration::from_millis(600), stream.read(&mut buf)).await {
        // Read timed out: connection still open, no immediate teardown → working.
        Err(_) => PortStatus::Working,
        // Got a banner byte → working.
        Ok(Ok(n)) if n > 0 => PortStatus::Working,
        // Immediate EOF (0 bytes) or read error (RST) → broken.
        Ok(_) => PortStatus::Broken,
    }
}

/// Aggregate probe over all forwards of a tunnel.
///
/// Only rules with a local listener (Local/Dynamic) are probed: Remote rules
/// listen on the server, not here, so they can neither be probed nor mistaken
/// for an externally managed tunnel (B5). If any port is Free we treat the
/// whole tunnel as bindable (Free) and let the normal start path run.
/// Otherwise, all-Working → Working, else Broken.
pub async fn probe_forwards(forwards: &[ForwardSpec]) -> PortStatus {
    let local: Vec<&ForwardSpec> = forwards
        .iter()
        .filter(|f| f.has_local_listener())
        .collect();
    if local.is_empty() {
        // Nothing on this machine to probe (all Remote/Unknown) — never block
        // the app from starting the tunnel and never mark it external.
        return PortStatus::Free;
    }

    let mut all_working = true;
    for f in local {
        match probe_port(f.listen_port()).await {
            PortStatus::Free => return PortStatus::Free,
            PortStatus::Working => {}
            PortStatus::Broken => all_working = false,
        }
    }

    if all_working {
        PortStatus::Working
    } else {
        PortStatus::Broken
    }
}

/// Local ports owned by the given rules. Used before killing whatever holds a
/// "broken" port — a Remote rule's port lives on the server and must never be
/// killed locally (B5).
pub fn local_listen_ports(forwards: &[ForwardSpec]) -> Vec<u16> {
    forwards
        .iter()
        .filter(|f| f.has_local_listener())
        .map(|f| f.listen_port())
        .collect()
}

/// Kill the process(es) currently listening on `port` (macOS `lsof`).
///
/// Used only when an external, broken tunnel is squatting the port and we need to
/// free it before binding. Skips our own PID to avoid self-termination.
pub fn kill_port_listeners(port: u16) {
    let self_pid = std::process::id();

    let output = match std::process::Command::new("lsof")
        .args(["-ti", &format!("tcp:{}", port), "-sTCP:LISTEN"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("lsof failed for port {}: {}", port, e);
            return;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    for pid in stdout.split_whitespace().filter_map(|s| s.parse::<u32>().ok()) {
        if pid == self_pid {
            continue;
        }
        tracing::info!("Killing process {} holding port {}", pid, port);
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ForwardKind;

    fn spec(kind: ForwardKind, port: u16) -> ForwardSpec {
        ForwardSpec {
            local_port: port,
            remote_host: "host".into(),
            remote_port: 2,
            kind,
            remote_bind: None,
        }
    }

    /// A tunnel made only of Remote rules has nothing local to probe: it is
    /// always Free (never marked external, never blocked from starting).
    #[tokio::test]
    async fn probe_forwards_all_remote_is_free() {
        let forwards = [
            spec(ForwardKind::Remote, 8080),
            spec(ForwardKind::Unknown("socks-udp".into()), 9090),
        ];
        assert_eq!(probe_forwards(&forwards).await, PortStatus::Free);
    }

    #[tokio::test]
    async fn probe_forwards_empty_is_free() {
        assert_eq!(probe_forwards(&[]).await, PortStatus::Free);
    }

    #[test]
    fn local_listen_ports_matrix() {
        let forwards = [
            spec(ForwardKind::Local, 10001),
            spec(ForwardKind::Remote, 8080),
            spec(ForwardKind::Dynamic, 1080),
            spec(ForwardKind::Unknown("socks-udp".into()), 9090),
        ];
        assert_eq!(local_listen_ports(&forwards), vec![10001, 1080]);

        let all_remote = [spec(ForwardKind::Remote, 8080)];
        assert!(local_listen_ports(&all_remote).is_empty());
        assert!(local_listen_ports(&[]).is_empty());
    }
}
