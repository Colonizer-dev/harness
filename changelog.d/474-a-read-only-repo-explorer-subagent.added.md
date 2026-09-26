- **A read-only `repo-explorer` subagent.** A new first-party agent alongside Explore, always
  available regardless of `subagent_effort`: before grepping, it checks the Skill tool for a shipped
  retrieval skill (starting with graft's code map) and prefers it, falling back to find/grep like
  Explore when none applies. First slice of #474 — pinning ast-grep, ast-outline and fff as skills of
  their own, and a bench comparison of read/search token share with and without them, are follow-up
  work. ([#474])

[#474]: https://github.com/Colonizer-dev/harness/issues/474
