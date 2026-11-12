-- Where each item comes in its channel's definition, for the `listed`
-- rule: the index of the selector entry (a selector's `path`, or one of
-- its `files`) that selected it, counted across the channel's selectors.
-- Items selected by one entry (a folder) share a position. Rebuilt with
-- the rest of `channel_items`; NULL only until the first rebuild.
ALTER TABLE channel_items ADD COLUMN position INTEGER;
