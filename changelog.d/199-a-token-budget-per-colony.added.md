- **A token budget per colony.** A new `budget_tokens` sandbox setting caps the tokens one colony
  routes through the gateway, counted for every routed response whether or not the provider prices it —
  a prepaid token or coding plan prices nothing, so its colonies cost $0 and the USD budget can never
  trip; this is what holds them. Enforced exactly like `budget_usd`: past the budget the colony is
  stopped with its worktree kept, its next routed request is refused with `403`, and raising the budget
  and pressing Resume continues. It is global, with no per-org override; `0`, the default, means
  unlimited. ([#199])

[#199]: https://github.com/Colonizer-dev/harness/issues/199
