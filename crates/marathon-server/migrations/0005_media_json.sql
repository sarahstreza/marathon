-- What ffprobe found in an item's file (PLAN.md M13): container,
-- duration, bitrate, and its video, audio and subtitle streams, as the
-- server's `MediaInfo` JSON. NULL until the item is first probed, and set
-- back to NULL when a rescan sees the file's size or mtime change.
ALTER TABLE items ADD COLUMN media_json TEXT;
