# 6.11 Fleet export bundle (#687)

Part of the [Colonizer protocol](../protocol.md).

When a machine joins a fleet (#686) it brings its past with it: the colonies it has run, the logs
behind them, and what they spent. The same format serves on its own as a backup. `colonizer fleet
export` writes this machine's session history, colony logs and spend/usage stats into one bundle;
`colonizer fleet import` reads a bundle back into a data dir. Both run locally off the data dir —
no mothership needs to be running. The fleet-join flow (#686) drives the same format
programmatically: a preview first, then — only once the joining member confirms — a chunked,
resumable transfer.

**The bundle.** A zstd-compressed tar (`.tar.zst`). `manifest.json` is the first entry; every
other entry is a regular file — never a symlink:

```
manifest.json
history/sessions.jsonl                one ImportedSession per line    (category history)
logs/<original_id>/events.jsonl       a colony's event log            (category logs)
logs/<original_id>/harness.jsonl      its harness lines
logs/<original_id>/gateway.jsonl      its gateway lines
logs/<original_id>/transcripts/…      its transcripts, relative paths kept
stats/spend.jsonl                     the top-level journals          (category stats)
stats/provider-usage.json
stats/provider-quota.json
stats/routing.jsonl
stats/activity.jsonl
stats/summary.json                    computed: sessions, by_status, cost_usd, routed_cost_usd,
                                      model_usage, boot_ms_mean
```

Log entries are read from the live colony dir `sessions/<id>/` where it exists, and from the
colony's latest archive revision (#496) when the live dir is gone — a finished colony's logs
travel even after its session files have been folded into the archive.

**manifest.json.** The bundle's first entry, and its table of contents:

```json
{
  "format": "colonizer-fleet-export",
  "version": 1,
  "origin_host": "<host id>",
  "origin_name": "<hostname>",
  "created_at": "<RFC3339>",
  "categories": {
    "history": {"included": true, "count": 12, "from": "<RFC3339|null>", "to": "<RFC3339|null>", "bytes": 1234},
    "logs":    {"included": true, "count": 30, "from": null, "to": null, "bytes": 99999},
    "stats":   {"included": true, "count": 6,  "from": null, "to": null, "bytes": 4567}
  },
  "files": [{"path": "logs/abc/events.jsonl", "category": "logs", "bytes": 123, "sha256": "<hex>"}]
}
```

- `count` is the number of sessions for `history`, the number of files for `logs` and `stats`.
  `from`/`to` are the min `created_at` / max `updated_at` across the exported sessions, either
  null when there is nothing to bound; `bytes` is the category's uncompressed size.
- An excluded category (`--no-logs` and friends) appears with `"included": false` and zeros, so a
  reader can tell "not asked for" from "empty".
- `files` lists every other entry with its size and the sha256 of its raw bytes; the importer
  verifies each before it applies it.
- `origin_host` is the machine's persisted `<config_dir>/host_id` (created on first export) — the
  same id fleet claims carry — and `origin_name` is the hostname.
- The `categories` object is exactly the preview shown before anything is sent — `fleet export
  --preview` prints it (the whole manifest, with `--json`), and so does the join flow's preview
  step.

**What is never in a bundle.** Secrets, of every kind. Export reads only an allowlist of data-dir
paths — the ones above — and of the config dir only its `host_id`: no API token
(`api-token`, `api-tokens.json`), no provider keys, no Claude credential, no colony secrets, no
keychain, no settings (`providers.json`, `orgs.json`, `modules.json`, `claude-accounts.json`). A
session record itself is exported as an allowlist projection, not whole (below).

**ImportedSession.** One JSON object per line of `history/sessions.jsonl`:

```json
{"id": "host-a:c1c9215b", "origin_host": "host-a", "original_id": "c1c9215b",
 "repo": "acme/web", "org": "acme", "issue": 3473, "issue_title": "Wire the method picker",
 "status": "merged", "branch": "colonizer/issue-3473-…", "base": "main",
 "pr_url": "https://github.com/acme/web/pull/12", "pr_opened_at": "<RFC3339|null>",
 "merged_at": "<RFC3339|null>", "summary": "…", "error": null,
 "cost_usd": 1.24, "routed_cost_usd": 0.97, "model_tier": "…",
 "model_usage": {"…": {"input_tokens": 0, "output_tokens": 0, "cache_read_tokens": 0, "cache_write_tokens": 0}},
 "model_routing": {}, "agent": "claude-code", "boot_timing": {},
 "created_at": "<RFC3339>", "updated_at": "<RFC3339>"}
```

- `id` is namespaced `<origin_host>:<original_id>`, so ids from many machines cannot collide in
  one fleet's import; `origin_host` and `original_id` match `^[A-Za-z0-9_-][A-Za-z0-9._-]*$`.
- Only those three are required. Everything else — repo, org, issue, issue_title, status, branch,
  base, pr_url, pr_opened_at, merged_at, summary, error, cost_usd, routed_cost_usd, model_tier,
  model_usage, model_routing, agent, boot_timing, created_at, updated_at, repo_identity — is
  optional and nullable; an exporter may leave out what its records never held.
- `repo_identity` (#763) is `{"roots": ["<sha>", …], "url": "<host/path>|null"}`, read from the
  exporting machine's mirror of `repo`: the root commit SHA(s), sorted, and the origin URL
  normalised (scheme, `user@`/token, port, trailing `/` and `.git` removed; host lowercased;
  github.com paths lowercased too, since GitHub treats them case-insensitively). The raw remote
  never travels. Fleet members match repositories by root first, then URL, and treat more than
  one candidate as no match.
- The machine-readable shapes — the manifest at the top level, `imported_session`, `chunk` and
  `import_cursor` under `$defs` — are in
  [fleet-export.schema.json](../fleet-export.schema.json) (draft 2020-12).

**Import layout and idempotency.** `fleet import` (and the fleet applying a member's transfer)
writes under `<data_dir>/fleet-imports/<origin_host>/`:

```
sessions.json          map of namespaced id → ImportedSession; re-import replaces by id
logs/<original_id>/…   as bundled
stats/…                as bundled
manifest.json          the manifest of the import that produced this tree
cursor.json            the ImportCursor, per file, for the resumable transfer
```

- Every entry path is validated before it touches the disk: not absolute, no `..`, and only under
  `history/`, `logs/`, `stats/` or `manifest.json` itself — the same wall the session store's
  file names have.
- Each file's sha256 is verified against the manifest. `.json` snapshot files (`sessions.json`,
  the `stats/*.json`) are replaced atomically, whole. `.jsonl` files resume by the cursor (below).
  `history/sessions.jsonl` folds into `sessions.json` by id, so importing the same session twice
  replaces the record and never duplicates it.
- An import is cancellable, and a partial import is valid: the cursor keeps what landed, and the
  next run of the same import resumes it.

**Chunks.** The transfer moves one file as `Chunk` messages:

```json
{"path": "logs/abc/events.jsonl", "offset": 65536, "raw_len": 65536, "total": 200704,
 "data": "<base64 of the zstd-compressed raw bytes>"}
```

`offset`, `raw_len` and `total` count raw (decompressed) bytes; `data` carries the chunk's raw
bytes zstd-compressed. The receiver keeps one `ImportCursor` per member at
`<data_dir>/fleet-imports/<origin_host>/cursor.json`:
`{"origin_host": "<host id>", "updated_at": "<RFC3339|null>", "files": {"<path>": {"offset":
<committed bytes>, "prefix_sha256": "<hex of those bytes>"}}}` — one `files` entry per path. Per
chunk:

- `offset` equals the committed offset: append, and advance the cursor.
- The chunk lies wholly below the committed offset: a duplicate (a re-sent tail) — ignored.
- `offset` is above the committed offset: a gap — error, write nothing.
- A resumed transfer starts from the cursor's offset, not from zero.
- If the first `offset` bytes of a re-sent file no longer hash to `prefix_sha256` — the source
  rotated or rewrote the file mid-transfer — the file restarts from 0 under the new bytes.

**The join hook.** The join dialog (#686) drives four calls, and nothing is sent before the
member confirms:

1. `preview(categories)` answers the manifest — its `categories` object carries the count, time
   range and size per category (`preview_for` names the origin explicitly).
2. The member reviews it and confirms, with per-category switches; history, logs and stats are on
   by default.
3. The member streams its files with `chunks_for`.
4. The fleet applies each chunk with `apply_chunk`, keeping one cursor per member; a whole bundle
   moves at once with `import_bundle`, which chunks internally through the same cursor.

After the import, new colonies stream live, so the import is only the backfill of what happened
before the join.

**Versioning.** An importer rejects a `format` other than `colonizer-fleet-export`, and a
`version` greater than it supports; version 1 is this document. Two additions are planned and
deliberately *not* in version 1: optional categories (approved memory notes, loops and schedules,
repo claims — off by default) and the cockpit's origin-host marking of imported colonies.

---
