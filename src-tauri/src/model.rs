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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardSpec {
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
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

#[derive(Debug)]
pub struct PasswordResponse {
    pub password: String,
    pub save: bool,
}
