//! `diggers-rarity` — a crate organizer for Discogs collections.
//!
//! The goal of this crate is *not* "call an API and print JSON". The goal is
//! to answer two questions about a collection:
//!
//! 1. **How rare is this release?** — see [`domain::rarity`].
//! 2. **What is it worth right now?** — see [`domain::market`] and
//!    [`domain::valuation`].
//!
//! # Layering
//!
//! Read the dependency arrows in this direction, never the other way around:
//!
//! ```text
//!   ui  ──▶  app  ──▶  ports  ◀──  adapters
//!                 └──▶  domain
//! ```
//!
//! - [`domain`] is **pure**: no I/O, no `std::time`, no network, no `unwrap`.
//!   It is where the rules of collecting music live, and it is the part worth
//!   reading, reviewing, and owning.
//! - [`ports`] are **traits**: the vocabulary the application speaks. They are
//!   written in terms of domain types, never in terms of HTTP.
//! - [`adapters`] are the **dirty** implementations: `reqwest`, `std::time`,
//!   JSON parsing.
//! - [`app`] is **orchestration**: it wires ports to domain functions.
//! - [`ui`] is **presentation**: text and JSON rendering only.
//!
//! A dependency pointing the "wrong" way (e.g. `domain` importing `reqwest`)
//! is a bug even if the code compiles. The whole point of the ports seam is
//! that [`domain`] stays compilable, testable, and readable on its own.
//!
//! # Ownership of the unfinished parts
//!
//! [`domain::rarity`], [`domain::market`], and [`domain::valuation`] contain
//! deliberately unimplemented functions marked with `TODO(you)`. Those are
//! yours to write — see `docs/EXERCISES.md` for the prompts, the tests that
//! already gate them, and the hints.

pub mod adapters;
pub mod app;
pub mod cli;
pub mod domain;
pub mod error;
pub mod ports;
pub mod ui;

/// Crate version, used for the HTTP `User-Agent` and the CLI banner.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
