//! The digger terminal UI: Elm-style state machine, drawn with ratatui.
//!
//! Layout of the module mirrors the loop:
//!
//! - [`model`] — pure state, messages, effects. No I/O types beyond the key
//!   event wrapper.
//! - [`update`] — the reducer. Pure. Returns effects.
//! - [`view`] — the renderer. Reads state; writes nothing.
//! - [`runtime`] — the loop's wiring: one worker thread owning the ports,
//!   the main thread polling keys at 50 ms and letting the model speak.
//!
//! Everything numeric that is a *policy* (enrichment concurrency, the
//! pacing between requests, the "masters are not openable" opinion) is
//! named here or in `docs/DECISIONS.md`, never hidden in a helper.

pub mod model;
pub mod runtime;
pub mod update;
pub mod view;
