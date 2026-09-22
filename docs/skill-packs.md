# Skill packs

A *skill pack* is a directory that packages related agent skills (and optionally the
tool servers those skills call) in the canonical Agent Plugins folder layout:

```
my-pack/
  plugin.json            manifest: what this pack is and what it ships
  mcp.json               tool servers the skills call (only when there are any)
  skills/<name>/SKILL.md one directory per skill
```

`COLONIZER_PLUGIN_DIRS` (docs/protocol.md, "Plugin directories") names the enabled packs.
Each resolves to `<data>/plugins/<name>` or the vendored `<COLONIZER_HOME>/plugins/<name>`,
mounts read-only at `/opt/colonizer/plugins/<name>`, and loads into Claude Code as one
`{type: 'local', path}` plugin entry.

## plugin.json

The manifest. Canonical readers (the validator, boot validation) prefer the root
`plugin.json`; the Claude Code SDK reads the legacy `.claude-plugin/plugin.json`, so
staged packs keep both with the same `name`, `version` and `description`.

| Field | Meaning |
| :--- | :--- |
| `$schema` | https URL naming this manifest shape (`https://colonizer.dev/schemas/skill-pack.json`). Optional, but staged packs set it. |
| `name` | Plain, filesystem-safe pack name; conventionally the directory basename. |
| `version` | Semver (`1.2.3`, with optional prerelease/build). |
| `description` | Non-empty: what the pack is for. |
| `skills` | Array of skill names the pack ships. Each must have a `skills/<name>/SKILL.md`. |

Version pins live in `vendor/vendor.lock` as the sha256 of the upstream archive. Patch bumps
are auto-adoptable by the daily vendored-plugin updater; a minor or major bump needs a new
pin pull request, because new or changed skills can change what a colony does.

## mcp.json

Only when the pack ships tool servers. An object of server entries (or one under
`mcpServers`/`servers`); each entry is either local or remote:

- Local stdio: `command` plus optional `args` and `env`.
- Remote: `url` plus a non-empty `hosts` (or `allowedHosts`) declaration, so the sandbox
  gate keeps working. A remote entry without hosts is a validation error.

Packs with no tool servers — superpowers is one — ship no `mcp.json` at all.

## skills/<name>/SKILL.md

One directory per skill, named for the skill. `SKILL.md` is a Markdown file with a leading
`---` frontmatter block and a body:

- Frontmatter: non-empty `name:` and `description:`. `description` is what the model sees
  when deciding to invoke the skill, so keep it short and say when the skill applies.
- Body: the skill itself — instructions, procedures, references to files beside it.

Skill names must be unique across every enabled pack. A collision is a validation error
that names both packs: the model addresses a skill as `<pack>:<name>`, and two packs
answering to the same name are ambiguous by construction.

## Minimal example

`plugin.json`:

```json
{
  "$schema": "https://colonizer.dev/schemas/skill-pack.json",
  "name": "my-pack",
  "version": "1.0.0",
  "description": "Example pack: one skill, one local tool server",
  "skills": ["my-skill"]
}
```

`mcp.json`:

```json
{
  "mcpServers": {
    "my-server": {
      "command": "my-server",
      "args": ["--stdio"],
      "env": { "MY_SERVER_MODE": "readonly" }
    }
  }
}
```

`skills/my-skill/SKILL.md`:

```md
---
name: my-skill
description: Use when the colony needs an example worked through.
---

# My skill

Do the example thing, then stop.
```

## Reference migration: superpowers

superpowers ([obra/superpowers](https://github.com/obra/superpowers), v6.4.1, MIT) is the
first pack on the canonical layout, staged by `scripts/fetch-vendor.sh` at
`dist/plugins/superpowers`:

- Added: a root `plugin.json`, synthesized at stage time from the upstream manifest's
  `name`/`version`/`description` plus the staged skill names (upstream lists no skills).
  `.claude-plugin/plugin.json` stays byte-identical for the SDK.
- Untouched: `skills/` (13 of the 15 upstream skills; `using-git-worktrees` and
  `finishing-a-development-branch` stay dropped — see protocol.md) and `LICENSE`.
- No `mcp.json`: superpowers ships no tool servers.
- Runtime behavior is identical: the same skills load, the SDK reads what it always read,
  and the runner still bootstraps `skills/using-superpowers/SKILL.md` into the system
  prompt. The diff is layout + manifest only.

To verify: validate the staged pack (`node scripts/validate-plugins.mjs
dist/plugins/superpowers`), enable the pack, and check the agent lists its skills.
