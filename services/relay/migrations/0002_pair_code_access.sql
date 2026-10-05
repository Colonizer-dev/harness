-- Pairing with the Colonizer pair code alone (#1086).
--
-- require_github: whether this install's subdomain still sends every browser through the GitHub owner
-- sign-in (#534) before anything is forwarded. Rows that exist when this migration runs keep 1 (the
-- old behaviour) until their mothership switches it off with a signed PUT …/settings; installs
-- registered from now on default to 0 unless the registration asks for 1.
ALTER TABLE installs ADD COLUMN require_github INTEGER NOT NULL DEFAULT 1;

-- The relay-side throttle on the pass-through paths (src/throttle.js): invite opens, and credentials
-- the mothership rejected, counted in fixed windows per install and per client. A client is never
-- stored as an address: its key is an HMAC of the connecting IP under SESSION_SECRET, truncated, and a
-- row lives only until its window ends (expired rows are deleted on the back of the next count).
CREATE TABLE throttle (
  key TEXT PRIMARY KEY,         -- '<kind>:install:<install id>' or '<kind>:client:<hmac>'
  count INTEGER NOT NULL,       -- events counted in the current window
  window_ends INTEGER NOT NULL  -- unix seconds; at or past this the row is dead and the count restarts
);
