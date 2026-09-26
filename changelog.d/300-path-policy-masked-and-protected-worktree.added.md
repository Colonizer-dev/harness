- **Path policy: masked and protected worktree paths.** Credential files in a checkout — `.env`,
  `.envrc`, `.npmrc`, `.netrc`, `.git-credentials`, `.pypirc` — are now masked out of a colony's
  view (the guest gets an empty file instead, before the agent starts), and agent-facing config —
  `.git/config`, `.git/hooks/`, `.gitmodules`, `.claude/`, `.codex/`, `.mcp.json`, `.devcontainer/`,
  `.vscode/`, `.idea/` — is pinned read-only. Three sandbox settings add to the lists or opt paths
  out of them (`mask_paths`, `protect_paths`, `unmask_paths`; every opt-out is logged at boot);
  unusable entries are refused at save time. At publish, empty boot placeholders are removed before
  staging and changed masked or protected paths are logged on the colony — reported, not rewritten.
  See [docs/path-policy.md](docs/path-policy.md). ([#300])

[#300]: https://github.com/Colonizer-dev/harness/issues/300
