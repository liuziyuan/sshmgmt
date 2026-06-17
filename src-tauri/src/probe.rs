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
/// If any port is Free we treat the whole tunnel as bindable (Free) and let the
/// normal start path run. Otherwise, all-Working → Working, else Broken.
pub async fn probe_forwards(forwards: &[ForwardSpec]) -> PortStatus {
    if forwards.is_empty() {
        return PortStatus::Free;
    }

    let mut all_working = true;
    for f in forwards {
        match probe_port(f.local_port).await {
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
