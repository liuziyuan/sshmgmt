use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::model::TunnelConfig;

const KEYRING_SERVICE: &str = "sshmgmt";

// Live data dir name. LEGACY_DIR_NAME is a read-only migration source that is
// never written to; it only exists so upgrades keep working.
pub const APP_DIR_NAME: &str = "portunnel";
pub const LEGACY_DIR_NAME: &str = "sshmgmt";
const MIGRATED_FILES: [&str; 3] = ["tunnels.json", "group_order.json", "collapsed_groups.json"];

fn base_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
}

fn config_dir() -> PathBuf {
    base_dir().join(APP_DIR_NAME)
}

/// Legacy data dir; only `migrate_legacy_data_dir` may reference it.
fn legacy_config_dir() -> PathBuf {
    base_dir().join(LEGACY_DIR_NAME)
}

/// One-shot legacy → new data-dir migration (spec C1).
/// MUST run before any `create_dir_all(config_dir())`: an early write would
/// create the new dir and the three-state check would never fire again (P1).
pub fn migrate_legacy_data_dir() -> Option<String> {
    // Derive the base from the legacy path so the migration source and the
    // live dir share one derivation (no hardcoded absolute paths, P9).
    let legacy_dir = legacy_config_dir();
    let base = legacy_dir.parent()?;
    migrate_between(base, LEGACY_DIR_NAME, APP_DIR_NAME)
}

/// Pure, testable core. States:
///   new dir exists             -> None (use new dir; legacy untouched)
///   new missing, legacy exists -> copy the three JSON files (copy, NOT move)
///   both missing               -> None
/// Any failure -> Some(warning message), keep going (empty-config startup).
pub(crate) fn migrate_between(base: &Path, legacy_name: &str, new_name: &str) -> Option<String> {
    let new_dir = base.join(new_name);
    if new_dir.exists() {
        return None;
    }
    let legacy_dir = base.join(legacy_name);
    if !legacy_dir.exists() {
        return None;
    }
    if let Err(e) = std::fs::create_dir_all(&new_dir) {
        return Some(format!(
            "配置目录迁移失败，将以空配置启动（旧数据仍保留在 {}）：{e}",
            legacy_dir.display()
        ));
    }
    let mut failures = Vec::new();
    for file in MIGRATED_FILES {
        let src = legacy_dir.join(file);
        if !src.exists() {
            continue;
        }
        let dst = new_dir.join(file);
        if let Err(e) = std::fs::copy(&src, &dst) {
            failures.push(format!("{file}: {e}"));
        }
    }
    if failures.is_empty() {
        None
    } else {
        Some(format!(
            "部分配置文件迁移失败，缺少的配置将以空内容启动。旧数据仍完整保留在 {}，可手动将其中的对应文件复制到新目录 {}。失败详情：{}",
            legacy_dir.display(),
            new_dir.display(),
            failures.join("；")
        ))
    }
}

static STARTUP_WARNING: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Stash the migration warning for the frontend to pull on mount. First write
/// wins; repeated sets are ignored.
pub fn set_startup_warning(msg: String) {
    let _ = STARTUP_WARNING.set(msg);
}

