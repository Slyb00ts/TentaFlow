// =============================================================================
// File: tentanas/datasets.rs — filesystems and zvols of the node's pools
//       (plan-02 §5.2, tab "Datasety"). One `zfs list -Hp` gives every row of
//       the table; `zfs get -Hp all` gives the properties panel with the
//       source column that tells a local value from an inherited one.
//
//       Sizes are exact bytes because of `-p`; the property strings stay in
//       ZFS spelling ('zstd', '128K', 'off') — the UI shows them verbatim and
//       the helper's allowlist accepts exactly those spellings back.
// =============================================================================

use std::collections::HashMap;

use tentaflow_protocol::tentanas::{NasDataset, NasProperty};

use super::broker::BrokerError;
use super::zfs;

/// `zfs list` columns, in the order the parser expects them.
pub const LIST_COLUMNS: &str = "name,type,mountpoint,mounted,used,avail,refer,quota,volsize,\
                                refreservation,compression,compressratio,recordsize,volblocksize,\
                                atime,sync,encryption,keystatus,creation";

pub fn parse_list(text: &str) -> Vec<NasDataset> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 19 {
                return None;
            }
            let name = f[0].trim().to_string();
            let kind = f[1].trim().to_string();
            let is_volume = kind == "volume";
            let refreservation = zfs::u64_field(f[9]);
            Some(NasDataset {
                pool: name.split('/').next().unwrap_or(&name).to_string(),
                mountpoint: zfs::field(f[2])
                    .filter(|m| *m != "none" && *m != "legacy")
                    .map(str::to_string),
                mounted: zfs::bool_field(f[3]),
                used_bytes: zfs::u64_field(f[4]),
                available_bytes: zfs::u64_field(f[5]),
                referenced_bytes: zfs::u64_field(f[6]),
                // Under `-p` an unset quota is 0, not `-`.
                quota_bytes: Some(zfs::u64_field(f[7])).filter(|v| *v > 0),
                volsize_bytes: is_volume.then(|| zfs::u64_field(f[8])),
                // A zvol with no refreservation is thin: it may allocate less
                // than its volsize promises.
                thin: is_volume && refreservation == 0,
                compression: zfs::field(f[10]).unwrap_or("off").to_string(),
                compression_source: String::new(),
                compress_ratio: zfs::f64_field(f[11]),
                block_size: if is_volume {
                    zfs::field(f[13]).unwrap_or_default().to_string()
                } else {
                    zfs::field(f[12]).unwrap_or_default().to_string()
                },
                atime: zfs::field(f[14]).unwrap_or_default().to_string(),
                sync: zfs::field(f[15]).unwrap_or_default().to_string(),
                encryption: zfs::field(f[16]).unwrap_or("off").to_string(),
                key_status: zfs::field(f[17]).unwrap_or("none").to_string(),
                snapshot_count: 0,
                snapshot_used_bytes: 0,
                snapshot_schedule: None,
                created_at: zfs::field(f[18])
                    .and_then(|v| v.parse::<i64>().ok())
                    .map(zfs::epoch_to_rfc3339),
                kind,
                name,
            })
        })
        .collect()
}

/// `zfs get -Hp <prop> …`: `name<TAB>property<TAB>value<TAB>source`. The
/// source column is what the properties panel shows next to every row, and
/// `inherited from <ancestor>` is split into the two protocol fields.
pub fn parse_get(text: &str) -> Vec<(String, NasProperty)> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 4 {
                return None;
            }
            let raw_source = f[3].trim();
            let (source, inherited_from) = match raw_source {
                "" | "-" => ("none", None),
                s if s.starts_with("inherited from ") => {
                    ("inherited", Some(s["inherited from ".len()..].to_string()))
                }
                s => (s, None),
            };
            Some((
                f[0].trim().to_string(),
                NasProperty {
                    name: f[1].trim().to_string(),
                    value: f[2].trim().to_string(),
                    source: source.to_string(),
                    inherited_from,
                },
            ))
        })
        .collect()
}

/// The `compression` source of every dataset, keyed by dataset name — the one
/// column `zfs list` cannot give, so the table shows "inherited from tank"
/// without a `zfs get` per row.
pub fn parse_compression_sources(text: &str) -> HashMap<String, String> {
    parse_get(text)
        .into_iter()
        .map(|(name, prop)| {
            let label = match (prop.source.as_str(), prop.inherited_from.as_deref()) {
                ("inherited", Some(from)) => format!("inherited from {from}"),
                (source, _) => source.to_string(),
            };
            (name, label)
        })
        .collect()
}

