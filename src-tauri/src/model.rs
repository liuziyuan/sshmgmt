use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    pub id: String,
    /// User-defined display name
    pub name: String,
    /// Original ssh command string (for display / re-parse)
    pub raw_command: String,
    /// SSH username (may contain '@', e.g. "liu.zy@pg.com")
    pub jump_user: String,
    pub jump_host: String,
    pub jump_port: u16,
    pub forwards: Vec<ForwardSpec>,
    /// -g flag: bind listener on 0.0.0.0 instead of 127.0.0.1
    pub bind_all: bool,
    /// -i flag: path to identity file
    pub identity_file: Option<String>,
    pub auto_reconnect: bool,
    /// Optional project/group name
    #[serde(default)]
    pub group: Option<String>,
    /// Optional environment label (e.g. dev, staging, prod)
    #[serde(default)]
    pub environment: Option<String>,
}

/// Kind of a forwarding rule. `Local` = classic `ssh -L` (fixed target through
/// the jump host); `Dynamic` = `ssh -D` (local SOCKS5 proxy whose target is
/// decided per-connection at SOCKS handshake time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ForwardKind {
    #[default]
    Local,
    Dynamic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardSpec {
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    /// Rule kind. Absent in JSON written by older versions → Local.
    #[serde(default)]
    pub kind: ForwardKind,
}

impl ForwardSpec {
    /// The port this rule listens on locally. Local and Dynamic both listen
    /// here; the placeholder remote fields are meaningless for Dynamic.
    pub fn listen_port(&self) -> u16 {
        self.local_port
    }

    /// Fixed target (host, port) reached through the jump host. None for
    /// Dynamic — the target is chosen by each SOCKS client at handshake time.
    // Reserved for the -R dispatch path (v1.4); v1.3 has no producer call
    // site yet, tests pin the semantics.
    #[allow(dead_code)]
    pub fn target(&self) -> Option<(&str, u16)> {
        match self.kind {
            ForwardKind::Local => Some((self.remote_host.as_str(), self.remote_port)),
            ForwardKind::Dynamic => None,
        }
    }

    /// Build a Dynamic rule. `remote_host`/`remote_port` are placeholders
    /// (""/0) carrying no meaning for Dynamic forwards.
    pub fn dynamic(local_port: u16) -> Self {
        Self {
            local_port,
            remote_host: String::new(),
            remote_port: 0,
            kind: ForwardKind::Dynamic,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "message")]
pub enum TunnelState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    Failed(String),
    PasswordRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelInfo {
    pub config: TunnelConfig,
    pub state: TunnelState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicKeyInfo {
    /// Absolute path to the .pub file
    pub path: String,
    /// File name only (e.g. "id_ed25519.pub")
    pub name: String,
    /// Full public key text (type base64 comment)
    pub content: String,
    /// Whether the paired private key exists next to it
    pub has_private: bool,
}

#[derive(Debug)]
pub struct PasswordResponse {
    pub password: String,
    pub save: bool,
    /// Username entered by the user (second-layer target host login).
    /// None for the jump host (username comes from config).
    pub username: Option<String>,
    /// Path to the local .pub file to upload after auth. None = don't upload.
    pub pubkey_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Legacy JSON without a `kind` field must read back as Local — existing
    /// tunnels keep behaving exactly as before the upgrade.
    #[test]
    fn legacy_json_without_kind_reads_local() {
        let json = r#"{"local_port":10008,"remote_host":"db.internal","remote_port":3306}"#;
        let f: ForwardSpec = serde_json::from_str(json).unwrap();
        assert_eq!(f.kind, ForwardKind::Local);
        assert_eq!(f.listen_port(), 10008);
        assert_eq!(f.target(), Some(("db.internal", 3306)));
    }

    #[test]
    fn dynamic_constructor_sets_placeholders() {
        let f = ForwardSpec::dynamic(1080);
        assert_eq!(f.kind, ForwardKind::Dynamic);
        assert_eq!(f.local_port, 1080);
        assert_eq!(f.remote_host, "");
        assert_eq!(f.remote_port, 0);
        // Dynamic has no fixed target; it still listens on its own port.
        assert_eq!(f.target(), None);
        assert_eq!(f.listen_port(), 1080);
    }

    #[test]
    fn kind_serializes_lowercase_and_roundtrips() {
        let f = ForwardSpec::dynamic(1080);
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains(r#""kind":"dynamic""#), "json was: {}", json);
        let back: ForwardSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, ForwardKind::Dynamic);
    }
}
