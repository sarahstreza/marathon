//! SQLite storage: connection setup, migrations, and the queries the rest of
//! the server uses. Schema: `migrations/` (and PLAN.md §7).
//!
//! Queries are runtime-checked (`sqlx::query` / `query_as`), not the
//! compile-time macros, so building needs no database.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use sqlx::FromRow;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

/// The schema migrations, embedded at compile time from `migrations/`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// Errors from opening the database.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("failed to open database {path}")]
    Connect {
        path: String,
        #[source]
        source: sqlx::Error,
    },
    #[error("failed to migrate database {path}")]
    Migrate {
        path: String,
        #[source]
        source: sqlx::migrate::MigrateError,
    },
}

/// A handle to the database. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Db {
    pool: SqlitePool,
}

/// A row of `items`.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct ItemRow {
    pub id: String,
    pub source_id: String,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub show: Option<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub title: String,
    pub duration_ms: Option<i64>,
    pub subtitles_json: String,
    pub missing: bool,
    /// The source's own ID for the item (Jellyfin's item ID); `None` for
    /// filesystem items.
    pub external_id: Option<String>,
}

/// A row of `channels`.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct ChannelRow {
    pub id: String,
    pub number: i64,
    pub name: String,
    pub rule_json: String,
    pub selectors_json: String,
}

/// A row of `channel_state`.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct StateRow {
    pub channel_id: String,
    pub item_id: Option<String>,
    pub position_ms: i64,
    pub cursor_json: String,
    pub updated_at: i64,
    /// The states an independent parent keeps for its descendants (PLAN.md
    /// M11), as a JSON object; `{}` otherwise.
    pub children_json: String,
}

/// A row of `events`.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct EventRow {
    pub id: String,
    pub ts: i64,
    pub client_id: Option<String>,
    pub channel_id: Option<String>,
    pub item_id: Option<String>,
    pub kind: String,
    pub position_ms: Option<i64>,
    pub resolved_channel_id: Option<String>,
    pub resolution_path_json: Option<String>,
}

/// A new event to append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent<'a> {
    pub ts: i64,
    pub client_id: Option<&'a str>,
    /// The channel the viewer was tuned to.
    pub channel_id: &'a str,
    /// The channel `item_id` belongs to (`channel_id`, or a nested child).
    pub resolved_channel_id: &'a str,
    /// JSON array of the channel IDs from `channel_id` to
    /// `resolved_channel_id`.
    pub resolution_path_json: &'a str,
    pub item_id: &'a str,
    pub kind: &'a str,
    pub position_ms: i64,
}

/// The columns of an item that a scan writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedItem {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub show: Option<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub title: String,
    pub subtitles_json: String,
    /// The source's own ID for the item, if it has one.
    pub external_id: Option<String>,
    /// The duration, if the source knows it (Jellyfin does). Overwrites
    /// whatever a client reported; `None` leaves the stored value alone.
    pub duration_ms: Option<i64>,
}

/// What a scan changes for one source, applied in one transaction.
#[derive(Debug, Default)]
pub struct ScanChanges {
    /// New items: (new ID, columns).
    pub insert: Vec<(String, ScannedItem)>,
    /// Known items to rewrite and mark present: (ID, columns, whether the
    /// file itself changed, which clears its duration).
    pub update: Vec<(String, ScannedItem, bool)>,
    /// IDs of items to mark missing.
    pub mark_missing: Vec<String>,
}

/// A source or channel definition to mirror from config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDef {
    pub id: String,
    pub kind: String,
    pub config_json: String,
}

const ITEM_COLUMNS: &str = "id, source_id, path, size, mtime, show, season, episode, title, \
                            duration_ms, subtitles_json, missing, external_id";

impl Db {
    /// Opens (creating if needed) the database at `path` and runs any
    /// pending migrations. Safe to call on an up-to-date database.
    pub async fn open(path: &Path) -> Result<Self, OpenError> {
        let display = path.display().to_string();
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        Self::connect(options, display).await
    }

    /// Opens a private in-memory database, for tests.
    pub async fn open_in_memory() -> Result<Self, OpenError> {
        let display = ":memory:".to_owned();
        let options = SqliteConnectOptions::from_str("sqlite::memory:").map_err(|source| {
            OpenError::Connect {
                path: display.clone(),
                source,
            }
        })?;
        Self::connect(options, display).await
    }

    async fn connect(options: SqliteConnectOptions, display: String) -> Result<Self, OpenError> {
        // An in-memory database exists per connection, so keep exactly one;
        // a file database gets a few for concurrent reads.
        let in_memory = display == ":memory:";
        let pool = SqlitePoolOptions::new()
            .max_connections(if in_memory { 1 } else { 4 })
            .min_connections(if in_memory { 1 } else { 0 })
            .idle_timeout(if in_memory {
                None
            } else {
                Some(Duration::from_secs(600))
            })
            .max_lifetime(if in_memory {
                None
            } else {
                Some(Duration::from_secs(3600))
            })
            .connect_with(options)
            .await
            .map_err(|source| OpenError::Connect {
                path: display.clone(),
                source,
            })?;
        MIGRATOR
            .run(&pool)
            .await
            .map_err(|source| OpenError::Migrate {
                path: display,
                source,
            })?;
        Ok(Self { pool })
    }

