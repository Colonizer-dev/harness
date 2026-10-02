- **Security preset for red-team runs.** A run can now use `"preset": "security"` (the wizard's
  **Preset** choice, the API and schedules, or `colonizer redteam start --preset security`): eight
  security focus areas with the same cycling and briefs, a proof attached to every finding, and hunters
  kept to the repository and a local instance. Before the hunters launch, a deterministic pre-scan of
  the host mirror (no model tokens, no repository code run; gitleaks when installed, a built-in
  fallback otherwise) turns committed env files, secrets, client-exposed keys, lockfile gaps, open
  CORS, string-built SQL, unsigned webhooks, public buckets, disabled row level security and risky
  agent files into leads dealt to the matching hunter. The run's report adds the leads and an operator
  checklist that is never marked passed, and the synthesis ranks by severity with proofs. General runs
  are unchanged. See [docs/red-team.md](docs/red-team.md#the-security-preset).
