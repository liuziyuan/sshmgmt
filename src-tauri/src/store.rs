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
            "配置目录迁移失败，将以空配置启动（旧数据仍保留）：{e}"
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
            "部分配置文件迁移失败，缺少的配置将以空内容启动。旧数据仍完整保留，可手动将旧目录中的对应文件复制到新目录 {}。失败详情：{}",
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

#[cfg(test)]
mod tests {
    use super::*;

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
