---
name: graft
description: Find, understand and scope changes in this repository through its code map before grepping or reading files. Use for "where is X", "how does Y work", "who calls Z", "what does this change break", or any refactor that touches more than one file.
---

# graft: the repository's code map

`/opt/colonizer/plugins/graft/bin/graft` answers from a graph of this repository — every file, symbol and
call edge — with exact `file:line`. The first call builds the graph (about a minute on a large repository);
later calls keep it in sync with your edits. It runs offline: no model, no key, nothing leaves the colony.
The graph lives outside the worktree, so it is never committed.

Pick the one command that fits and act on its answer. Most tasks need a single call.

| You need | Run |
| --- | --- |
| Locate and understand ("how does X work", "where is Y") | `graft ask "<task in plain words>" --source` |
| Every occurrence of a literal, grouped by enclosing symbol | `graft grep "<literal>"` |
| Who calls a symbol / what it calls | `graft callers <symbol>` / `graft callers <symbol> --direction out` |
| Everything a change could break | `graft callers <symbol> --depth all` |
| A file's whole API (signatures and spans) | `graft skeleton <file>` |
| Orientation in an unfamiliar repository | `graft map` |

(`graft` above is `/opt/colonizer/plugins/graft/bin/graft`.)

Rules:

- Start with `graft ask … --source` before `grep -r` or opening files; open a file only to edit the span it names.
- Before a rename or a change to a shared function, run `graft callers <symbol> --depth all` and change every
  file it lists — editing the primary file and stopping is the classic miss.
- Never pipe graft through `head` or `tail`: its output is already capped, and clipping drops the part you need.
- Do not re-ask the same question reworded; switch to the command that fits the next need.
- If graft reports an error or the repository has no supported language, fall back to ordinary search.
