use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One SSH hop: the machine we connect through (or, for the last hop, the
/// machine we log into and forward from).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hop {
    pub user: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    pub id: String,
    /// User-defined display name
    pub name: String,
    /// Original ssh command string (for display / re-parse)
    pub raw_command: String,
    /// SSH username (may contain '@', e.g. "liu.zy@pg.com").
    /// Legacy mirror of `hops[0].user`, kept for older app versions.
    pub jump_user: String,
    pub jump_host: String,
    pub jump_port: u16,
    /// Authoritative jump chain. Empty in configs written by older versions;
    /// `ensure_hops` fills it from the legacy mirror at load time.
    #[serde(default)]
    pub hops: Vec<Hop>,
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

impl TunnelConfig {
    /// Reconcile `hops` with the legacy mirror. Returns whether anything
    /// changed (so callers can decide to persist):
    /// - empty hops + a legacy jump host → synthesize a single hop;
    /// - non-empty hops → rewrite the legacy fields to mirror the first hop
    ///   (repairs drift from older versions or hand-edited files).
    ///
    /// Idempotent: a second call on an already-consistent config returns false.
    pub fn ensure_hops(&mut self) -> bool {
        if self.hops.is_empty() {
            if self.jump_host.is_empty() {
                return false;
            }
            self.hops.push(Hop {
                user: self.jump_user.clone(),
                host: self.jump_host.clone(),
                port: self.jump_port,
            });
            return true;
        }

        let first = &self.hops[0];
        if first.user == self.jump_user
            && first.host == self.jump_host
            && first.port == self.jump_port
        {
            return false;
        }
        self.jump_user = first.user.clone();
        self.jump_host = first.host.clone();
        self.jump_port = first.port;
        true
    }

    /// First hop, falling back to the legacy mirror when `hops` is absent.
    /// None only when neither source names a host.
    pub fn first_hop(&self) -> Option<Hop> {
        if let Some(first) = self.hops.first() {
            return Some(first.clone());
        }
        if self.jump_host.is_empty() {
            None
        } else {
            Some(Hop {
                user: self.jump_user.clone(),
                host: self.jump_host.clone(),
                port: self.jump_port,
            })
        }
    }

    /// The full jump chain (hops + final destination). Falls back to a
    /// synthesized single hop for configs without `hops`. Empty when there is
    /// nothing to connect to (callers treat that as a fatal config error).
    pub fn resolved_hops(&self) -> Vec<Hop> {
        if !self.hops.is_empty() {
            self.hops.clone()
        } else {
            self.first_hop().into_iter().collect()
        }
    }
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
    // Still no production caller after v1.5: the -R dispatcher routes by
    // RemoteRoute, and hop handling never needs this helper. Only tests call
    // it (pinning the Local/Remote/Dynamic/Unknown semantics), so the allow
    // stays.
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

/// Structured user-facing message: a stable code plus interpolation params.
/// The backend never renders user-visible text; the frontend locales file
/// translates `code` (key: `msg.<camelCase variant>`) and interpolates
/// `params`. Logs keep using Debug output (English).
/// MUST stay field-identical with `Msg` in src/types.ts (see CLAUDE.md C1).
///
/// Serialization is adjacently tagged (same style as `TunnelState`):
/// `{"code": "bindFailed", "params": {"addr": "...", "detail": "..."}}`.
/// Optional (`Option`) params are omitted from `params` when `None`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "code",
    content = "params",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Msg {
    // ── Tunnel lifecycle / connection errors (tunnel.rs) ──
    NoSupportedForwards,
    MissingJumpConfig,
    ReconnectRequested,
    SshSessionClosed,
    SshChannelError,
    BindFailed { addr: String, detail: String },

    // ── Remote forward -R (tunnel.rs) ──
    RemoteForwardTimeout { port: u16, timeout_secs: u64 },
    RemotePortDenied { port: u16 },
    RemoteForwardFailed { port: u16, detail: String },
    RemotePortOutOfRange { port: u32 },
    RemotePortAllocated { addr: String, port: u16 },

    // ── Connection step errors (tunnel.rs; jump = entry/chain hops,
    //    target = second-layer login) ──
    JumpConnectTimeout { host: String, port: u16, timeout_secs: u64 },
    JumpConnectFailed { detail: String },
    HopChannelOpenFailed { host: String, port: u16, detail: String },
    HopConnectFailed { host: String, port: u16, detail: String },
    HopConnectTimeout { host: String, port: u16, timeout_secs: u64 },
    TargetChannelOpenFailed { host: String, port: u16, detail: String },
    TargetConnectFailed { detail: String },

    // ── Auth (tunnel.rs; classification and the two-phase flow are
    //    unchanged, only the message carrier changes) ──
    JumpKeyAuthError { detail: String },
    JumpPasswordPhaseError { detail: String },
    TargetKeyAuthError { detail: String },
    TargetPasswordPhaseError { detail: String },
    /// Single-hop auth failure carries no hop fields; the multi-hop form
    /// (hop_index/hop_total Some) is rendered from the multi template.
    HopAuthFailed {
        user: String,
        host: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        hop_index: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hop_total: Option<usize>,
    },
    TargetAuthFailed { host: String },

    // ── Success / warning notices (tunnel.rs emit_notice) ──
    PubkeyUploaded {
        host: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        hop_index: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hop_total: Option<usize>,
    },
    PubkeyUploadFailed {
        host: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        hop_index: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hop_total: Option<usize>,
    },
    TargetPubkeyUploaded { host: String },
    TargetPubkeyUploadFailed { host: String },

    // ── Password prompt payloads (tunnel.rs password_payload) ──
    PromptJumpPassword { user: String, host: String },
    PromptTargetLogin { host: String },

    // ── Parser errors (parser.rs) ──
    ParseTokenizeFailed { detail: String },
    ParseFlagNeedsArg { flag: String },
    ParseInvalidPort { value: String },
    ParseNoDestination,
    ParseNoForwards,
    ParseInvalidLocalPort { value: String },
    ParseInvalidRemotePort { value: String },
    ParseInvalidLSpec { spec: String },
    ParseInvalidDSpec { spec: String },
    ParseInvalidRSpec { spec: String },
    ParseInvalidRListenPort { value: String },
    ParseREmptyTarget { spec: String },
    ParseInvalidRTargetPort { value: String },
    ParseJEmptyHop { spec: String },
    ParseJEmptyHost { hop: String },

    // ── Command layer (commands.rs) ──
    TunnelNotFound,
    NoPendingPassword,
    ImportKeychainFailed { name: String, detail: String },
    ImportHopKeychainFailed {
        name: String,
        hop_index: usize,
        hop_total: usize,
        detail: String,
    },
    ImportTargetUserKeychainFailed { name: String, detail: String },
    /// Fallback wrapper for low-level IO/serialization errors at the command
    /// boundary; the frontend also maps non-JSON errors onto this code.
    CommandFailed { detail: String },

    // ── Startup migration warnings (store.rs, pulled via get_startup_warning) ──
    MigrationFailed { legacy_dir: String, detail: String },
    MigrationPartialFailed {
        legacy_dir: String,
        new_dir: String,
        /// Joined by the frontend with the language-specific list separator.
        failures: Vec<String>,
    },

    // ── Import / export (transfer.rs) ──
    ExportSerializeFailed { detail: String },
    ExportWriteFailed { path: String, detail: String },
    ImportReadFailed { path: String, detail: String },
    ImportInvalidJson { detail: String },
    ImportUnknownFormat { format: String, expected: String },
    ImportUnsupportedVersion { version: u32, max_version: u32 },
    ImportEmptyId,
    ImportEmptyName,

    // ── UI support commands (rebuild_tray / set_window_title failure paths) ──
    TrayMissing,
    TrayUpdateFailed { detail: String },
    MainWindowMissing,
    SetTitleFailed { detail: String },
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

    // ─── hops / legacy mirror (C5) ───────────────────────────────────────────

    fn tunnel_config() -> TunnelConfig {
        TunnelConfig {
            id: "id".into(),
            name: "t".into(),
            raw_command: "ssh -L 1:h:2 jump".into(),
            jump_user: "legacy-user".into(),
            jump_host: "legacy-host".into(),
            jump_port: 2222,
            hops: Vec::new(),
            forwards: vec![spec(ForwardKind::Local)],
            bind_all: false,
            identity_file: None,
            auto_reconnect: true,
            group: None,
            environment: None,
        }
    }

    #[test]
    fn legacy_config_without_hops_gets_single_hop() {
        let json = r#"{"id":"a","name":"n","raw_command":"ssh","jump_user":"u",
            "jump_host":"h","jump_port":2200,"forwards":[],"bind_all":false,
            "identity_file":null,"auto_reconnect":true}"#;
        let mut cfg: TunnelConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.hops.is_empty());

        // Runtime resolution already works before migration runs.
        assert_eq!(
            cfg.resolved_hops(),
            vec![Hop { user: "u".into(), host: "h".into(), port: 2200 }]
        );

        assert!(cfg.ensure_hops());
        assert_eq!(cfg.hops.len(), 1);
        assert_eq!(cfg.hops[0].host, "h");
    }

    #[test]
    fn ensure_hops_syncs_legacy_mirror_from_first_hop() {
        let mut cfg = tunnel_config();
        cfg.hops = vec![
            Hop { user: "new-u".into(), host: "new-h".into(), port: 22 },
            Hop { user: "final".into(), host: "final-h".into(), port: 22 },
        ];
        assert!(cfg.ensure_hops());
        assert_eq!(cfg.jump_user, "new-u");
        assert_eq!(cfg.jump_host, "new-h");
        assert_eq!(cfg.jump_port, 22);
    }

    #[test]
    fn ensure_hops_is_idempotent() {
        let mut cfg = tunnel_config();
        assert!(cfg.ensure_hops());
        assert!(!cfg.ensure_hops());
    }

    #[test]
    fn serialization_double_writes_hops_and_first_hop() {
        let mut cfg = tunnel_config();
        cfg.hops = vec![
            Hop { user: "u1".into(), host: "h1".into(), port: 2222 },
            Hop { user: "u2".into(), host: "h2".into(), port: 22 },
        ];
        cfg.ensure_hops();

        let json = serde_json::to_string(&cfg).unwrap();
        assert!(
            json.contains(r#""hops":[{"user":"u1","host":"h1","port":2222}"#),
            "json was: {}",
            json
        );
        assert!(json.contains(r#""jump_host":"h1""#), "json was: {}", json);
    }

    #[test]
    fn first_hop_falls_back_to_legacy_fields() {
        let cfg = tunnel_config();
        assert_eq!(
            cfg.first_hop(),
            Some(Hop { user: "legacy-user".into(), host: "legacy-host".into(), port: 2222 })
        );
    }

    #[test]
    fn resolved_hops_empty_when_nothing_to_synthesize() {
        let mut cfg = tunnel_config();
        cfg.jump_user = String::new();
        cfg.jump_host = String::new();
        cfg.jump_port = 22;
        assert_eq!(cfg.first_hop(), None);
        assert!(cfg.resolved_hops().is_empty());
        assert!(!cfg.ensure_hops());
    }

    // ─── Msg serde shape (adjacently tagged, camelCase) ──────────────────────

    #[test]
    fn msg_unit_variant_serializes_as_code_only() {
        let json = serde_json::to_string(&Msg::NoSupportedForwards).unwrap();
        assert_eq!(json, r#"{"code":"noSupportedForwards"}"#);
        let back: Msg = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Msg::NoSupportedForwards);
    }

    #[test]
    fn msg_parametrized_variant_carries_camel_case_params() {
        let msg = Msg::BindFailed {
            addr: "127.0.0.1:8080".into(),
            detail: "boom".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"code":"bindFailed","params":{"addr":"127.0.0.1:8080","detail":"boom"}}"#
        );
        let back: Msg = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    /// None-valued Option params must be omitted from `params` entirely, so
    /// the single-hop JSON shape stays byte-stable for the frontend.
    #[test]
    fn msg_optional_params_omitted_when_none() {
        let single = Msg::HopAuthFailed {
            user: "u".into(),
            host: "h".into(),
            hop_index: None,
            hop_total: None,
        };
        let json = serde_json::to_string(&single).unwrap();
        assert_eq!(
            json,
            r#"{"code":"hopAuthFailed","params":{"user":"u","host":"h"}}"#
        );

        let multi = Msg::HopAuthFailed {
            user: "u2".into(),
            host: "h2".into(),
            hop_index: Some(2),
            hop_total: Some(3),
        };
        let json = serde_json::to_string(&multi).unwrap();
        assert_eq!(
            json,
            r#"{"code":"hopAuthFailed","params":{"user":"u2","host":"h2","hopIndex":2,"hopTotal":3}}"#
        );
        let back: Msg = serde_json::from_str(&json).unwrap();
        assert_eq!(back, multi);
    }
}
