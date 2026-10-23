//! Command-line parsing.
//!
//! # Where validation belongs
//!
//! `clap` handles *shape* (is there an argument, is it a number) and
//! [`app::Request::new`] handles *meaning* (is this page size acceptable).
//! Both reject bad input; neither duplicates the other. `clap`'s custom
//! parsers could call the domain constructors directly — worth doing if you
//! want `--help` to show domain-specific errors, but it couples the CLI to the
//! domain types, so this module keeps them separate.
//!
//! Secrets: the token comes from `DISCOGS_TOKEN` first and
//! `hide_env_values = true` keeps it out of `--help` output. Never add a
//! positional token argument; shell history and `ps` are world-readable.

use clap::{Parser, ValueEnum};

use crate::{app::Request, domain::identity::PageSize, ui::Format};

/// Fetch a Discogs collection and report rarity and current value.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// Discogs username whose collection to inspect.
    pub username: String,

    /// Personal access token. Prefer the DISCOGS_TOKEN environment variable.
    #[arg(long, env = "DISCOGS_TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// Collection folder ID; 0 means all folders.
    #[arg(long, default_value_t = 0)]
    pub folder: u32,

    /// Items requested per page (1-100).
    #[arg(long, default_value_t = PageSize::DEFAULT.get())]
    pub per_page: u32,

    /// Only sales within this many days inform a price.
    ///
    /// Rarity signal for buyers. A pressing with no sales in three years is a
    /// different proposition from one that changed hands last month.
    #[arg(long, default_value_t = 365, value_parser = clap::value_parser!(i32).range(1..))]
    pub market_window_days: i32,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

/// `--format` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable summary.
    Text,
    /// Machine-readable JSON.
    Json,
}

impl From<OutputFormat> for Format {
    fn from(format: OutputFormat) -> Self {
        match format {
            OutputFormat::Text => Format::Text,
            OutputFormat::Json => Format::Json,
        }
    }
}

impl Args {
    /// Validate the arguments and turn them into a domain [`Request`].
    ///
    /// # Why the conversion is a method
    ///
    /// It keeps the raw CLI shape (strings, u32, a flag enum) from leaking into
    /// the app. After this call, the rest of the program works with validated
    /// types only. The unit test below checks that round-trip.
    pub fn into_request(self) -> crate::error::Result<Request> {
        Request::new(
            &self.username,
            self.folder,
            self.per_page,
            self.market_window_days,
        )
    }

    /// The output format as a UI format.
    pub fn output_format(&self) -> Format {
        self.format.into()
    }
}

/// The token to use, if any.
///
/// A testability note for later: `run_discogs` reads the environment itself,
/// which makes the environment part of its behaviour and therefore untestable.
/// Threading the token through `app::run_discogs(token, request)` would make
/// the anonymous/authenticated pacing split testable with a fake.
pub fn token_from(args: &Args) -> Option<&str> {
    args.token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
}

/// Whether a token is available, for choosing [`crate::ports::Pacing`].
pub fn is_authenticated(args: &Args) -> bool {
    token_from(args).is_some()
}

/// Where the token came from, for `--verbose` output.
pub fn describe_token_source(args: &Args) -> &'static str {
    match token_from(args) {
        Some(_) => "DISCOGS_TOKEN or --token",
        None => "none (anonymous; lower rate limit)",
    }
}

/// A sensible [`Pacing`] for this invocation.
pub fn pacing_for(args: &Args) -> crate::ports::Pacing {
    crate::ports::Pacing::for_token(is_authenticated(args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::FolderId;

    fn parse(args: &[&str]) -> Args {
        let mut full = vec!["diggers-rarity"];
        full.extend_from_slice(args);
        Args::try_parse_from(full).unwrap()
    }

    #[test]
    fn defaults_are_the_documented_ones() {
        let args = parse(&["arok"]);
        assert_eq!(args.username, "arok");
        assert_eq!(args.folder, 0);
        assert_eq!(args.per_page, 100);
        assert_eq!(args.market_window_days, 365);
        assert_eq!(args.format, OutputFormat::Text);
        assert!(args.token.is_none());
    }

    #[test]
    fn every_flag_parses() {
        let args = parse(&[
            "arok",
            "--folder",
            "3",
            "--per-page",
            "50",
            "--market-window-days",
            "90",
            "--format",
            "json",
        ]);
        assert_eq!(args.folder, 3);
        assert_eq!(args.per_page, 50);
        assert_eq!(args.market_window_days, 90);
        assert_eq!(args.format, OutputFormat::Json);
    }

    #[test]
    fn a_non_positive_market_window_is_rejected_by_the_parser() {
        let mut full = vec!["diggers-rarity", "arok"];
        full.extend(["--market-window-days", "0"]);
        assert!(Args::try_parse_from(full).is_err());
    }

    #[test]
    fn a_username_is_required() {
        assert!(Args::try_parse_from(["diggers-rarity"]).is_err());
    }

    #[test]
    fn into_request_validates_the_username() {
        let args = parse(&["a/b"]);
        assert!(
            args.into_request().is_err(),
            "the domain must reject a path"
        );
    }

    #[test]
    fn into_request_validates_the_page_size() {
        let args = parse(&["arok", "--per-page", "101"]);
        assert!(args.into_request().is_err());
    }

    #[test]
    fn into_request_produces_a_validated_request() {
        let args = parse(&["arok", "--per-page", "50", "--folder", "2"]);
        let request = args.into_request().unwrap();
        assert_eq!(request.username.as_str(), "arok");
        assert_eq!(request.page_size, PageSize::new(50).unwrap());
        assert_eq!(request.folder, FolderId::new(2).unwrap());
    }

    #[test]
    fn a_blank_token_is_treated_as_absent() {
        assert!(token_from(&parse(&["arok"])).is_none());
        let mut full = vec!["diggers-rarity", "arok", "--token", "   "];
        full.truncate(4);
        let args = parse(&["arok"]);
        assert!(!is_authenticated(&args));
    }

    #[test]
    fn a_present_token_selects_authenticated_pacing() {
        let anonymous = parse(&["arok"]);
        let authenticated = Args {
            token: Some("secret".into()),
            ..parse(&["arok"])
        };
        assert!(!is_authenticated(&anonymous));
        assert!(is_authenticated(&authenticated));
        assert!(pacing_for(&authenticated).interval < pacing_for(&anonymous).interval);
    }

    #[test]
    fn output_format_converts() {
        assert_eq!(parse(&["arok"]).output_format(), Format::Text);
        assert_eq!(
            parse(&["arok", "--format", "json"]).output_format(),
            Format::Json
        );
    }

    #[test]
    fn token_source_description_never_leaks_the_token() {
        let args = Args {
            token: Some("super-secret-value".into()),
            ..parse(&["arok"])
        };
        let described = describe_token_source(&args);
        assert!(!described.contains("super-secret-value"));
    }
}