    /// Closes all connections, waiting for in-flight queries.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// The underlying pool, for tests and ad-hoc queries.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // --- Config mirroring -------------------------------------------------

    /// Makes `sources` and `channels` match config: upserts every given
    /// definition and deletes the rest. Channel state and history are left
    /// alone, so a channel whose rule changes keeps its position.
    pub async fn sync_definitions(
        &self,
        sources: &[SourceDef],
        channels: &[ChannelRow],
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM sources").execute(&mut *tx).await?;
        for source in sources {
            sqlx::query("INSERT INTO sources (id, kind, config_json) VALUES (?, ?, ?)")
                .bind(&source.id)
                .bind(&source.kind)
                .bind(&source.config_json)
                .execute(&mut *tx)
                .await?;
        }

        let keep: Vec<&str> = channels.iter().map(|c| c.id.as_str()).collect();
        let existing: Vec<(String,)> = sqlx::query_as("SELECT id FROM channels")
            .fetch_all(&mut *tx)
            .await?;
        for (id,) in existing {
            if !keep.contains(&id.as_str()) {
                sqlx::query("DELETE FROM channels WHERE id = ?")
                    .bind(&id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM channel_items WHERE channel_id = ?")
                    .bind(&id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        for channel in channels {
            sqlx::query(
                "INSERT INTO channels (id, number, name, rule_json, selectors_json)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT (id) DO UPDATE SET
                    number = excluded.number,
                    name = excluded.name,
                    rule_json = excluded.rule_json,
                    selectors_json = excluded.selectors_json",
            )
            .bind(&channel.id)
            .bind(channel.number)
            .bind(&channel.name)
            .bind(&channel.rule_json)
            .bind(&channel.selectors_json)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }

    pub async fn channels(&self) -> Result<Vec<ChannelRow>, sqlx::Error> {
        sqlx::query_as(
            "SELECT id, number, name, rule_json, selectors_json FROM channels ORDER BY number, id",
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn channel(&self, id: &str) -> Result<Option<ChannelRow>, sqlx::Error> {
        sqlx::query_as(
            "SELECT id, number, name, rule_json, selectors_json FROM channels WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }

    // --- Items ------------------------------------------------------------

    pub async fn item(&self, id: &str) -> Result<Option<ItemRow>, sqlx::Error> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {ITEM_COLUMNS} FROM items WHERE id = ?"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Every item of a source, including missing ones.
    pub async fn source_items(&self, source_id: &str) -> Result<Vec<ItemRow>, sqlx::Error> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {ITEM_COLUMNS} FROM items WHERE source_id = ?"
        )))
        .bind(source_id)
        .fetch_all(&self.pool)
        .await
    }

    /// `(id, source_id, path)` of every item, including missing ones.
    pub async fn item_paths(&self) -> Result<Vec<(String, String, String)>, sqlx::Error> {
        sqlx::query_as("SELECT id, source_id, path FROM items")
            .fetch_all(&self.pool)
            .await
    }

    /// Applies one source's scan results.
    pub async fn apply_scan(
        &self,
        source_id: &str,
        changes: &ScanChanges,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        for (id, item) in &changes.insert {
            sqlx::query(
                "INSERT INTO items (id, source_id, path, size, mtime, show, season, episode,
                                    title, subtitles_json, external_id, duration_ms, missing)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
            )
            .bind(id)
            .bind(source_id)
            .bind(&item.path)
            .bind(item.size)
            .bind(item.mtime)
            .bind(&item.show)
            .bind(item.season)
            .bind(item.episode)
            .bind(&item.title)
            .bind(&item.subtitles_json)
            .bind(&item.external_id)
            .bind(item.duration_ms)
            .execute(&mut *tx)
            .await?;
        }
        for (id, item, file_changed) in &changes.update {
            sqlx::query(
                "UPDATE items SET size = ?, mtime = ?, show = ?, season = ?, episode = ?,
                    title = ?, subtitles_json = ?, external_id = ?, missing = 0,
                    duration_ms = COALESCE(?, CASE WHEN ? THEN NULL ELSE duration_ms END)
                 WHERE id = ?",
            )
            .bind(item.size)
            .bind(item.mtime)
            .bind(&item.show)
            .bind(item.season)
            .bind(item.episode)
            .bind(&item.title)
            .bind(&item.subtitles_json)
            .bind(&item.external_id)
            .bind(item.duration_ms)
            .bind(*file_changed)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        for id in &changes.mark_missing {
            sqlx::query("UPDATE items SET missing = 1 WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await
    }

    /// Records an item's duration as reported by a client.
    pub async fn set_duration(&self, item_id: &str, duration_ms: i64) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE items SET duration_ms = ? WHERE id = ?")
            .bind(duration_ms)
            .bind(item_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
    }

    // --- Channel membership -----------------------------------------------

    /// Replaces the whole `channel_items` table.
    pub async fn replace_channel_items(
        &self,
        pairs: &[(String, String)],
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM channel_items")
            .execute(&mut *tx)
            .await?;
        for (channel_id, item_id) in pairs {
            sqlx::query("INSERT OR IGNORE INTO channel_items (channel_id, item_id) VALUES (?, ?)")
                .bind(channel_id)
                .bind(item_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await
    }

    /// The playable (not missing) items of a channel.
    pub async fn channel_items(&self, channel_id: &str) -> Result<Vec<ItemRow>, sqlx::Error> {
        let columns = ITEM_COLUMNS
            .split(", ")
            .map(|c| format!("i.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {columns} FROM items i
             JOIN channel_items ci ON ci.item_id = i.id
             WHERE ci.channel_id = ? AND i.missing = 0"
        )))
        .bind(channel_id)
        .fetch_all(&self.pool)
        .await
    }

    // --- Channel state ----------------------------------------------------

    pub async fn state(&self, channel_id: &str) -> Result<Option<StateRow>, sqlx::Error> {
        sqlx::query_as(
            "SELECT channel_id, item_id, position_ms, cursor_json, updated_at, children_json
             FROM channel_state WHERE channel_id = ?",
        )
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Moves a channel to `item_id` with a new cursor and position.
    pub async fn save_state(&self, state: &StateRow) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO channel_state
                (channel_id, item_id, position_ms, cursor_json, updated_at, children_json)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (channel_id) DO UPDATE SET
                item_id = excluded.item_id,
                position_ms = excluded.position_ms,
                cursor_json = excluded.cursor_json,
                updated_at = excluded.updated_at,
                children_json = excluded.children_json",
        )
        .bind(&state.channel_id)
        .bind(&state.item_id)
        .bind(state.position_ms)
        .bind(&state.cursor_json)
        .bind(state.updated_at)
        .bind(&state.children_json)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    // --- Events -----------------------------------------------------------

    pub async fn insert_event(&self, id: &str, event: &NewEvent<'_>) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO events (id, ts, client_id, channel_id, item_id, kind, position_ms,
                                 resolved_channel_id, resolution_path_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(event.ts)
        .bind(event.client_id)
        .bind(event.channel_id)
        .bind(event.item_id)
        .bind(event.kind)
        .bind(event.position_ms)
        .bind(event.resolved_channel_id)
        .bind(event.resolution_path_json)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// The kind of the most recent event on a channel.
    pub async fn last_event_kind(&self, channel_id: &str) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT kind FROM events WHERE channel_id = ? ORDER BY rowid DESC LIMIT 1",
        )
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(kind,)| kind))
    }

    /// The most recent events, newest first. Insertion order (rowid) defines
    /// "newest", so events recorded in the same millisecond keep their order.
    pub async fn history(&self, limit: u32) -> Result<Vec<EventRow>, sqlx::Error> {
        sqlx::query_as(
            "SELECT id, ts, client_id, channel_id, item_id, kind, position_ms,
                    resolved_channel_id, resolution_path_json
             FROM events ORDER BY rowid DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_run_on_empty_database_and_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marathon.db");

        let db = Db::open(&path).await.unwrap();
        let tables: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        let tables: Vec<_> = tables.into_iter().map(|(n,)| n).collect();
        assert_eq!(
            tables,
            [
                "_sqlx_migrations",
                "channel_items",
                "channel_state",
                "channels",
                "events",
                "items",
                "sources"
            ]
        );
        sqlx::query("INSERT INTO sources (id, kind, config_json) VALUES ('s', 'filesystem', '{}')")
            .execute(db.pool())
            .await
            .unwrap();
        db.close().await;

        // Reopening (a restart) applies nothing new and keeps the data.
        let db = Db::open(&path).await.unwrap();
        let applied: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(applied.0, MIGRATOR.iter().count() as i64);
        let sources: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sources")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(sources.0, 1);
        // Running the migrator again explicitly is also a no-op.
        MIGRATOR.run(db.pool()).await.unwrap();
    }

    #[tokio::test]
    async fn history_is_newest_first() {
        let db = Db::open_in_memory().await.unwrap();
        for (i, kind) in ["tuned", "progress", "ended"].iter().enumerate() {
            let event = NewEvent {
                // Same timestamp for all: order must come from insertion.
                ts: 1000,
                client_id: None,
                channel_id: "c",
                resolved_channel_id: "c",
                resolution_path_json: r#"["c"]"#,
                item_id: "i",
                kind,
                position_ms: i as i64,
            };
            db.insert_event(&format!("e{i}"), &event).await.unwrap();
        }
        let kinds: Vec<_> = db
            .history(10)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, ["ended", "progress", "tuned"]);
        assert_eq!(db.history(1).await.unwrap().len(), 1);
        assert_eq!(
            db.last_event_kind("c").await.unwrap().as_deref(),
            Some("ended")
        );
        assert_eq!(db.last_event_kind("other").await.unwrap(), None);
    }
}
