-- One row per mothership that has the live map switched on and sent a heartbeat in the last hour.
-- No IP address, no exact location, no repository, no user: see docs/telemetry.md.
CREATE TABLE presence (
  install TEXT PRIMARY KEY,   -- SHA-256 of the random install id the mothership sends
  cell_lat REAL,              -- centre of its ~25 km grid cell; NULL when Cloudflare had no location
  cell_lon REAL,
  colonies INTEGER NOT NULL,  -- colonies with a running microVM
  version TEXT NOT NULL,      -- Colonizer version
  platform TEXT NOT NULL,     -- linux-x86_64, darwin-arm64 or other
  seen_at INTEGER NOT NULL    -- unix seconds of the last heartbeat
);

CREATE INDEX presence_seen_at ON presence (seen_at);
