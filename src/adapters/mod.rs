//! Adapters: the implementations that touch the outside world.
//!
//! Everything here is allowed to be awkward. HTTP status codes, JSON shapes,
//! header parsing, token handling, real sleeping. The discipline that keeps it
//! from spreading is simple: **an adapter translates, it does not decide.**
//!
//! - No valuation logic here.
//! - No coherence rules here — see [`crate::domain::collection::PageSet`].
//! - Anything an adapter decides on its own must be written down as a named
//!   constant or a doc comment, because nobody will read this file otherwise.

pub mod discogs;
pub mod system;
pub mod wantlist;

// Test doubles are compiled unconditionally so that integration tests and
// example code can use them, not only the unit tests inside this crate.
pub mod testing;

pub use discogs::{DiscogsCatalogSource, DiscogsCollectionSource};
pub use system::{SystemClock, ThreadSleeper};
pub use wantlist::JsonWantlist;
