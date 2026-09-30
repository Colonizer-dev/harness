**A built-in "TypeScript: remove any" loop.** Off by default and opt-in per org or repository, it
counts the explicit `any` in each opted-in TypeScript repository on the mothership's mirror (never
inside a colony, and without a model): with the repository's own TypeScript when `node` is on the
host and its dependencies install offline from the host's cache, and with a token scan that skips
comments and strings otherwise, saying which method ran. Once per run and repository, the module
with the most `any` goes to one colony as a batch of at most 20 `file:line` occurrences, with rules
against casts, `@ts-ignore`, `eslint-disable` and new `any`; an open colony on the module, the
per-repository cooldown, the per-run cap and `COLONIZER_NO_EXTERNAL_EFFECTS` hold a dispatch back.
When the batch's pull request is published it is counted again, and a module count that did not
drop or added suppressions raise an attention item. Every run's totals, trend, dispatches and skips
are on the Loops page and in the activity log, and a dry run writes nothing. See
[docs/loops.md](docs/loops.md#typescript-remove-any).
