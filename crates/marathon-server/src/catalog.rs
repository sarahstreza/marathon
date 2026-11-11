//! The catalog: scanning sources into `items`, and working out which items
//! belong to which channel.

use std::collections::{HashMap, HashSet};

use marathon_api::ScanReport;

use crate::Error;
use crate::config::Config;
use crate::db::{Db, ScanChanges, ScannedItem};
use crate::parse::{ParsedName, parse_path};
use crate::scan::{ScannedFile, SidecarSubtitle, scan_filesystem};
use crate::sources::{Source, Sources};

/// An item a source reports, before it is matched against the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Path within the source, `/`-separated; unique within the source.
    pub path: String,
    pub size: u64,
    /// Modification time in milliseconds since the Unix epoch (0 if
    /// unknown).
    pub mtime_ms: i64,
    pub subtitles: Vec<SidecarSubtitle>,
    /// Show, season, episode, and title, if the source knows them;
    /// otherwise they are parsed from `path`.
    pub metadata: Option<ParsedName>,
    /// The source's own ID for the item, if it has one.
    pub external_id: Option<String>,
    /// The duration, if the source knows it.
    pub duration_ms: Option<i64>,
}

impl From<ScannedFile> for Found {
    fn from(file: ScannedFile) -> Self {
        Self {
            path: file.path,
            size: file.size,
            mtime_ms: file.mtime_ms,
            subtitles: file.subtitles,
            metadata: None,
            external_id: None,
            duration_ms: None,
        }
    }
}

/// Lists what one source holds.
async fn list(id: &str, source: &Source) -> Result<Vec<Found>, String> {
    match source {
        Source::Filesystem { root } => {
            let root = root.clone();
            let started = std::time::Instant::now();
            let result = tokio::task::spawn_blocking(move || scan_filesystem(&root))
                .await
                .map_err(|e| e.to_string())?;
            tracing::debug!(source = %id, elapsed = ?started.elapsed(), "walked source");
            result
                .map(|files| files.into_iter().map(Found::from).collect())
                .map_err(|e| e.to_string())
        }
        Source::Jellyfin(client) => client.scan().await.map_err(|e| e.to_string()),
    }
}

/// Scans every configured source, updates `items`, and rebuilds channel
/// membership.
///
/// A source that cannot be scanned at all (its root is unreadable, or its
/// server unreachable) is reported in [`ScanReport::errors`] and its items
/// are left as they were. Within a source that could be scanned, items
/// that are gone are marked missing and items that come back are unmarked.
pub async fn rescan(db: &Db, config: &Config, sources: &Sources) -> Result<ScanReport, Error> {
    let mut report = ScanReport::default();
    for source in &config.sources {
        let id = source.id().to_owned();
        let Some(runtime) = sources.get(&id) else {
            continue;
        };
        let files = match list(&id, runtime).await {
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
    files: &[Found],
    existing: Vec<crate::db::ItemRow>,
) -> Result<(ScanChanges, ScanReport), Error> {
    let mut by_path: HashMap<String, crate::db::ItemRow> = existing
        .into_iter()
        .map(|row| (row.path.clone(), row))
        .collect();
    let mut changes = ScanChanges::default();
    let mut counts = ScanReport::default();

    for file in files {
        let parsed = file
            .metadata
            .clone()
            .unwrap_or_else(|| parse_path(&file.path));
        let item = ScannedItem {
            path: file.path.clone(),
            size: i64::try_from(file.size).unwrap_or(i64::MAX),
            mtime: file.mtime_ms,
            show: parsed.show,
            season: parsed.season.map(i64::from),
            episode: parsed.episode.map(i64::from),
            title: parsed.title,
            subtitles_json: serde_json::to_string(&file.subtitles)?,
            external_id: file.external_id.clone(),
            duration_ms: file.duration_ms,
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
                    || row.subtitles_json != item.subtitles_json
                    || row.external_id != item.external_id
                    || (item.duration_ms.is_some() && row.duration_ms != item.duration_ms);
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
///
/// Logs a warning for a channel that selects nothing, and for each entry
/// in a selector's `files` that matches no item (most likely a typo, or a
/// file that has been renamed).
pub async fn rebuild_membership(db: &Db, config: &Config) -> Result<(), Error> {
    let items = db.item_paths().await?;
    let mut pairs = Vec::new();
    for channel in &config.channels {
        let mut seen = HashSet::new();
        for selector in channel.selectors() {
            // `Config::validate` rejects selectors with invalid paths.
            let Some(matcher) = selector.matcher() else {
                continue;
            };
            let mut matched = vec![false; matcher.entries().len()];
            for (item_id, source_id, path) in &items {
                if *source_id != selector.source {
                    continue;
                }
                let Some(entry) = matcher.matching_entry(path) else {
                    continue;
                };
                matched[entry] = true;
                if seen.insert(item_id.as_str()) {
                    pairs.push((channel.id.clone(), item_id.clone()));
                }
            }
            if !selector.files.is_empty() {
                for (entry, _) in matcher.entries().iter().zip(&matched).filter(|(_, m)| !**m) {
                    tracing::warn!(
                        channel = %channel.id,
                        source = %selector.source,
                        path = %entry,
                        "channel lists a file that matches no item"
                    );
                }
            }
        }
        if seen.is_empty() && channel.children().next().is_none() {
            tracing::warn!(channel = %channel.id, "channel matches no items");
        }
    }
    db.replace_channel_items(&pairs).await?;
    Ok(())
}
