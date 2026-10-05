//! Checks that need the whole repository, not one crate.
//!
//! A published crate's own tests have to stay self-contained — they run from a crate tarball, with
//! no repository around them — so a check that reads outside `crates/colonizer` (docs, agent module
//! manifests, `scripts/` fixtures, another crate's data) cannot live in `colonizer-harness`'s
//! tests. It lives here instead, in a crate that is never published. This crate links
//! `colonizer-harness` as a library and reaches its code through the `#[doc(hidden)]` `contract`
//! module, which is a surface for these checks and not a stable API.
//!
//! Each module below holds the checks moved out of the source module it is named for, exercising
//! the docs ↔ code, schemas ↔ types, module manifests ↔ presets and shared-fixture seams.

#[cfg(test)]
mod agentd;
#[cfg(test)]
mod boundary;
#[cfg(test)]
mod events;
#[cfg(test)]
mod fleet_export;
#[cfg(test)]
mod graft;
#[cfg(test)]
mod modules;
#[cfg(test)]
mod presets;
#[cfg(test)]
mod protocol;
#[cfg(test)]
mod spend;
