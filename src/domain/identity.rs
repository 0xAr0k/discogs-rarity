//! Value objects: the identities this app reasons about.
//!
//! # Why newtypes instead of `u64`
//!
//! `fn value(release_id: u64, folder: u64, per_page: u64)` compiles happily and
//! then silently queries the wrong thing when two arguments get swapped. There
//! is no type error, no test failure message that points at the cause, just
//! garbage data hours later.
//!
//! A newtype makes that mistake *impossible to express*:
//!
//! ```ignore
//! let id: ReleaseId = ...;
//! let folder: FolderId = ...;
//! client.collection(&user, folder, id); // ✗ type mismatch, caught by rustc
//! ```
//!
//! # Why parsing lives in `TryFrom<…>`
//!
//! The constructor is the only place a rule can be broken, so it is the only
//! place a rule needs to be checked. After that the rest of the program can
//! assume validity — that assumption *is* the value of the newtype.
//!
//! The `const fn` constructors matter too: they are checkable at compile time,
//! so a bound like `MAX_PAGE_SIZE` can never drift out of sync with the
//! validator.

use std::{fmt, str::FromStr};

use crate::error::DomainError;

/// A Discogs release ID. Never zero: Discogs IDs start at 1, so 0 is free to
/// mean "absent".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReleaseId(u64);

impl ReleaseId {
    /// Smallest legal Discogs ID.
    pub const FIRST: Self = Self(1);
    /// The largest ID we will accept. Discogs is far below this; the bound
    /// exists so the type has a stated domain rather than "whatever u64 is".
    pub const MAX: u64 = 1_000_000_000;

    /// Compile-time-checked construction.
    ///
    /// Note the `const fn`: `ReleaseId::new(0)` in a `const` context is a
    /// compile error, not a runtime one.
    pub const fn new(id: u64) -> Option<Self> {
        if id == 0 || id > Self::MAX {
            None
        } else {
            Some(Self(id))
        }
    }

    /// The ID as a plain number, for the wire.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for ReleaseId {
    type Error = DomainError;

    fn try_from(id: u64) -> Result<Self, Self::Error> {
        Self::new(id).ok_or(DomainError::ZeroReleaseId)
    }
}

impl From<ReleaseId> for u64 {
    fn from(id: ReleaseId) -> Self {
        id.0
    }
}

impl fmt::Display for ReleaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A Discogs artist ID. Same rules as [`ReleaseId`]: never zero, Discogs
/// starts at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtistId(u64);

impl ArtistId {
    pub const FIRST: Self = Self(1);
    pub const MAX: u64 = ReleaseId::MAX;

