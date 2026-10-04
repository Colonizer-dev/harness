# TypeScript: remove any

Part of the [Colonizer loops](../loops.md).

A built-in loop that counts the explicit `any` in the TypeScript repositories you opt in and hands
one small batch at a time to a colony that replaces them with real types. It is **off** until you
switch it on *and* add an org (`acme`) or a repository (`acme/app`) to its allowlist; both start
empty. It is on the Loops page, below your own loops, and its settings are in
`<config_dir>/ts-any-loop.json`.

**When it runs.** Daily by default (07:43 UTC); hourly, every 6 hours or weekly from the page, and
never more often than hourly. **Dry run** counts and lists what it would dispatch, without starting
a colony or saving anything; **Run now** does a real run.

**How it counts.** On the mothership, never in a colony, and without a model: the TypeScript
sources (`.ts`, `.tsx`, `.mts`, `.cts`) at each repository's default branch, read from the
mothership's own mirror, in any repository with a `tsconfig.json`. `node_modules`, `dist`, `build`,
`out`, `coverage` and `vendor` are left out. Two methods, and every report says which one ran and
why:

- **The repository's own TypeScript**, when `node` is on the host and `node_modules/typescript` can
  be had: it is not in the mirror, so the loop installs the repository's dependencies into a scratch
  copy with its own package manager (`npm ci`, `pnpm install`, `yarn install`) *offline* — only what
  the host's package cache already holds, with install scripts off. A parse with its compiler API
  then finds every `any` keyword. With **Also count implicit any** ticked, a program built from each
  tsconfig with `noImplicitAny` counts what that flag would report as well (reported, never
  dispatched). Switch **Install offline** off to skip the install.
- **A token scan** otherwise (no `node`, a bun lockfile — bun has no offline-only install — a cache
  that lacks packages, or TypeScript 7, which has no JavaScript compiler API). It skips comments,
  strings, template text and regular expressions, and reads the same forms from the tokens around
  each `any`. It is close to the parse but not exact: an identifier named `any` in an odd place can
  fool it.

Both count `: any`, `as any`, `<any>x`, `Foo<any>`, `any[]`, `Array<any>`, `Record<string, any>`,
generic defaults (`<T = any>`) and `any` elsewhere in a type (`string | any`, `() => any`, `type X =
any`), per file and per module (a file's directory). Each real run keeps the totals, so the page
draws the trend and each report says how the count moved since the last run.

**How it fixes.** One colony per repository per run, with a small batch: the module with the most
explicit `any`, and its first 20 occurrences (the batch size is a setting). The brief lists each
`file:line:column` with its source line and the rules: replace each with a real type, `unknown`
plus narrowing, or a generic; never an `as` cast to silence an error, `// @ts-ignore`,
`// @ts-expect-error`, `// eslint-disable` or a new `any`; no change in behaviour; the repository's
type check (`tsc --noEmit` or `tsc -b`) and tests must pass; keep the diff small. Its origin is
`ts-any:<module>`, and its title `TypeScript: remove any in <module> (20 of 57)`.

A batch is **not** dispatched, and the report says why, when:

- `COLONIZER_NO_EXTERNAL_EFFECTS` (or `COLONIZER_NO_WRITE`) is set: the run reports only;
- a colony on the same repository and module is still live, queued or parked, or has its pull
  request open: the next module down goes instead;
- the repository is cooling down: 20 hours after its last dispatch by default;
- the run has reached its cap: 3 colonies per run by default, never more than one per repository.
  Repositories are counted one at a time, so a run never bursts.

**The post-check.** Once a batch colony has published its pull request, the next run checks out its
branch from the mirror and counts again (at most three such recounts per run). The batch is flagged
when the module's explicit `any` did not drop, or when suppression comments (`@ts-ignore`,
`@ts-expect-error`, `@ts-nocheck`, `eslint-disable`), `as` casts or `any` outside the module were
added. A flagged batch is listed in the report and stays an attention item on the page while its
record is kept (30 days).

**The report.** Every run records the totals, the busiest modules and files, the trend, what it
dispatched and what it skipped and why, and any recount. The last report, a small trend line and
the history of runs are on the Loops page, and each run and each dispatch is a `loop.ts_any` line in
the activity log. Its routes are `GET/PUT /api/ts-any-loop` and `POST /api/ts-any-loop/run` (see
[protocol.md](../protocol/loops.md)).
