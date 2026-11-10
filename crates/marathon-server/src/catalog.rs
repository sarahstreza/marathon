//! The catalog: scanning sources into `items`, and working out which items
//! belong to which channel.

use std::collections::{HashMap, HashSet};

use marathon_api::ScanReport;

use crate::Error;
use crate::config::{Config, SourceConfig, expand_home};
use crate::db::{Db, ScanChanges, ScannedItem};
use crate::parse::parse_path;
use crate::scan::{ScannedFile, scan_filesystem};

/// Scans every configured source, updates `items`, and rebuilds channel
/// membership.
///
/// A source that cannot be scanned at all (its root is unreadable) is
/// reported in [`ScanReport::errors`] and its items are left as they were.
/// Within a readable source, files that are gone are marked missing and
/// files that come back are unmarked.
pub async fn rescan(db: &Db, config: &Config) -> Result<ScanReport, Error> {
    let mut report = ScanReport::default();
    for source in &config.sources {
        let id = source.id().to_owned();
        let files = match source {
            SourceConfig::Filesystem { root, .. } => {
                let root = expand_home(root);
                let started = std::time::Instant::now();
                let result = tokio::task::spawn_blocking(move || scan_filesystem(&root)).await?;
                tracing::debug!(source = %id, elapsed = ?started.elapsed(), "walked source");
                result
            }
        };
        let files = match files {
            Ok(files) => files,
            Err(err) => {
                tracing::warn!(source = %id, error = %err, "skipping source that cannot be scanned");
                report.errors.push(format!("source {id}: {err}"));
                continue;
            }
        };
        let existing = db.source_items(&id).await?;
        let (changes, counts) = diff(&files, existing)?;
        db.apply_scan(&id, &changes).await?;
        tracing::info!(
            source = %id,
            present = counts.items_present,
            added = counts.added,
            updated = counts.updated,
            newly_missing = counts.newly_missing,
            restored = counts.restored,
            "scanned source"
        );
        report.sources_scanned += 1;
        report.items_present += counts.items_present;
        report.added += counts.added;
        report.updated += counts.updated;
        report.newly_missing += counts.newly_missing;
        report.restored += counts.restored;
    }
    rebuild_membership(db, config).await?;
    Ok(report)
}

/// Works out the database changes that bring one source's items in line
/// with a scan.
fn diff(
    files: &[ScannedFile],
    existing: Vec<crate::db::ItemRow>,
) -> Result<(ScanChanges, ScanReport), Error> {
    let mut by_path: HashMap<String, crate::db::ItemRow> = existing
        .into_iter()
        .map(|row| (row.path.clone(), row))
        .collect();
    let mut changes = ScanChanges::default();
    let mut counts = ScanReport::default();

    for file in files {
        let parsed = parse_path(&file.path);
        let item = ScannedItem {
            path: file.path.clone(),
            size: i64::try_from(file.size).unwrap_or(i64::MAX),
            mtime: file.mtime_ms,
            show: parsed.show,
            season: parsed.season.map(i64::from),
            episode: parsed.episode.map(i64::from),
            title: parsed.title,
            subtitles_json: serde_json::to_string(&file.subtitles)?,
        };
        counts.items_present += 1;
        match by_path.remove(&file.path) {
            None => {
                counts.added += 1;
                changes
                    .insert
                    .push((uuid::Uuid::now_v7().to_string(), item));
            }
            Some(row) => {
                let file_changed = row.size != item.size || row.mtime != item.mtime;
                let metadata_changed = row.show != item.show
                    || row.season != item.season
                    || row.episode != item.episode
                    || row.title != item.title
                    || row.subtitles_json != item.subtitles_json;
                if row.missing {
                    counts.restored += 1;
                } else if file_changed || metadata_changed {
                    counts.updated += 1;
                } else {
                    continue;
                }
                changes.update.push((row.id, item, file_changed));
            }
        }
    }

    // Whatever is left was not found by this scan.
    for row in by_path.into_values() {
        if !row.missing {
            counts.newly_missing += 1;
            changes.mark_missing.push(row.id);
        }
    }
    Ok((changes, counts))
}

/// Recomputes `channel_items` from every channel's selectors.
pub async fn rebuild_membership(db: &Db, config: &Config) -> Result<(), Error> {
    let items = db.item_paths().await?;
    let mut pairs = Vec::new();
    for channel in &config.channels {
        let mut seen = HashSet::new();
        for selector in &channel.items {
            for (item_id, source_id, path) in &items {
                if *source_id == selector.source
                    && selector.matches(path)
                    && seen.insert(item_id.as_str())
                {
                    pairs.push((channel.id.clone(), item_id.clone()));
                }
            }
        }
        if seen.is_empty() {
            tracing::warn!(channel = %channel.id, "channel matches no items");
        }
    }
    db.replace_channel_items(&pairs).await?;
    Ok(())
}
