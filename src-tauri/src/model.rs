use serde::{Deserialize, Deserializer, Serialize, Serializer};

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
/// decided per-connection at SOCKS handshake time); `Remote` = `ssh -R`
/// (server-side listener that tunnels connections back to a local target).
///
/// `Unknown` carries a kind tag written by a newer version of the app. It is
/// round-tripped verbatim so upgrade/downgrade cycles never lose data — a
/// forward rule we cannot understand is skipped at runtime, not dropped from
/// the config file.
///
/// Serialization is hand-written on purpose: deriving the external tag would
/// make an unrecognized tag fail the whole config parse (L8).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ForwardKind {
    #[default]
    Local,
    Dynamic,
    Remote,
    Unknown(String),
}

impl Serialize for ForwardKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let tag = match self {
            ForwardKind::Local => "local",
            ForwardKind::Dynamic => "dynamic",
            ForwardKind::Remote => "remote",
            ForwardKind::Unknown(raw) => raw.as_str(),
        };
        serializer.serialize_str(tag)
    }
}

impl<'de> Deserialize<'de> for ForwardKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.as_str() {
            "local" => ForwardKind::Local,
            "dynamic" => ForwardKind::Dynamic,
            "remote" => ForwardKind::Remote,
            _ => ForwardKind::Unknown(raw),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardSpec {
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    /// Rule kind. Absent in JSON written by older versions → Local.
    #[serde(default)]
    pub kind: ForwardKind,
    /// Remote-only: address the server should bind its listener to. `None`
    /// means the default, "localhost" (loopback on the server side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_bind: Option<String>,
}

impl ForwardSpec {
    /// The port this rule listens on. Local and Dynamic listen locally;
    /// Remote listens on the server side (same field, other side of the wire).
    pub fn listen_port(&self) -> u16 {
        self.local_port
    }

    /// Fixed target (host, port). Local: reached through the jump host.
    /// Remote: the local service the server-side listener tunnels back to.
    /// None for Dynamic (target chosen per SOCKS connection) and Unknown.
    // No production caller in v1.4: the -R dispatcher routes by RemoteRoute
    // instead. Kept (with its allow) as the semantics are pinned by tests and
    // it is expected to be needed by v1.5.
    #[allow(dead_code)]
    pub fn target(&self) -> Option<(&str, u16)> {
        match &self.kind {
            ForwardKind::Local | ForwardKind::Remote => {
                Some((self.remote_host.as_str(), self.remote_port))
            }
            ForwardKind::Dynamic | ForwardKind::Unknown(_) => None,
        }
    }

    /// Whether this rule creates a listener on the local machine. Remote and
    /// Unknown rules have no local port to probe or kill.
    pub fn has_local_listener(&self) -> bool {
        matches!(&self.kind, ForwardKind::Local | ForwardKind::Dynamic)
    }

    /// Whether this version knows how to run the rule. Unknown kinds (from a
    /// newer app version) are preserved in configs but skipped at runtime.
    pub fn is_supported(&self) -> bool {
        !matches!(&self.kind, ForwardKind::Unknown(_))
    }

    /// Build a Dynamic rule. `remote_host`/`remote_port` are placeholders
    /// (""/0) carrying no meaning for Dynamic forwards.
    pub fn dynamic(local_port: u16) -> Self {
        Self {
            local_port,
            remote_host: String::new(),
            remote_port: 0,
            kind: ForwardKind::Dynamic,
            remote_bind: None,
        }
    }

    /// Build a Remote rule (`ssh -R`): the server listens on `listen_port`
    /// (0 = let the server pick) and tunnels connections back to the local
    /// `target_host:target_port`. `bind` is the server-side bind address;
    /// None means "localhost".
    pub fn remote(
        listen_port: u16,
        target_host: impl Into<String>,
        target_port: u16,
        bind: Option<String>,
    ) -> Self {
        Self {
            local_port: listen_port,
            remote_host: target_host.into(),
            remote_port: target_port,
            kind: ForwardKind::Remote,
            remote_bind: bind,
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

    fn spec(kind: ForwardKind) -> ForwardSpec {
        ForwardSpec {
            local_port: 1,
            remote_host: "host".into(),
            remote_port: 2,
            kind,
            remote_bind: None,
        }
    }

    #[test]
    fn remote_constructor_and_serde_roundtrip() {
        let f = ForwardSpec::remote(8080, "127.0.0.1", 3000, Some("0.0.0.0".into()));
        assert_eq!(f.kind, ForwardKind::Remote);
        assert_eq!(f.listen_port(), 8080);
        assert_eq!(f.target(), Some(("127.0.0.1", 3000)));
        assert!(!f.has_local_listener());
        assert!(f.is_supported());

        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains(r#""kind":"remote""#), "json was: {}", json);
        assert!(json.contains(r#""remote_bind":"0.0.0.0""#), "json was: {}", json);
        let back: ForwardSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, ForwardKind::Remote);
        assert_eq!(back.remote_bind.as_deref(), Some("0.0.0.0"));
        assert_eq!(back.target(), Some(("127.0.0.1", 3000)));
    }

    /// A kind written by a newer app version must parse and round-trip
    /// verbatim — never fail the whole config (L8).
    #[test]
    fn unknown_kind_is_preserved_verbatim() {
        let json =
            r#"{"local_port":1,"remote_host":"h","remote_port":2,"kind":"socks-udp"}"#;
        let f: ForwardSpec = serde_json::from_str(json).unwrap();
        assert_eq!(f.kind, ForwardKind::Unknown("socks-udp".into()));
        assert!(!f.is_supported());
        assert!(!f.has_local_listener());
        assert_eq!(f.target(), None);

        let out = serde_json::to_string(&f).unwrap();
        assert!(out.contains(r#""kind":"socks-udp""#), "json was: {}", out);
    }

    #[test]
    fn has_local_listener_matrix() {
        assert!(spec(ForwardKind::Local).has_local_listener());
        assert!(spec(ForwardKind::Dynamic).has_local_listener());
        assert!(!spec(ForwardKind::Remote).has_local_listener());
        assert!(!spec(ForwardKind::Unknown("x".into())).has_local_listener());
    }

    #[test]
    fn is_supported_matrix() {
        assert!(spec(ForwardKind::Local).is_supported());
        assert!(spec(ForwardKind::Dynamic).is_supported());
        assert!(spec(ForwardKind::Remote).is_supported());
        assert!(!spec(ForwardKind::Unknown("x".into())).is_supported());
    }

    #[test]
    fn target_matrix() {
        assert_eq!(spec(ForwardKind::Local).target(), Some(("host", 2)));
        assert_eq!(spec(ForwardKind::Remote).target(), Some(("host", 2)));
        assert_eq!(spec(ForwardKind::Dynamic).target(), None);
        assert_eq!(spec(ForwardKind::Unknown("x".into())).target(), None);
    }

    #[test]
    fn remote_bind_omitted_when_none() {
        let f = ForwardSpec::remote(8080, "localhost", 3000, None);
        let json = serde_json::to_string(&f).unwrap();
        assert!(!json.contains("remote_bind"), "json was: {}", json);
        let back: ForwardSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.remote_bind, None);
    }
}
