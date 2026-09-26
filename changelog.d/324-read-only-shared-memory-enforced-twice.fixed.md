- **Read-only shared memory, enforced twice.** Subagents and background tasks can search shared memory but never
  propose: the runner's hook already refused `memory_propose` for any agent but the orchestrator, and now the
  mothership re-checks each proposal's `origin` before it touches a store — with the mem0 provider, a refused
  proposal is never sent upstream. Proposals record who made them (`source.origin` beside `source.session_id`,
  shown in review), the access matrix and what is *not* implemented (automatic extraction of memories from
  conversation turns) are documented. ([#324])

[#324]: https://github.com/Colonizer-dev/harness/issues/324
