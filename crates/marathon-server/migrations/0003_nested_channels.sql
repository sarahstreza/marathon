-- Nested channels (PLAN.md M11).
--
-- An independent parent channel keeps its own position in each child
-- channel (and their children), as a JSON object of states keyed by the
-- child's address relative to the parent: {"contrapoints": {"item_id": …,
-- "position_ms": …, "cursor": …}, "b>c": …}. Empty for every other channel.
ALTER TABLE channel_state ADD COLUMN children_json TEXT NOT NULL DEFAULT '{}';

-- Events keep `channel_id` as the channel the viewer was tuned to, and now
-- also record the channel the item came from and the chain of channels in
-- between (a JSON array of IDs, tuned channel first). NULL for events
-- recorded before nesting existed.
ALTER TABLE events ADD COLUMN resolved_channel_id TEXT;
ALTER TABLE events ADD COLUMN resolution_path_json TEXT;
