<p align="center">
  <img src="https://img.shields.io/badge/ROLE-BENCH%20FIXTURE-FF6B35?style=flat-square&labelColor=0A0A0B" alt="Role: bench fixture">
  <img src="https://img.shields.io/badge/RUNTIME-NODE%2024-EDEBE6?style=flat-square&labelColor=0A0A0B" alt="Runtime: Node 24">
  <img src="https://img.shields.io/badge/TESTS-NODE%20%2D%2DTEST-EDEBE6?style=flat-square&labelColor=0A0A0B" alt="Tests: node --test">
  <img src="https://img.shields.io/badge/DEPENDENCIES-ZERO-EDEBE6?style=flat-square&labelColor=0A0A0B" alt="Dependencies: zero">
  <img src="https://img.shields.io/badge/GENERATED-DO%20NOT%20EDIT%20HERE-FF6B35?style=flat-square&labelColor=0A0A0B" alt="Generated: do not edit here">
</p>

<p align="center">
  <b>colonizer.dev</b> · BENCH · the repository colonies are measured on
</p>

---

# The bench fixture

A deliberately small JavaScript project. It exists so that Colonizer can be measured: the same tasks, on
the same code, before and after a change to a prompt, a module setting or a model.

Two colonies on two different issues tell you almost nothing. A prompt that reads better may score worse,
and reading one colony's chat will not tell you which. This repository is the fixed ground that makes the
comparison mean something.

## What is here

| Path | What it is |
| :--- | :--- |
| `src/greet.js` | Builds a greeting. Two lines, on purpose |
| `src/cart.js` | Adds up a cart of items |
| `src/*.test.js` | The project's own tests, which every task must leave passing |
| `package.json` | No dependencies, so a colony never waits on a registry |

```sh
npm test      # node --test src/*.test.js
```

## How it is used

Each bench task is filed here as an issue. A colony picks it up, works in its own microVM, and opens a
pull request. A task passes only when all of this holds:

- a pull request was opened;
- a hidden check passes on its branch;
- this repository's own tests still pass;
- no file outside the task's allowed list was touched;
- it asked exactly as many questions as the task expects.

The checks that decide this are **not** in this repository. They live in the harness and are copied onto
the colony's branch afterwards, so an agent cannot write code to satisfy a test it can read.

## Do not edit this repository by hand

Everything here is generated. The files come from `scripts/bench/fixture` in
[Colonizer-dev/harness](https://github.com/Colonizer-dev/harness), and `node scripts/bench.mjs seed`
writes them here along with the issues. Anything committed straight to this repository is overwritten on
the next seed, and a change to the code or the wording can move the scores it produces — which is the one
thing a bench must not do quietly.

Fix things in the harness, under `scripts/bench/`, and re-seed.

## What this is not

Useful software. Nothing here is meant to be imported, published or depended on, and the code is kept thin
so that a task's difficulty comes from the task rather than from the codebase around it.

---

MIT, the same as the harness.
