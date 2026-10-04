//! The observability exporter's lowest layer (issue #842): a rotation- and truncation-safe jsonl
//! tailer with a durable cursor. Nothing calls it yet — the multi-source tailer (#843) wires it
//! into the running app — so outside the tests it is dead code.
#![cfg_attr(not(test), allow(dead_code))]

pub(crate) mod cursor;
pub(crate) mod state;
