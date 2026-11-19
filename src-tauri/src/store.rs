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

/// Parse the persisted tunnel list tolerantly: a single corrupt entry must not
/// discard the rest of the file (a whole-list fallback would silently drop
/// every tunnel and then persist that loss on the next save). A file that is
/// not a JSON array at all still falls back to an empty list — same as before.
fn parse_tunnels(data: &str) -> Vec<TunnelConfig> {
    let values: Vec<serde_json::Value> = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to parse tunnels.json, starting with an empty list: {}", e);
            return Vec::new();
        }
    };
    let total = values.len();
    let tunnels: Vec<TunnelConfig> = values
        .into_iter()
        .filter_map(|v| match serde_json::from_value::<TunnelConfig>(v) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!("Skipping invalid tunnel entry: {}", e);
                None
            }
        })
        .collect();
    if tunnels.len() != total {
        tracing::warn!(
            "Loaded {} of {} tunnel entries (corrupt entries skipped)",
            tunnels.len(),
            total
        );
    }
    tunnels
}

pub fn load_tunnels() -> Vec<TunnelConfig> {
    let path = tunnels_path();
    if !path.exists() {
        return Vec::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(data) => parse_tunnels(&data),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ForwardKind;
    use serde_json::json;

    fn valid_tunnel(id: &str) -> serde_json::Value {
        json!({
            "id": id,
            "name": "t",
            "raw_command": "ssh -L 1:h:2 j",
            "jump_user": "u",
            "jump_host": "j",
            "jump_port": 22,
            "forwards": [{"local_port": 1, "remote_host": "h", "remote_port": 2}],
            "bind_all": false,
            "identity_file": null,
            "auto_reconnect": true
        })
    }

    /// L8: one corrupt entry is skipped, the rest of the list survives.
    #[test]
    fn parse_tunnels_keeps_valid_entries_when_one_is_bad() {
        let data = json!([
            valid_tunnel("a"),
            {"id": "broken"},
            valid_tunnel("b")
        ])
        .to_string();

        let tunnels = parse_tunnels(&data);
        assert_eq!(tunnels.len(), 2);
        assert_eq!(tunnels[0].id, "a");
        assert_eq!(tunnels[1].id, "b");
    }

    #[test]
    fn parse_tunnels_preserves_unknown_kind() {
        let mut t = valid_tunnel("a");
        t["forwards"] = json!([{
            "local_port": 1,
            "remote_host": "h",
            "remote_port": 2,
            "kind": "socks-udp"
        }]);
        let tunnels = parse_tunnels(&json!([t]).to_string());

        assert_eq!(tunnels.len(), 1);
        assert_eq!(
            tunnels[0].forwards[0].kind,
            ForwardKind::Unknown("socks-udp".into())
        );
    }

    #[test]
    fn parse_tunnels_rejects_non_array() {
        assert!(parse_tunnels(r#"{"not":"an array"}"#).is_empty());
        assert!(parse_tunnels("not json").is_empty());
    }

    #[test]
    fn parse_tunnels_reads_legacy_without_kind() {
        let tunnels = parse_tunnels(&json!([valid_tunnel("a")]).to_string());

        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].forwards[0].kind, ForwardKind::Local);
        assert_eq!(tunnels[0].forwards[0].remote_bind, None);
    }
}
