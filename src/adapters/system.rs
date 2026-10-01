//! Real-clock and real-sleep adapters.
//!
//! Two lines of code, but they exist as a *pair* on purpose: whatever needs the
//! clock or a pause takes them as arguments, so nothing else in the crate
//! calls `SystemTime::now` or `thread::sleep` directly. Grep for those two
//! symbols; they should only appear here.

use std::{
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::ports::{Clock, Sleeper};

/// The actual system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        // `duration_since` fails only for times before 1970, which cannot
        // happen here; `unwrap_or_default` keeps this total instead of panicking.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_secs() as i64)
            .unwrap_or_default()
    }
}

/// Actually sleeps the current thread.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}
