-- What the relay keeps about an install: the Ed25519 public key it registered, when, and who owns it.
-- Nothing else: no request or response bodies, no headers, no IP addresses, no tokens — the README
-- (services/relay/README.md) is the full account of what is and is not stored.
CREATE TABLE installs (
  id TEXT PRIMARY KEY,          -- 20 random lowercase base32 chars: the <id> in <id>.my.colonizer.dev
  public_key TEXT NOT NULL,     -- base64 Ed25519 public key the mothership generated at registration
  created_at INTEGER NOT NULL,  -- unix seconds of registration
  owner_github_id INTEGER,      -- the GitHub user id bound by a confirmed pairing; NULL until then
  owner_github_login TEXT
);

-- A pairing is the short-lived 6-digit code a browser sign-in produced, waiting for the local cockpit
-- to confirm it with a signed request. Single-use: confirming one deletes every pairing of the install.
CREATE TABLE pairings (
  install_id TEXT NOT NULL,
  code TEXT NOT NULL,           -- 6 digits
  github_id INTEGER NOT NULL,   -- the GitHub account waiting to become the owner
  github_login TEXT NOT NULL,
  expires_at INTEGER NOT NULL,  -- unix seconds; expired rows are deleted opportunistically, never read
  PRIMARY KEY (install_id, code)
);