// ----- live reads ------------------------------------------------------------------

/// Every filesystem and volume of the node, or of one pool. Recursive by
/// default — `zfs list` walks the whole tree unless told otherwise.
pub async fn list(pool: &str) -> Result<Vec<NasDataset>, BrokerError> {
    let mut args = vec!["list", "-Hp", "-t", "filesystem,volume", "-o", LIST_COLUMNS];
    if !pool.is_empty() {
        tentanas_helper::validate_pool_name(pool)
            .map_err(|e| BrokerError::InvalidArgument(e.to_string()))?;
        args.extend(["-r", pool]);
    }
    let text = zfs::zfs(&args).await?;
    let mut datasets = parse_list(&text);

    let mut source_args = vec!["get", "-Hp", "-t", "filesystem,volume", "compression"];
    if !pool.is_empty() {
        source_args.extend(["-r", pool]);
    }
    if let Ok(text) = zfs::zfs(&source_args).await {
        let sources = parse_compression_sources(&text);
        for d in datasets.iter_mut() {
            if let Some(s) = sources.get(&d.name) {
                d.compression_source = s.clone();
            }
        }
    }
    Ok(datasets)
}

/// One dataset, or `NotFound` when it does not exist on this node.
pub async fn get(name: &str) -> Result<Option<NasDataset>, BrokerError> {
    tentanas_helper::validate_dataset_name(name)
        .map_err(|e| BrokerError::InvalidArgument(e.to_string()))?;
    let text = match zfs::zfs(&["list", "-Hp", "-d", "0", "-o", LIST_COLUMNS, name]).await {
        Ok(t) => t,
        // `zfs list` of a missing dataset exits 1; that is a not-found, not a
        // broken node.
        Err(BrokerError::Exit { .. }) => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(parse_list(&text).into_iter().next())
}

/// Every property of one dataset, for the properties panel.
pub async fn properties(name: &str) -> Result<Vec<NasProperty>, BrokerError> {
    tentanas_helper::validate_dataset_or_snapshot(name)
        .map_err(|e| BrokerError::InvalidArgument(e.to_string()))?;
    let text = zfs::zfs(&["get", "-Hp", "all", name]).await?;
    let mut props: Vec<NasProperty> = parse_get(&text).into_iter().map(|(_, p)| p).collect();
    props.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(props)
}

/// Pool-level properties for the pool detail's "Właściwości" table.
pub async fn pool_properties(pool: &str) -> Result<Vec<NasProperty>, BrokerError> {
    tentanas_helper::validate_pool_name(pool)
        .map_err(|e| BrokerError::InvalidArgument(e.to_string()))?;
    let text = zfs::zpool(&["get", "-Hp", "all", pool]).await?;
    let mut props: Vec<NasProperty> = parse_get(&text).into_iter().map(|(_, p)| p).collect();
    // The root dataset's properties are what datasets inherit, so the panel
    // shows both sets under one heading.
    if let Ok(text) = zfs::zfs(&["get", "-Hp", "all", pool]).await {
        props.extend(parse_get(&text).into_iter().map(|(_, p)| p));
    }
    props.sort_by(|a, b| a.name.cmp(&b.name));
    props.dedup_by(|a, b| a.name == b.name);
    Ok(props)
}

// ----- jobs -----------------------------------------------------------------------

/// Destroys a dataset or a whole pool and then drops the encryption keys the
/// node kept for it. The keys go only after the command succeeded: a key
/// removed for a destroy that failed would lock the admin out of data that is
/// still there.
pub async fn destroy_job(
    h: super::jobs::JobHandle,
    command: tentanas_helper::HelperCommand,
    addon_id: String,
    subject: String,
    subtree: bool,
    explicit: Option<std::sync::Arc<crate::profiling::collectors::elevation::ElevationToken>>,
) -> anyhow::Result<()> {
    super::jobs::run_step(
        &h,
        &command,
        explicit.as_deref(),
        std::time::Duration::from_secs(15 * 60),
    )
    .await?;
    drop(explicit);
    match super::keystore::forget(&addon_id, &subject, subtree) {
        Ok(0) => {}
        Ok(n) => h.log(format!("dropped {n} encryption keys from the node keystore")),
        Err(e) => h.log(format!("keystore cleanup failed: {e}")),
    }
    h.progress(100);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "tank\tfilesystem\t/mnt/tank\tyes\t23980465111040\t10300000000000\t196608\t0\t-\t0\tzstd\t1.31\t131072\t-\ton\tstandard\toff\tnone\t1756000000\n\
tank/projekty\tfilesystem\t/mnt/tank/projekty\tyes\t18140000000000\t10300000000000\t18100000000000\t27487790694400\t-\t0\tzstd\t1.38\t131072\t-\toff\tstandard\toff\tnone\t1756100000\n\
tank/backups\tfilesystem\t/mnt/tank/backups\tyes\t4508876800000\t10300000000000\t4500000000000\t0\t-\t0\tlz4\t1.21\t131072\t-\toff\tstandard\taes-256-gcm\tavailable\t1756200000\n\
tank/vm-store\tvolume\t-\tno\t1319413953331\t10300000000000\t1319413953331\t0\t2199023255552\t0\tzstd\t1.05\t-\t16384\t-\tstandard\toff\tnone\t1756300000\n";

    #[test]
    fn zfs_list_rows_become_datasets() {
        let rows = parse_list(LIST);
        assert_eq!(rows.len(), 4);

        let root = &rows[0];
        assert_eq!(root.name, "tank");
        assert_eq!(root.pool, "tank");
        assert_eq!(root.kind, "filesystem");
        assert_eq!(root.mountpoint.as_deref(), Some("/mnt/tank"));
        assert!(root.mounted);
        assert_eq!(root.quota_bytes, None);
        assert_eq!(root.volsize_bytes, None);
        assert!(!root.thin);
        assert_eq!(root.block_size, "131072");
        assert_eq!(root.atime, "on");
        assert!((root.compress_ratio - 1.31).abs() < 1e-9);
        assert!(root.created_at.as_deref().is_some_and(|c| c.ends_with('Z')));

        let projekty = &rows[1];
        assert_eq!(projekty.quota_bytes, Some(27_487_790_694_400));
        assert_eq!(projekty.encryption, "off");

        let backups = &rows[2];
        assert_eq!(backups.encryption, "aes-256-gcm");
        assert_eq!(backups.key_status, "available");
        assert_eq!(backups.compression, "lz4");

        let zvol = &rows[3];
        assert_eq!(zvol.kind, "volume");
        assert_eq!(zvol.mountpoint, None);
        assert!(!zvol.mounted);
        assert_eq!(zvol.volsize_bytes, Some(2_199_023_255_552));
        // refreservation 0 on a volume means thin provisioning.
        assert!(zvol.thin);
        assert_eq!(zvol.block_size, "16384");
    }

    #[test]
    fn property_sources_split_the_inherited_ancestor() {
        let text = "tank/projekty\trecordsize\t131072\tlocal\n\
tank/projekty\tcompression\tzstd\tinherited from tank\n\
tank/projekty\tatime\toff\tdefault\n\
tank/projekty\tcreation\t1756100000\t-\n\
tank/projekty\tquota\t27487790694400\treceived\n";
        let props: Vec<NasProperty> = parse_get(text).into_iter().map(|(_, p)| p).collect();
        assert_eq!(props.len(), 5);
        assert_eq!(props[0].source, "local");
        assert_eq!(props[1].source, "inherited");
        assert_eq!(props[1].inherited_from.as_deref(), Some("tank"));
        assert_eq!(props[2].source, "default");
        assert_eq!(props[3].source, "none");
        assert_eq!(props[4].source, "received");

        let sources = parse_compression_sources(
            "tank\tcompression\tzstd\tlocal\ntank/projekty\tcompression\tzstd\tinherited from tank\n",
        );
        assert_eq!(sources["tank"], "local");
        assert_eq!(sources["tank/projekty"], "inherited from tank");
    }

    #[test]
    fn a_short_or_malformed_row_is_skipped_not_guessed() {
        assert!(parse_list("tank\tfilesystem\t/mnt/tank\n").is_empty());
        assert!(parse_get("tank\trecordsize\n").is_empty());
    }
}
