//! The domain: everything that is true about crates, releases, and value,
//! regardless of where the data came from or who asked for it.
//!
//! Each submodule is one *domain*, and within a domain you will find two kinds
//! of thing:
//!
//! - **types** that name a concept and refuse to hold nonsense, and
//! - **pure functions** `A -> B` that state a rule.
//!
//! Rules of the road for everything in here:
//!
//! 1. No `std::fs`, no `reqwest`, no `println!`, no `thread::sleep`.
//! 2. No `unwrap`, `expect`, or `panic!` on external input — invalid input is
//!    a `DomainError`.
//! 3. Functions take references or owned values and *return* results; they do
//!    not mutate arguments. If you need a "changed" version, return a new
//!    value. That is what makes each rule independently testable.
//!
//! If a change to this module makes you want to `use crate::adapters::…`, the
//! rule belongs somewhere else.

pub mod browse;
pub mod collection;
pub mod identity;
pub mod market;
pub mod rarity;
pub mod release;
pub mod valuation;
