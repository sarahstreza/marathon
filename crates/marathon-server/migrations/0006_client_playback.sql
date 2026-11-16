-- What each client can play and which quality it chose (PLAN.md M13):
-- its capabilities as the API's `Capabilities` JSON, a profile ID
-- (`original`, `auto`, or a rung of the quality ladder), and the last
-- bandwidth it measured, in kbit/s. A client with no row plays the
-- original files, as before.
CREATE TABLE client_playback (
    client_id         TEXT PRIMARY KEY,
    capabilities_json TEXT NOT NULL,
    profile           TEXT NOT NULL,
    measured_kbps     INTEGER,
    updated_at        INTEGER NOT NULL
);
