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

fn group_order_path() -> PathBuf {
    config_dir().join("group_order.json")
}

fn collapsed_groups_path() -> PathBuf {
    config_dir().join("collapsed_groups.json")
}

/// Groups the user has manually collapsed; absent/corrupt file means every
/// group starts expanded.
pub fn load_collapsed_groups() -> Vec<String> {
    std::fs::read_to_string(collapsed_groups_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the collapsed-group list (deduped, empty entries dropped).
pub fn save_collapsed_groups(groups: &[String]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    let cleaned: Vec<String> = groups
        .iter()
        .filter(|g| !g.is_empty() && seen.insert((*g).clone()))
        .cloned()
        .collect();
    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .context("Failed to create config directory")?;
    let data = serde_json::to_string_pretty(&cleaned)?;
    std::fs::write(collapsed_groups_path(), data)
        .context("Failed to write collapsed_groups.json")?;
    Ok(())
}

/// Saved group display order. Groups missing from the list sort last
/// (alphabetically), so an absent/corrupt file simply falls back to the
/// default alphabetical order.
pub fn load_group_order() -> Vec<String> {
    std::fs::read_to_string(group_order_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the group order list. Entries are trimmed, empty ones dropped and
/// duplicates removed (first occurrence wins) before writing.
pub fn save_group_order(order: &[String]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    let cleaned: Vec<String> = order
        .iter()
        .map(|g| g.trim().to_string())
        .filter(|g| !g.is_empty() && seen.insert(g.clone()))
        .collect();
    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .context("Failed to create config directory")?;
    let data = serde_json::to_string_pretty(&cleaned)?;
    std::fs::write(group_order_path(), data).context("Failed to write group_order.json")?;
    Ok(())
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

/// Account key for a remembered target-host username. Reserved prefix keeps it
/// distinct from password entries (which use "user@host:port").
fn target_user_account(host: &str, port: u16) -> String {
    format!("__targetuser__@{}:{}", host, port)
}

/// The username that last authenticated successfully to a target host, if any.
pub fn get_target_user(host: &str, port: u16) -> Option<String> {
    let account = target_user_account(host, port);
    match keyring::Entry::new(KEYRING_SERVICE, &account) {
        Ok(entry) => entry.get_password().ok(),
        Err(_) => None,
    }
}

/// Remember the username that authenticated to a target host, so the next
/// connection can attempt public-key auth without prompting.
pub fn set_target_user(host: &str, port: u16, user: &str) -> Result<()> {
    let account = target_user_account(host, port);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &account)?;
    entry.set_password(user)?;
    Ok(())
}
