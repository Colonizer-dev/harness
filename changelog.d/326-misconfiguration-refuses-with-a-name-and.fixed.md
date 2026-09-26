- **Misconfiguration refuses with a name and a fix instead of degrading silently.** A settings save
  refuses an unknown key, naming it and the settings the module does take (a key already stored still
  passes, or a provider switch would lock you out of saving); an enum refusal lists the options; a
  corrupt `colonizer.toml` names the file, the error and the fix instead of a bare "using defaults"; a
  corrupt `claude-accounts.json` is logged instead of silently resetting your default account, and its
  writers refuse to overwrite it; a local plugin copy shadowing a vendored one is logged when the
  skillset is saved; and duplicate provider ids in a hand-edited `providers.json` are named, with the
  save over them refused. The house rule and its audit table are in
  [docs/architecture.md](docs/architecture.md). ([#326])

[#326]: https://github.com/Colonizer-dev/harness/issues/326
