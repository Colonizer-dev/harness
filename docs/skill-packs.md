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

One pack is loaded by purpose rather than by setting: `archify` (vendored from
[tt-a1i/archify](https://github.com/tt-a1i/archify), MIT) is added to every mapping colony's
packs, whatever its org has switched on, because drawing the repository is that colony's whole
task (docs/protocol.md, "Architecture maps"). It is a one-skill pack: upstream's skill directory,
which reads its schemas and runs its own `bin/archify.mjs` by relative path, staged unchanged under
`skills/archify/`.

## plugin.json

The manifest. Canonical readers (the validator, boot validation) prefer the root
`plugin.json`; the Claude Code SDK reads the legacy `.claude-plugin/plugin.json`. Only
superpowers stages both today (staging synthesizes the root manifest); ecc and
google-skills stage `.claude-plugin/plugin.json` alone, which the SDK and both checkers
read as the manifest.

| Field | Meaning |
| :--- | :--- |
| `$schema` | Optional. When present it must be an `https://` URL string. Colonizer publishes no schema for this manifest, so staged packs leave it out. |
| `name` | Plain, filesystem-safe pack name; conventionally the directory basename. |
| `version` | Semver (`1.2.3`, with optional prerelease/build). |
| `description` | Non-empty: what the pack is for. |
| `skills` | Array of skill names the pack ships. Each must have a `skills/<name>/SKILL.md`. A legacy directory-style entry (`"./skills/"`, as upstream ecc ships) means every skill under that directory; both checkers accept it, as `is_skill_tree` in `crates/colonizer/src/plugins.rs` does. |

Version pins live in `vendor/vendor.lock` as the sha256 of the upstream archive. Patch bumps
are auto-adoptable by the daily vendored-plugin updater; a minor or major bump needs a new
pin pull request, because new or changed skills can change what a colony does.

## mcp.json

Only when the pack ships tool servers. An object of server entries (or one under
`mcpServers`/`servers`); each entry is either local or remote:

- Local stdio: `command` plus optional `args` and `env`.
- Remote: `url` plus a non-empty `hosts` (or `allowedHosts`) declaration. Both checkers
  reject a remote entry without hosts (see Validation below). The declared hosts are not
  enforced by a sandbox egress gate yet — colonies boot with `--net public` — but the boot
  path reads and checks the declaration so the future gate (#304) can consume it.

Packs with no tool servers — superpowers is one — ship no `mcp.json` at all.

## skills/<name>/SKILL.md

One directory per skill, named for the skill. `SKILL.md` is a Markdown file with a leading
`---` frontmatter block and a body:

- Frontmatter: non-empty `name:` and `description:`. `description` is what the model sees
  when deciding to invoke the skill, so keep it short and say when the skill applies.
- Body: the skill itself — instructions, procedures, references to files beside it.

Skill names must be unique across every enabled pack. A collision blocks the colony's
boot with an error that names both packs: the model addresses a skill as `<pack>:<name>`,
and two packs answering to the same name are ambiguous by construction.

## Validation

Two checkers, with different reach:

- **Boot and save time** (`validate` in `crates/colonizer/src/plugins.rs`). Every enabled
  pack is checked when a colony boots, and again when Settings or an org override names
  it. The manifest must exist at `plugin.json` or `.claude-plugin/plugin.json` and parse
  as a JSON object, every `skills/<name>/` holding a `SKILL.md` must have a plain name,
  every skill the manifest lists must exist on disk, and an `mcp.json` must give every
  server a stdio `command` or a remote `url` with a non-empty `hosts`/`allowedHosts`
  declaration (`mcp_hosts`, the reader the egress gate of #304 will consume). A pack that
  fails blocks the colony's boot, and the save is refused, with the error naming the file.
  Skill-name uniqueness across packs is checked at boot only.
- **The full rule set** (`scripts/validate-plugins.mjs`): semver `version`, non-empty
  `description`, SKILL.md frontmatter, `mcp.json` shape and remote-host declarations,
  and duplicate names within a pack. It runs by hand
  (`node scripts/validate-plugins.mjs <dir>...`), in CI and in the vendored-plugin
  updater's proposal workflow over the staged packs (`VENDOR_KINDS="plugin prompt" sh
  scripts/fetch-vendor.sh` stages `dist/plugins/*`, then the validator runs over them),
  and in the updater itself, which validates the new archive of each pin it stages from
  that archive before rewriting the lock line — a pack that fails is skipped, its errors
  in the proposal when another pin is adopted, otherwise in the failed run's log.

## Minimal example

`plugin.json`:

```json
{
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

## Downloadable skillsets

Some skillsets are too big, or carry native code, to ship in every release. Those are
**downloadable**: Settings → Skillsets lists them with a Download button, the mothership
fetches a bundle pinned by sha256 and unpacks it to `<data>/plugins/<name>`, and from then on
it is an ordinary local skillset — the same resolution, validation and read-only mount at
`/opt/colonizer/plugins/<name>` as any other, switched on with the same toggle. Nothing is
fetched until someone asks, and an operator's own `plugins/<name>` directory is never
overwritten (the download refuses, and the row says the local copy is used instead).

### graft

[graft](https://github.com/NanoNets/context-graph-engine) (`@nanonets/graft`, MIT) builds a
code map of a repository — files, symbols and call edges — and answers `graft ask "<task>" --source`,
`graft grep`, `graft callers <symbol> --depth all`, `graft skeleton <file>` and `graft map` with
exact `file:line`. The skillset's `SKILL.md` teaches the agent to reach for it before grepping and
reading files.

| | |
| --- | --- |
| Pin | `crates/colonizer/graft.lock`, one row per colony architecture (`linux-x86_64`, `linux-aarch64`), compiled into the mothership |
| Bundle | `graft/` with the plugin (`.claude-plugin/plugin.json`, `plugin.json`, `skills/graft/SKILL.md`), `bin/graft`, `node/bin/node` (Node 22, `vendor/graft/node.lock`), `runtime/node_modules` (`npm ci` from `vendor/graft/package-lock.json`), `BUNDLE.json`, `LICENSE` — about 80 MB compressed |
| Built by | `.github/workflows/graft-bundle.yml` on a `graft-<graft version>-<build>` tag (native runners, `node:22-bookworm`, smoke-tested by `scripts/graft-bundle/smoke.sh`); `workflow_dispatch` is a dry run that only uploads artifacts. Locally: `scripts/build-graft-bundle.sh` in a microVM |
| API | `GET /api/plugins/graft` (state: `idle`, `downloading`, `unpacking`, `installed`, `failed`, `unavailable` when nothing is pinned for this architecture, `local` when `plugins/graft` is the operator's own); `POST /api/plugins/graft/download` starts or joins the download; `GET /api/plugins` carries the same status under `downloadable` |

**Why its own Node.** graft 0.19 imports the native `tree-sitter` 0.21 core at startup, and that
binding does not compile against Node 24's V8 headers — the major `node-guest` pins for colonies.
Several grammars also publish no `linux-arm64` prebuild (`tree-sitter-kotlin` none at all), so the
bundle compiles them against the Node 22 it carries, inside Debian bookworm (the colony image's
glibc), and `bin/graft` runs graft with that Node, never the colony's.

**In the colony.** The mount is read-only, so `bin/graft` keeps the graph under
`/var/tmp/colonizer-graft` — outside the worktree, never in a commit — and builds it on the first
query (`graft build`, the free pass: no model). Every later query keeps it in sync with the agent's
edits. It runs offline by construction: `ANTHROPIC_*`, `OPENAI_API_KEY` and `GRAFT_*` keys are
unset for it (only `graft build --deep` would use a model), `DO_NOT_TRACK=1` and `CI=1` close its
telemetry, and its background registry version check is answered from a cache that never expires.

**Updating graft.** Bump `@nanonets/graft` in `vendor/graft/package.json`, regenerate the lock
(`npm install --package-lock-only --ignore-scripts`), push a `graft-<version>-1` tag, and pin the
release's `SHA256SUMS` in `graft.lock` by pull request. A pinned release newer than what is on
disk shows the Download button again.

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
