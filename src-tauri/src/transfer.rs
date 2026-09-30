//! One-shot export/import of all tunnel configs (plus keychain secrets).
//!
//! The export file is a self-contained JSON document. Secrets are embedded in
//! PLAINTEXT by explicit user consent — treat the file like a password.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::TunnelConfig;
use crate::store;

pub const EXPORT_FORMAT: &str = "sshmgmt-tunnels";
pub const EXPORT_VERSION: u32 = 1;

/// Keychain-derived secrets for one tunnel. `None` means "not exported" —
/// never "delete the local one": the importer only writes `Some` values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TunnelSecret {
    pub password: Option<String>,
    pub target_user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedTunnel {
    pub config: TunnelConfig,
    #[serde(default)]
    pub secret: TunnelSecret,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportFile {
    pub format: String,
    pub version: u32,
    pub exported_at: u64,
    pub tunnels: Vec<ExportedTunnel>,
    /// Saved group display order. Optional (older export files lack it);
    /// absent means "no custom ordering".
    #[serde(default)]
    pub group_order: Option<Vec<String>>,
}

/// Assemble the export payload, pulling secrets from the keychain.
/// Keychain read failures degrade to `None` (store.rs already handles that),
/// so a locked/unavailable keychain never blocks an export.
pub fn build_export(tunnels: &[TunnelConfig]) -> ExportFile {
    let exported_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let tunnels = tunnels
        .iter()
        .map(|c| ExportedTunnel {
            secret: TunnelSecret {
                password: store::get_password(&c.jump_user, &c.jump_host, c.jump_port),
                target_user: store::get_target_user(&c.jump_host, c.jump_port),
            },
            config: c.clone(),
        })
        .collect();
    ExportFile {
        format: EXPORT_FORMAT.to_string(),
        version: EXPORT_VERSION,
        exported_at,
        tunnels,
        group_order: {
            let order = store::load_group_order();
            if order.is_empty() { None } else { Some(order) }
        },
    }
}

/// Serialize and write the export file (pretty JSON so users can inspect it).
pub fn write_export(path: &Path, file: &ExportFile) -> Result<()> {
    let data = serde_json::to_string_pretty(file).context("Failed to serialize export")?;
    std::fs::write(path, data)
        .with_context(|| format!("Failed to write export file {:?}", path))?;
    Ok(())
}

/// Read and validate an import file. Rejects unknown formats, versions newer
/// than what this build understands, and entries with empty id/name.
pub fn read_import(path: &Path) -> Result<ExportFile> {
    let data = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read import file {:?}", path))?;
    let file: ExportFile =
        serde_json::from_str(&data).context("Import file is not valid JSON")?;
    if file.format != EXPORT_FORMAT {
        bail!(
            "Unknown export format {:?} (expected {:?})",
            file.format,
            EXPORT_FORMAT
        );
    }
    if file.version > EXPORT_VERSION {
        bail!(
            "Unsupported export file version {} (this build supports up to {})",
            file.version,
            EXPORT_VERSION
        );
    }
    for t in &file.tunnels {
        if t.config.id.trim().is_empty() {
            bail!("Import file contains a tunnel with an empty id");
        }
        if t.config.name.trim().is_empty() {
            bail!("Import file contains a tunnel with an empty name");
        }
    }
    Ok(file)
}

/// Split incoming tunnels into (to_import, skipped).
///
/// A tunnel conflicts when its id OR its name matches an existing config:
/// name is the identity users recognize across machines, while the id check
/// makes re-importing the same file on the same machine idempotent.
/// Skipped tunnels are left completely untouched (secret included).
pub fn split_new(
    existing: &[TunnelConfig],
    incoming: &[ExportedTunnel],
) -> (Vec<ExportedTunnel>, Vec<ExportedTunnel>) {
    let mut to_import = Vec::new();
    let mut skipped = Vec::new();
    for t in incoming {
        let conflict = existing
            .iter()
            .any(|e| e.id == t.config.id || e.name == t.config.name);
        if conflict {
            skipped.push(t.clone());
        } else {
            to_import.push(t.clone());
        }
    }
    (to_import, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ForwardSpec;

    fn config(id: &str, name: &str) -> TunnelConfig {
        TunnelConfig {
            id: id.to_string(),
            name: name.to_string(),
            raw_command: "ssh -L 5432:db:5432 jump".into(),
            jump_user: "user".into(),
            jump_host: "jump".into(),
            jump_port: 22,
            forwards: vec![ForwardSpec {
                local_port: 5432,
                remote_host: "db".into(),
                remote_port: 5432,
            }],
            bind_all: false,
            identity_file: None,
            auto_reconnect: true,
            group: None,
            environment: None,
        }
    }

    fn exported(id: &str, name: &str) -> ExportedTunnel {
        ExportedTunnel {
            config: config(id, name),
            secret: TunnelSecret::default(),
        }
    }

    #[test]
    fn split_new_passes_when_no_conflict() {
        let existing = vec![config("a", "alpha")];
        let incoming = vec![exported("b", "beta")];
        let (to_import, skipped) = split_new(&existing, &incoming);
        assert_eq!(to_import.len(), 1);
        assert!(skipped.is_empty());
    }

    #[test]
    fn split_new_skips_on_name_conflict() {
        // Same name, different id: two machines created "the same" tunnel.
        let existing = vec![config("a", "alpha")];
        let incoming = vec![exported("z", "alpha")];
        let (to_import, skipped) = split_new(&existing, &incoming);
        assert!(to_import.is_empty());
        assert_eq!(skipped.len(), 1);
    }

    #[test]
    fn split_new_skips_on_id_conflict() {
        // Same id, different name: re-importing on the same machine.
        let existing = vec![config("a", "alpha")];
        let incoming = vec![exported("a", "renamed")];
        let (to_import, skipped) = split_new(&existing, &incoming);
        assert!(to_import.is_empty());
        assert_eq!(skipped.len(), 1);
    }

    #[test]
    fn export_file_roundtrips_without_secret() {
        let json = r#"{
            "format": "sshmgmt-tunnels",
            "version": 1,
            "exported_at": 1759180800,
            "tunnels": [
                { "config": {
                    "id": "a", "name": "alpha", "raw_command": "ssh",
                    "jump_user": "u", "jump_host": "h", "jump_port": 22,
                    "forwards": [], "bind_all": false,
                    "identity_file": null, "auto_reconnect": false
                } }
            ]
        }"#;
        let file: ExportFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.format, EXPORT_FORMAT);
        assert_eq!(file.tunnels.len(), 1);
        assert!(file.tunnels[0].secret.password.is_none());
        assert!(file.tunnels[0].secret.target_user.is_none());
        // group_order is optional and defaults to None on older files.
        assert!(file.group_order.is_none());

        // Round-trip: serialize back and re-parse.
        let back: ExportFile = serde_json::from_str(&serde_json::to_string(&file).unwrap())
            .unwrap();
        assert_eq!(back.tunnels[0].config.id, "a");
    }

    #[test]
    fn export_file_roundtrips_with_group_order() {
        let json = r#"{
            "format": "sshmgmt-tunnels",
            "version": 1,
            "exported_at": 1759180800,
            "tunnels": [],
            "group_order": ["GI", "CD"]
        }"#;
        let file: ExportFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.group_order, Some(vec!["GI".to_string(), "CD".to_string()]));

        let back: ExportFile = serde_json::from_str(&serde_json::to_string(&file).unwrap())
            .unwrap();
        assert_eq!(back.group_order, Some(vec!["GI".to_string(), "CD".to_string()]));
    }

    #[test]
    fn read_import_rejects_wrong_format() {
        let dir = std::env::temp_dir().join("sshmgmt-transfer-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wrong-format.json");
        std::fs::write(
            &path,
            r#"{"format": "something-else", "version": 1, "exported_at": 0, "tunnels": []}"#,
        )
        .unwrap();
        assert!(read_import(&path).is_err());
    }

    #[test]
    fn read_import_rejects_future_version() {
        let dir = std::env::temp_dir().join("sshmgmt-transfer-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("future-version.json");
        std::fs::write(
            &path,
            r#"{"format": "sshmgmt-tunnels", "version": 99, "exported_at": 0, "tunnels": []}"#,
        )
        .unwrap();
        assert!(read_import(&path).is_err());
    }
}