pub fn startup_warning() -> Option<String> {
    STARTUP_WARNING.get().cloned()
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

    /// Fresh, uniquely named temp dir per test: repeatable and parallel-safe.
    fn temp_base(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "portunnel-migrate-tests-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed_legacy(base: &Path, files: &[(&str, &str)]) {
        let legacy = base.join(LEGACY_DIR_NAME);
        std::fs::create_dir_all(&legacy).unwrap();
        for (name, body) in files {
            std::fs::write(legacy.join(name), body).unwrap();
        }
    }

    const TUNNELS: &str = r#"[{"id":"a"}]"#;
    const ORDER: &str = r#"["GI","CD"]"#;
    const COLLAPSED: &str = r#"["GI"]"#;

    #[test]
    fn legacy_copied_and_preserved() {
        let base = temp_base("copy");
        seed_legacy(
            &base,
            &[
                ("tunnels.json", TUNNELS),
                ("group_order.json", ORDER),
                ("collapsed_groups.json", COLLAPSED),
            ],
        );

        assert!(migrate_between(&base, LEGACY_DIR_NAME, APP_DIR_NAME).is_none());

        let new_dir = base.join(APP_DIR_NAME);
        let legacy_dir = base.join(LEGACY_DIR_NAME);
        for name in MIGRATED_FILES {
            assert_eq!(
                std::fs::read_to_string(new_dir.join(name)).unwrap(),
                std::fs::read_to_string(legacy_dir.join(name)).unwrap(),
                "{name} should be byte-identical in the new dir"
            );
            // Copy, not move: the legacy file still exists and is unchanged.
            assert!(legacy_dir.join(name).exists(), "{name} must stay in place");
        }
        assert_eq!(
            std::fs::read_to_string(legacy_dir.join("tunnels.json")).unwrap(),
            TUNNELS
        );
    }

    #[test]
    fn fresh_install_noop() {
        let base = temp_base("fresh");

        assert!(migrate_between(&base, LEGACY_DIR_NAME, APP_DIR_NAME).is_none());
        assert!(!base.join(APP_DIR_NAME).exists());
        assert!(!base.join(LEGACY_DIR_NAME).exists());
    }

    #[test]
    fn new_present_noop() {
        let base = temp_base("new-present");
        seed_legacy(&base, &[("tunnels.json", TUNNELS)]);
        let new_dir = base.join(APP_DIR_NAME);
        std::fs::create_dir_all(&new_dir).unwrap();

        assert!(migrate_between(&base, LEGACY_DIR_NAME, APP_DIR_NAME).is_none());
        // An existing (even empty) new dir always wins; legacy is untouched.
        assert!(!new_dir.join("tunnels.json").exists());
        assert_eq!(
            std::fs::read_to_string(base.join(LEGACY_DIR_NAME).join("tunnels.json")).unwrap(),
            TUNNELS
        );
    }

    #[test]
    fn rerun_after_migration_noop() {
        let base = temp_base("rerun");
        seed_legacy(
            &base,
            &[
                ("tunnels.json", TUNNELS),
                ("group_order.json", ORDER),
                ("collapsed_groups.json", COLLAPSED),
            ],
        );

        assert!(migrate_between(&base, LEGACY_DIR_NAME, APP_DIR_NAME).is_none());
        let new_dir = base.join(APP_DIR_NAME);
        // Simulate data written after the first migration (e.g. a new tunnel).
        let marker = new_dir.join("marker.txt");
        std::fs::write(&marker, "post-migration").unwrap();
        std::fs::write(new_dir.join("tunnels.json"), r#"[{"id":"new"}]"#).unwrap();

        assert!(migrate_between(&base, LEGACY_DIR_NAME, APP_DIR_NAME).is_none());
        // The second run must not copy legacy data over the new dir.
        assert_eq!(
            std::fs::read_to_string(new_dir.join("tunnels.json")).unwrap(),
            r#"[{"id":"new"}]"#
        );
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            "post-migration"
        );
        assert_eq!(
            std::fs::read_to_string(base.join(LEGACY_DIR_NAME).join("tunnels.json")).unwrap(),
            TUNNELS
        );
    }

    #[test]
    fn partial_failure_warns() {
        let base = temp_base("partial");
        let legacy_dir = base.join(LEGACY_DIR_NAME);
        std::fs::create_dir_all(&legacy_dir).unwrap();
        // A directory where a file is expected makes fs::copy fail.
        std::fs::create_dir_all(legacy_dir.join("tunnels.json")).unwrap();
        std::fs::write(legacy_dir.join("group_order.json"), ORDER).unwrap();
        std::fs::write(legacy_dir.join("collapsed_groups.json"), COLLAPSED).unwrap();

        let warning = migrate_between(&base, LEGACY_DIR_NAME, APP_DIR_NAME)
            .expect("partial failure must return a warning instead of panicking");
        assert!(warning.contains("tunnels.json"), "warning was: {warning}");

        // Best-effort: the other two files are still copied.
        let new_dir = base.join(APP_DIR_NAME);
        assert_eq!(
            std::fs::read_to_string(new_dir.join("group_order.json")).unwrap(),
            ORDER
        );
        assert_eq!(
            std::fs::read_to_string(new_dir.join("collapsed_groups.json")).unwrap(),
            COLLAPSED
        );
    }
}
