use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::model::TunnelConfig;

const KEYRING_SERVICE: &str = "sshmgmt";

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("sshmgmt")
}

fn tunnels_path() -> PathBuf {
    config_dir().join("tunnels.json")
}

pub fn load_tunnels() -> Vec<TunnelConfig> {
    let path = tunnels_path();
    if !path.exists() {
        return Vec::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(e) => {
            tracing::warn!("Failed to read tunnels.json: {}", e);
            Vec::new()
        }
    }
}

pub fn save_tunnels(tunnels: &[TunnelConfig]) -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .context("Failed to create config directory")?;
    let path = tunnels_path();
    let data = serde_json::to_string_pretty(tunnels)?;
    std::fs::write(&path, data).context("Failed to write tunnels.json")?;
    Ok(())
}

/// Account key for keychain: "user@host:port"
fn account_key(user: &str, host: &str, port: u16) -> String {
    format!("{}@{}:{}", user, host, port)
}

pub fn get_password(user: &str, host: &str, port: u16) -> Option<String> {
    let account = account_key(user, host, port);
    match keyring::Entry::new(KEYRING_SERVICE, &account) {
        Ok(entry) => entry.get_password().ok(),
        Err(_) => None,
    }
}

pub fn set_password(user: &str, host: &str, port: u16, password: &str) -> Result<()> {
    let account = account_key(user, host, port);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &account)?;
    entry.set_password(password)?;
    Ok(())
}

pub fn delete_password(user: &str, host: &str, port: u16) -> Result<()> {
    let account = account_key(user, host, port);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &account)?;
    entry.delete_credential()?;
    Ok(())
}
