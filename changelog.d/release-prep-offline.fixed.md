Release train: `release-prep.mjs` falls back to a networked `cargo update -w` when the offline one fails, so it works on a fresh CI runner with no registry cache.