    pub const fn new(id: u64) -> Option<Self> {
        if id == 0 || id > Self::MAX {
            None
        } else {
            Some(Self(id))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for ArtistId {
    type Error = DomainError;

    fn try_from(id: u64) -> Result<Self, Self::Error> {
        Self::new(id).ok_or(DomainError::ZeroReleaseId)
    }
}

impl From<ArtistId> for u64 {
    fn from(id: ArtistId) -> Self {
        id.0
    }
}

impl fmt::Display for ArtistId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A Discogs label ID. Same rules as [`ReleaseId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LabelId(u64);

impl LabelId {
    pub const FIRST: Self = Self(1);
    pub const MAX: u64 = ReleaseId::MAX;

    pub const fn new(id: u64) -> Option<Self> {
        if id == 0 || id > Self::MAX {
            None
        } else {
            Some(Self(id))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for LabelId {
    type Error = DomainError;

    fn try_from(id: u64) -> Result<Self, Self::Error> {
        Self::new(id).ok_or(DomainError::ZeroReleaseId)
    }
}

impl From<LabelId> for u64 {
    fn from(id: LabelId) -> Self {
        id.0
    }
}

impl fmt::Display for LabelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A collection folder ID. Zero is meaningful here — it means *all folders* —
/// so unlike [`ReleaseId`] it is allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FolderId(u32);

impl FolderId {
    /// The sentinel meaning "every folder in the collection".
    pub const ALL: Self = Self(0);

    /// Compile-time-checked construction.
    pub const fn new(id: u32) -> Option<Self> {
        // Discogs folder IDs are positive; only `ALL` uses 0.
        if id > 100_000 { None } else { Some(Self(id)) }
    }

    /// Whether this selector spans the whole collection.
    pub const fn is_all(self) -> bool {
        self.0 == 0
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for FolderId {
    type Error = DomainError;

    fn try_from(id: u32) -> Result<Self, Self::Error> {
        Self::new(id).ok_or(DomainError::InvalidFolder)
    }
}

impl From<FolderId> for u32 {
    fn from(folder: FolderId) -> Self {
        folder.0
    }
}

impl fmt::Display for FolderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_all() {
            f.write_str("all")
        } else {
            write!(f, "{}", self.0)
        }
    }
}

/// How many items to ask for per request. Discogs caps this at 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PageSize(u32);

impl PageSize {
    pub const MIN: u32 = 1;
    pub const MAX: u32 = 100;
    /// The default: one request per 100 items.
    pub const DEFAULT: Self = Self(Self::MAX);

    /// Compile-time-checked construction.
    pub const fn new(size: u32) -> Option<Self> {
        if size >= Self::MIN && size <= Self::MAX {
            Some(Self(size))
        } else {
            None
        }
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for PageSize {
    type Error = DomainError;

    fn try_from(size: u32) -> Result<Self, Self::Error> {
        Self::new(size).ok_or(DomainError::InvalidPageSize)
    }
}

impl Default for PageSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for PageSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A Discogs username, validated for use as a single URL path segment.
///
/// # Why the validation looks paranoid
///
/// The username becomes a URL path segment. A value of `../../users` would let
/// a CLI argument steer the request somewhere unintended. Rejecting path
/// separators, control characters, and empty segments turns that from "we
/// should probably validate this" into "this type is a valid path segment,
/// by construction".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Username(String);

impl Username {
    /// Longest username Discogs permits.
    pub const MAX_LEN: usize = 64;

    /// Whether `raw` is acceptable as a username.
    ///
    /// Kept separate from the constructor so the CLI can validate *before*
    /// building the value, and so tests can check the rule without allocating.
    pub fn validate(raw: &str) -> Result<(), DomainError> {
        let trimmed = raw.trim();
        let forbidden = trimmed.is_empty()
            || trimmed == "."
            || trimmed == ".."
            || trimmed.len() > Self::MAX_LEN
            || trimmed.chars().any(char::is_control)
            || trimmed.contains('/')
            || trimmed.contains('\\');
        if forbidden {
            Err(DomainError::InvalidUsername)
        } else {
            Ok(())
        }
    }

    /// Construction is infallible *given* a successful [`Self::validate`], so
    /// there is exactly one rule and one place it is written down.
    pub fn new(raw: &str) -> Result<Self, DomainError> {
        Self::validate(raw)?;
        Ok(Self(raw.trim().to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Username {
    type Err = DomainError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::new(raw)
    }
}

impl fmt::Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_id_rejects_zero_and_absurd_values() {
        assert!(ReleaseId::new(0).is_none());
        assert!(ReleaseId::new(ReleaseId::MAX + 1).is_none());
        assert_eq!(ReleaseId::new(1).unwrap().get(), 1);
        assert_eq!(ReleaseId::try_from(0), Err(DomainError::ZeroReleaseId));
    }

    #[test]
    fn release_id_first_is_the_smallest_legal_id() {
        assert_eq!(ReleaseId::FIRST.get(), 1);
        assert!(ReleaseId::new(0).is_none(), "0 must not be constructible");
    }

    #[test]
    fn folder_zero_means_all_and_prints_human_readably() {
        assert!(FolderId::ALL.is_all());
        assert_eq!(FolderId::ALL.to_string(), "all");
        assert!(!FolderId::new(3).unwrap().is_all());
        assert_eq!(FolderId::new(3).unwrap().to_string(), "3");
    }

    #[test]
    fn page_size_bounds_match_the_documented_range() {
        assert!(PageSize::new(0).is_none());
        assert!(PageSize::new(101).is_none());
        assert!(PageSize::new(1).is_some());
        assert!(PageSize::new(100).is_some());
        assert_eq!(PageSize::default(), PageSize::new(100).unwrap());
    }

    #[test]
    fn username_rejects_anything_that_is_not_a_path_segment() {
        for bad in ["", "  ", ".", "..", "a/b", "a\\b", "a\nb", &"a".repeat(65)] {
            assert!(
                Username::new(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
        assert_eq!(Username::new(" arok ").unwrap().as_str(), "arok");
    }

    #[test]
    fn validate_and_new_agree() {
        // The invariant that matters: `new` never produces a value that
        // `validate` would reject.
        for candidate in ["", ".", "ok", "a/b", "arok"] {
            let built = Username::new(candidate);
            assert_eq!(built.is_ok(), Username::validate(candidate).is_ok());
        }
    }
}
