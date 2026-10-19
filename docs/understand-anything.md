# understand-anything

[understand-anything](https://github.com/Egonex-AI/Understand-Anything) (MIT, by Egonex) is a Claude
Code plugin that builds a knowledge graph of a repository and answers from it: guided tours,
semantic search, and what a change would affect. It is an optional, **off-by-default** downloadable
skillset: nothing installs it until an operator asks, and downloading it does not switch it on.

It sits beside the two packs Colonizer already ships:

| | What it adds | Where its data lives |
| --- | --- | --- |
| [graft](skill-packs.md#graft) | a code map for lookups — ranked answers with exact `file:line`, callers of a symbol, a file's API | its own graph, per colony, offline |
| [archify](skill-packs.md) (vendored, on by default) | an architecture map — the modules, their edges and the flows between them, for mapping colonies | the repository's own `docs/` output |
| understand-anything | a knowledge graph over the same repository: **impact analysis before a change**, a **domain and business-flow view**, and **onboarding tours** | `.ua/` in the working tree |

The one that answers "what does changing this break?" is understand-anything. On a codebase nobody
has mapped yet, the tour is also the cheapest way for a new contributor — or a colony — to be shown
the shape of it.

## Switching it on

Settings → Skillsets lists it with a Download button. Downloading unpacks the plugin to
`<data>/plugins/understand-anything` and makes it an ordinary skillset; it is **not** enabled by
that. Flip its switch (the Claude Code module's `plugins` setting, a comma-separated list of names,
or an org's `agent.skillsets` override) and new colonies mount it read-only at
`/opt/colonizer/plugins/understand-anything`.

The same two endpoints graft has serve it, by name:

| | |
| --- | --- |
| Status | `GET /api/plugins/understand-anything` — `{name, release, installed_release, state, bytes, total, started_at, finished_at, error}`, where `state` is `idle`, `downloading`, `unpacking`, `installed`, `failed`, `unavailable` or `local` (an operator's own `plugins/understand-anything` directory is used instead) |
| Download | `POST /api/plugins/understand-anything/download` starts or joins the download and returns the same object at once; 409 for `local` |
| Listing | `GET /api/plugins` carries the same object in its `downloadable` array |

All three need an owner token.

## Its pin

`crates/colonizer/understand-anything.lock` pins one upstream release as a single row — name,
version (the upstream tag), platform `any`, kind `source`, sha256 and the tarball URL:

```
understand-anything  v2.9.0  any  source  <sha256>  https://codeload.github.com/Egonex-AI/Understand-Anything/tar.gz/<commit>
```

The URL names the **commit**, not the tag, so a tag that is ever repointed cannot move the bytes
under the checksum. The mothership hashes what it downloads and **fails closed**: a checksum that
does not match unpacks nothing, and the row says why. Nothing adopts a new pin on its own —
`.github/workflows/runtime-pin-updates.yml` runs daily, checks the latest release through GitHub's
API, and opens a pull request (or a stand-in issue) with the old and new commit and checksum. By
hand:

```sh
node scripts/update-runtime-pins.mjs --check           # report what moved, exit 1 when stale
node scripts/update-runtime-pins.mjs --write --summary out.md
```

Upstream's MIT `LICENSE` is copied into the installed plugin folder, next to the plugin it covers.

## Inside a colony

The plugin directory is mounted read-only, and a colony's only network is the model router. So:

- **Writes.** Everything the plugin writes goes to `.ua/` in the working tree. The colony's
  `core.excludesFile` ignores `.ua/` and `.understand-anything/`, so the graph is never part of the
  pull request. It is build output, like `target/`.
- **No network beyond the model router.** `/understand-figma` calls the Figma API and cannot work.
- **No `pnpm install`, no build.** `/understand` and `/understand-dashboard` bootstrap themselves on
  first run with a `pnpm install` and a build into the plugin root, which a read-only mount refuses.
  `/understand-dashboard` also starts a vite server, and needs the install to do it.

## The cost guard

Upstream warns that `/understand` — the full multi-agent pass over the whole repository — is
expensive. A colony is not the place to spend that, so when the plugin is mounted the runner
([modules/agents/claude-code/runner.mjs](../modules/agents/claude-code/runner.mjs)) does two
things:

1. appends a block to the system prompt saying so, and
2. **denies** three skills deterministically, in a `PreToolUse` hook, with a reason that names what
   to use instead: `/understand` (the full pass), `/understand-dashboard` (vite + `pnpm install`)
   and `/understand-figma` (network). Subagents are gated too — the pass is expensive whoever starts
   it.

Everything else stays available, and is what a colony is expected to use:

| Skill | In a colony |
| --- | --- |
| `/understand-explain <file>` | yes — file-scoped, incremental |
| `/understand-diff` | yes — impact of the current diff against the graph |
| `/understand-chat` | yes — ask the graph |
| `/understand-onboard`, `/understand-domain`, `/understand-knowledge` | yes — they read the graph rather than rebuilding it |
| `/understand` | **denied** — the whole-repository pass |
| `/understand-dashboard` | **denied** — needs a network and a writable plugin root |
| `/understand-figma` | **denied** — needs the Figma API |

Where there is no graph in `.ua/` yet, the file-scoped commands build the part they need as they
go; the whole-repository pass belongs to the shared graph below.

## The shared knowledge graph

The shared graph — one built once and handed to every colony — is the one place the full
`/understand` pass runs. Opt in per repository with the org's `ua_graph` setting (`orgs.json`, or
`PUT /api/orgs/{org}`), a list read like `merge_prs`: `owner/name` entries, an org's name for
every repository of it, or `*`. Empty, the default, builds nothing.

The mothership then launches a dedicated **Knowledge graph** colony on the repository (origin
`ua-graph`): it runs `/understand` once in full, then incrementally — resuming from the commit the
stored graph records — whenever a tick, every 10 minutes, sees main move past that graph. A pass
that fails is retried at most once an hour. The finished graph is stored versioned by the commit
it analysed, capped at 32 MB, under `<data>/understand-anything/graphs/<owner>/<name>/<sha>.json`,
beside a `latest.json` pointer.

Every colony on the repository gets the newest graph under `.ua/`:

- its base is the graph's commit: the stored file is bound at `.ua/knowledge-graph.json`
  **read-only**;
- its base is ahead of it (or unreadable): a **writable copy**, seeded with a `meta.json` that
  makes the next pass incremental, plus a note in the brief — answer `/understand-chat` and
  `/understand-explain` from the graph, and with the file-scoped commands refresh only the
  changed files (`git diff <graph>..HEAD`), never a full `/understand`.

Either way `.ua/` stays git-excluded — guest-side as before, and in the bare repository's
`info/exclude`, so a seeded graph never lands in a pull request. The graph colony is the cost
guard's one licence: the boot mounts the skillset for it whatever the org switched on and sets
`COLONIZER_UA_FULL_PASS=1`, standing the `/understand` refusal down for that colony alone (the
dashboard and Figma refusals stand). Its cost runs through the model router like any colony's;
the cockpit's Code page shows it once the repository is opted in — "Open knowledge graph" (the
raw JSON, owner-only), the commit it was built at, "graph jobs: $X"; the same facts come from
`GET /api/understand-anything/graphs/{owner}/{name}`.

None of it runs until the plugin is prebuilt: the pin ships TypeScript source, and `/understand`
executes `packages/core/dist/index.js`. A host-side prebuild — the built dist and its
`node_modules` in the plugin root, tracked by a follow-up issue — is what `launch` and the tick
wait for; until then both refuse, saying exactly that.

## See also

- [skill-packs.md](skill-packs.md#downloadable-skillsets) — what a downloadable skillset is, and
  graft's row as the worked example.
- [protocol/plugins.md](protocol/plugins.md) — plugin directories, validation, resolution.