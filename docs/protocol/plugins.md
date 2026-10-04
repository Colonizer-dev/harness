# Plugin directories

Part of the [Colonizer protocol](../protocol.md).

`COLONIZER_PLUGIN_DIRS` names directories the mothership resolves in two places, in order: what the
operator put in `<data>/plugins/<name>`, then what shipped with the app in `<COLONIZER_HOME>/plugins/<name>`.
A local copy therefore overrides a vendored one of the same name. Each is a plain name, never a path,
and each is mounted read-only at `/opt/colonizer/plugins/<name>`.

`scripts/fetch-vendor.sh` stages vendored plugins at `dist/plugins/<name>`, which `install.sh` copies to
`<COLONIZER_HOME>/plugins/<name>`, and `install.sh` fails if a `plugin` entry in `vendor/vendor.lock` didn't
land there. It stages four vendored plugins today:

| Plugin | Source | Staged |
| :--- | :--- | :--- |
| `ecc` | [affaan-m/ECC](https://github.com/affaan-m/ECC) v2.2.1, MIT, pinned by sha256 in `vendor/vendor.lock` | `.claude-plugin/`, `skills/` (286), `agents/` (68), `commands/` (94), `scripts/`, `LICENSE`. 8.2 MB of the 58 MB source |
| `superpowers` | [obra/superpowers](https://github.com/obra/superpowers) v6.4.2, MIT, pinned by sha256 in `vendor/vendor.lock` | `.claude-plugin/`, `skills/` (13 of 15), `LICENSE`. 596 KB of the 2.4 MB source |
| `archify` | [tt-a1i/archify](https://github.com/tt-a1i/archify) at a commit, MIT, pinned by sha256 in `vendor/vendor.lock` | `skills/archify/` (upstream's `archify/` skill: `SKILL.md`, schemas, examples, renderers, the dependency-free `bin/archify.mjs`) minus `test/` and `scripts/check-update.mjs`, generated root and `.claude-plugin/` manifests, `LICENSE`, `THIRD_PARTY_NOTICES.md`. 6.4 MB. Loaded by mapping colonies (see "Architecture maps") |
| `google-skills` | [google/skills](https://github.com/google/skills) at a commit (no upstream tags), Apache-2.0, pinned by sha256 in `vendor/vendor.lock` | `skills/finding-google-skills/` (Colonizer's copy), `catalog/` (147 skills), `index.json`, a generated `.claude-plugin/plugin.json`, `LICENSE`. 7.0 MB |

**Canonical layout.** `superpowers` is staged in the Agent Plugins folder layout in
[docs/skill-packs.md](../skill-packs.md): a root `plugin.json` (and `mcp.json` only when a
pack ships tool servers) alongside the `.claude-plugin/` manifest the SDK reads. Staging
synthesizes the root manifest; the skills are untouched and runtime behavior is identical.
`ecc` and `google-skills` still carry only `.claude-plugin/plugin.json`, which boot
validation accepts as the manifest.

**ECC's hooks are not staged.** Its plugin manifest sets `userConfig.hooks_enabled` to `true` by
default and Claude Code discovers `hooks/hooks.json` by convention, so "skills and agents only" cannot
be expressed as a setting: every ECC hook is a `node -e` bootstrap that spawns first and reads
`ECC_HOOKS_ENABLED` second. The staging step removes the directory, and `fetch-vendor.sh` fails if it
survives. `ECC_HOOKS_ENABLED=false` is also set in any colony that loads a plugin, as a second line.

**superpowers' hook becomes system-prompt text.** Its one hook, `SessionStart` on
`startup|clear|compact`, injects `skills/using-superpowers/SKILL.md`, and that is what makes the agent
reach for the other skills. `hooks/` is removed as for ECC. Instead, for every loaded plugin directory
that contains `skills/using-superpowers/SKILL.md`, the claude-code runner appends that text to the system
prompt inside the hook's own `<EXTREMELY_IMPORTANT>` wrapper. The system prompt survives compaction,
which is what the hook's `compact` matcher was for.

**Two superpowers skills are not staged.** `using-git-worktrees` creates another worktree and
`finishing-a-development-branch` merges, pushes or opens a pull request, from inside the colony, around
the worktree, branch and publish step Colonizer already owns. Other skills name them, so the appended
text says they are missing on purpose and to stop at those steps. `fetch-vendor.sh` fails if either, or
`hooks/`, survives staging.

**Google's skills load on demand.** Claude Code discovers exactly one skill in `google-skills`:
`finding-google-skills`. The other 147 sit in `catalog/`, outside `skills/`, with upstream's directory
shape, so their relative links still resolve. `index.json` is upstream's catalog with each `entrypoint`
rewritten from a `raw.githubusercontent.com` URL to a path relative to the plugin root
(`catalog/cloud/gke-basics/SKILL.md`). The finder is Colonizer's copy of upstream's
(`vendor/google-skills/finding-google-skills/SKILL.md`, Apache-2.0, changes noted in the file): it finds
the plugin root two directories above the base directory Claude Code gives a skill when it loads, filters
the local catalog and reads only the matching `SKILL.md`. It has no network steps, and it doesn't copy
anything into the working directory, where the copy would land in the pull request. Its description is
kept short: Claude Code drops long skill descriptions from the list it shows the model, which left
upstream's 604-character one as a bare name. Not staged: upstream's `plugins/` (MCP servers, and git
submodules a codeload archive doesn't include). `fetch-vendor.sh` fails on any catalog entry it can't map
to a staged file, on a hook or MCP configuration anywhere in the plugin, on a `raw.githubusercontent.com`
URL left in the catalog or the finder, and on any second skill under `skills/`.

**Architecture maps.** `POST /api/maps/{owner}/{repo}` launches a mapping colony: an ordinary colony
with `origin: "map"` and autopilot on, whose boot adds the `archify` skillset to its plugins and whose
instructions are to draw the repository at HEAD as an archify architecture diagram — every component
tied to the repository files it lives in (`sources`), validated with archify's own
`bin/archify.mjs validate architecture … --repo-root /workspace`, which checks each source is a file at
the pinned revision — written to `/harness/out/architecture.json`, and to leave the worktree untouched.
It runs as a single Sonnet agent at medium effort (`delegate` off), whatever the install's agent
settings, and never opens a pull request. When its turn ends with a valid file the mothership stores the
map and stops the colony; without one it is given 15 minutes, then stopped with an error. A publish or
the next `GET /api/maps/…` also picks up a file that was missed. The mothership reads the
file, keeps only what the cockpit draws after checking it (an architecture diagram, plain unique ids,
connections and boundary members that name components, repository-relative source paths with no `..`,
at least one sourced component, at most 1 MB, 120 components, 400 connections, 40 boundaries and 40
sources per component) and stores it at
`<data>/maps/<owner>/<repo>.json` with the revision, time and colony. The cockpit's nest has a Map mode
that draws it as the nest: components are chambers at archify's layout, boundaries the mounds they sit
in, connections tunnels, the mothership's mouth on the surface above the entry chamber (the one nothing
connects into that starts the most), and each live colony's ants walk from that mouth along the tunnels
to the chambers whose sources share a directory with the files `GET /api/touched` says it changed.
The map can keep itself fresh: a loop with `kind: "map"` ([loops.md](../loops.md)) launches the same
mapping colony on a schedule, with origin `map:loop:<loop id>` — its own repository each firing, or
`owner/*` mapping the org's repositories one at a time, ten minutes apart, the list taken fresh each
cycle. Its end reports back to the loop and records `map.refresh` in the activity log; a refresh
that produced no valid map keeps the stored one.

**Keeping vendored plugins current.** `scripts/update-vendored-plugins.mjs` checks every `plugin` entry in
`vendor/vendor.lock` against its upstream (the latest GitHub release for a `refs/tags/` pin, the default
branch for a commit pin) and reports skills added, removed and changed between the pinned archive and the
new one. `--write` rewrites the lock, comments included. `.github/workflows/vendored-plugin-updates.yml`
runs it daily, stages the result with `VENDOR_KINDS=plugin scripts/fetch-vendor.sh` so a failing check
stops the proposal, pushes `vendor/plugin-updates`, and opens a pull request, or, while the repository
doesn't let GitHub Actions open pull requests, keeps an issue open with the same description and a link to
open it, closing the issue once a run finds nothing to change. It never merges.

**Keeping the runtime pins current.** The same model covers the two runtime locks:
`crates/colonizer/images.lock`, which pins each preset's colony image by multi-arch OCI index digest:
one pin serves both linux/amd64 and linux/arm64 colonies, and the lock is compiled into the mothership,
and `crates/colonizer/claude-code.lock`, which pins the Linux Claude Code build colonies run by version and sha256.
`scripts/update-runtime-pins.mjs` checks both upstreams, the registry's manifest API for the images and
Anthropic's `stable` channel for Claude Code, and stages the newly pinned Claude Code build through
`scripts/fetch-agent-binary.sh`, so an update that fails the checksum check a real install does never
becomes a proposal. `.github/workflows/runtime-pin-updates.yml` runs it daily, pushes `runtime/pin-updates`,
and opens a pull request, or keeps an issue open with a link, the way the vendored plugins do. It never
merges: a pin bump changes what every release runs.

The `colony-node` and `colony-<preset>` toolbox images (#753) are built and published by
`.github/workflows/colony-image.yml`, and their index digests are pinned here the same way; until a
digest lands, that preset boots its stock upstream image.

**Skillsets are switches, off by default except `archify`.** Settings shows the `claude-code` module's
`plugins` setting (schema `"format": "plugin-dirs"`) as one switch per plugin directory from
`GET /api/plugins` (plus the downloadable graft skillset, listed under `downloadable`), and writes the
same comma-separated list of names. A saved name that no longer resolves is shown as missing, since a
colony loading it fails to boot. The default is `archify`, so any colony can draw a map; an empty list
loads nothing. An org workspace
can switch single skillsets on or off over that list with `agent.skillsets` (see Org workspaces).
