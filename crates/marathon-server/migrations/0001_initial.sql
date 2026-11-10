-- Initial schema; see PLAN.md §7.
--
-- No foreign keys between these tables, on purpose: config is authoritative
-- for sources and channels, and removing one from config must not cascade
-- into playback state or history. A channel that is removed and later added
-- back resumes where it was.

-- Configured sources, mirrored from config on startup.
CREATE TABLE sources (
    id          TEXT PRIMARY KEY NOT NULL,
    kind        TEXT NOT NULL,
    config_json TEXT NOT NULL
);

-- Every video ever found by a scan. Files that disappear are marked
-- `missing` rather than deleted, so history survives temporary unmounts.
CREATE TABLE items (
    id             TEXT PRIMARY KEY NOT NULL,       -- UUIDv7
    source_id      TEXT NOT NULL,
    path           TEXT NOT NULL,                   -- relative to source root, '/'-separated
    size           INTEGER NOT NULL,
    mtime          INTEGER NOT NULL,                -- ms since Unix epoch
    show           TEXT,
    season         INTEGER,
    episode        INTEGER,
    title          TEXT NOT NULL,
    duration_ms    INTEGER,                         -- reported by clients
    subtitles_json TEXT NOT NULL DEFAULT '[]',      -- sidecar subtitles
    missing        INTEGER NOT NULL DEFAULT 0,
    UNIQUE (source_id, path)
);

-- Channel definitions, upserted from config on startup.
CREATE TABLE channels (
    id             TEXT PRIMARY KEY NOT NULL,
    number         INTEGER NOT NULL,
    name           TEXT NOT NULL,
    rule_json      TEXT NOT NULL,
    selectors_json TEXT NOT NULL
);

-- Which items each channel's selectors match. Rebuilt after every scan and
-- on startup. Includes missing items; readers filter on items.missing.
CREATE TABLE channel_items (
    channel_id TEXT NOT NULL,
    item_id    TEXT NOT NULL,
    PRIMARY KEY (channel_id, item_id)
);
CREATE INDEX channel_items_by_item ON channel_items (item_id);

-- Where each channel is: the current item, the saved position within it,
-- and the rules engine's cursor.
CREATE TABLE channel_state (
    channel_id  TEXT PRIMARY KEY NOT NULL,
    item_id     TEXT,
    position_ms INTEGER NOT NULL DEFAULT 0,
    cursor_json TEXT NOT NULL,
    updated_at  INTEGER NOT NULL                    -- ms since Unix epoch
);

-- Append-only playback event log. Newest-first reads use rowid order.
CREATE TABLE events (
    id          TEXT PRIMARY KEY NOT NULL,          -- UUIDv7
    ts          INTEGER NOT NULL,                   -- ms since Unix epoch
    client_id   TEXT,
    channel_id  TEXT,
    item_id     TEXT,
    kind        TEXT NOT NULL,  -- tuned | progress | ended | skipped | previous | paused | resumed
    position_ms INTEGER
);
CREATE INDEX events_by_channel ON events (channel_id, ts);
