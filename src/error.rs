//! One error type for the whole crate.
//!
//! # Why not just use `anyhow` everywhere?
//!
//! Because `anyhow::Error` erases the cause into a string. The moment the
//! domain starts returning `anyhow::Error`, three things break at once:
//!
//! - `domain::…` can no longer be unit-tested against *specific* failure
//!   modes (you can only assert "it errored"),
//! - `match` on the reason becomes impossible, so callers can only print,
//! - the layering is silently violated: `anyhow` pulls in `std::error`,
//!   `Box<dyn …>`, backtrace plumbing — all things a pure domain should not
//!   need.
//!
//! So the domain owns [`DomainError`]: a **closed enumeration**. It cannot grow
//! a hidden variant by accident, `match` over it is checked for
//! exhaustiveness, and there is no stringly-typed soup to grep. Adapters and
//! app code wrap their failures into [`Error`], which keeps the reason around.
//!
//! This is the "errors as values" half of semantic Rust.

use std::fmt;

/// Why a collection, page set, or value is not usable.
///
/// Closed on purpose: adding a rule here should be a deliberate, visible act.
///
/// # Why not `Eq`
///
/// `DomainError::OutOfRange` carries an `f64`, and `f64` is not `Eq` — it has
/// `NaN`, which is not equal to itself. Deriving `PartialEq` only is the honest
/// choice: an error comparing unequal to itself for that one variant is fine,
/// and lying about it via a wrapper would not be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DomainError {
    // ---- identity ------------------------------------------------------
    /// A release ID was zero; Discogs IDs start at 1.
    ZeroReleaseId,
    /// A username cannot be used as a URL path segment.
    InvalidUsername,
    /// A page size outside `1..=100`, the range Discogs accepts.
    InvalidPageSize,
    /// A folder ID outside the plausible Discogs range.
    InvalidFolder,

    // ---- pagination coherence -----------------------------------------
    /// The server answered with a page number we did not ask for.
    WrongPage { asked: u32, got: u32 },
    /// `pages`/`items` changed between requests, so the fetch is a torn read.
    PaginationDrifted,
    /// A page claimed there were items but returned none.
    EmptyPageWithItems { page: u32 },
    /// Fewer items than the server reported. Partial data is refused.
    ShortCollection { got: u64, expected: u64 },
    /// More items than the server reported.
    OverCollection { got: u64, expected: u64 },
    /// A collection item's `id` disagreed with its nested `basic_information`.
    ItemIdMismatch { page: u32 },

    // ---- valuation -----------------------------------------------------
    /// A rarity or market rule received a value outside its documented domain,
    /// e.g. a negative price or a percentage above 100.
    OutOfRange { what: &'static str, value: f64 },
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroReleaseId => write!(f, "release id 0 is not a valid Discogs id"),
            Self::InvalidUsername => write!(
                f,
                "username must be nonempty and cannot contain `/`, `.`-only paths, \
                 or control characters"
            ),
            Self::InvalidPageSize => write!(f, "page size must be between 1 and 100"),
            Self::InvalidFolder => write!(f, "folder id is not a plausible Discogs folder"),

            Self::WrongPage { asked, got } => {
                write!(
                    f,
                    "asked for page {asked} but the server replied with page {got}"
                )
            }
            Self::PaginationDrifted => write!(
                f,
                "collection changed while it was being paged through; \
                 rerun for a consistent snapshot"
            ),
            Self::EmptyPageWithItems { page } => {
                write!(f, "page {page} reported items but returned none")
            }
            Self::ShortCollection { got, expected } => write!(
                f,
                "collection ended after {got} items but the server reported {expected}; \
                 refusing to export a partial collection"
            ),
            Self::OverCollection { got, expected } => {
                write!(
                    f,
                    "collected {got} items but the server only reported {expected}"
                )
            }
            Self::ItemIdMismatch { page } => write!(
                f,
                "page {page} contains a collection item whose id disagrees \
                 with its nested release"
            ),

            Self::OutOfRange { what, value } => {
                write!(
                    f,
                    "{what} must be a finite non-negative number, got {value}"
                )
            }
        }
    }
}

impl std::error::Error for DomainError {}

/// The crate-wide error: a domain reason, or a human sentence from outside.
#[derive(Debug)]
pub struct Error {
    /// What went wrong, in words a human can read.
    pub message: String,
    /// `Some` when this error came from a rule inside [`crate::domain`].
    pub domain: Option<DomainError>,
}

impl Error {
    /// An error from an adapter or the CLI: a plain sentence.
    pub fn msg(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            domain: None,
        }
    }

    /// An error carrying a precise, matchable domain cause.
    ///
    /// # Example
    /// ```ignore
    /// fn per_page(n: u32) -> Result<u32> { Ok(validate_page_size(n)?) }
    /// ```
    pub fn domain(error: DomainError) -> Self {
        Self {
            message: error.to_string(),
            domain: Some(error),
        }
    }

    /// Prepend human context while keeping the domain cause intact.
    ///
    /// This is how an adapter reports *where* a failure happened while the
    /// domain still owns *what* was wrong.
    pub fn context(mut self, context: impl fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }

    /// Whether retrying the same request unchanged could plausibly help.
    ///
    /// A future `--retries` flag would consult this. Today the honest answer
    /// is "no": this app has no retry loop outside the rate-limit path.
    pub fn is_retryable(&self) -> bool {
        // TODO(you): return true for transport-level failures (timeout,
        // connection reset) once the HTTP adapter tags them. Match on a
        // dedicated `Error::transient(..)` marker rather than sniffing the
        // message string — string matching is how retries go wrong.
        false
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<DomainError> for Error {
    fn from(error: DomainError) -> Self {
        Self::domain(error)
    }
}

impl From<anyhow::Error> for Error {
    fn from(error: anyhow::Error) -> Self {
        Self::msg(format!("{error:#}"))
    }
}

/// Every fallible function in this crate returns this alias.
///
/// Naming it `Result` keeps signatures short and leaves room to introduce a
/// second alias later (e.g. `type IoResult<T>`) without churn.
pub type Result<T, E = Error> = std::result::Result<T, E>;
