-- Items from sources other than the filesystem (Jellyfin, M8) are known to
-- the source by an ID of its own, which streaming needs. NULL for
-- filesystem items, whose path is all that is needed.
ALTER TABLE items ADD COLUMN external_id TEXT;
