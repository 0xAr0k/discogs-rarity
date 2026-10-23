//! The binary: parse arguments, wire adapters, render, exit.
//!
//! # How short this file should be
//!
//! This is the whole `main.rs`. If it grows past ~40 lines, something is doing
//! a job that belongs in a module. Specifically:
//!
//! - decisions about *what data means* → `domain`,
//! - decisions about *what to call and in what order* → `app`,
//! - anything requiring `unwrap` → a bug; return a `Result` instead.
//!
//! # The exit-code contract
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0 | Success |
//! | 1 | Failure, message on stderr |
//!
//! Using [`ExitCode`] rather than `process::exit` matters: `process::exit`
//! skips destructors, so a half-written buffer would be lost. Returning an
//! `ExitCode` unwinds normally first.

use std::process::ExitCode;

use clap::Parser;
use diggers_rarity::{
    app,
    cli::Args,
    error::{Error, Result},
    ui::Format,
};

/// Fetch a Discogs collection and report rarity and current value.
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // `{:#}` would flatten the domain cause; a plain message reads
            // better here because `Display` on `Error` already includes it.
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The whole program, as a fallible function.
///
/// # Why this shape
///
/// `main` is untestable — it parses real arguments and exits the process.
/// `run() -> Result<()>` is testable, composable, and is what every other
/// module's tests would have called otherwise.
fn run() -> Result<()> {
    // `.env` first, so clap's `env = "DISCOGS_TOKEN"` sees a file-based token
    // the same way it sees an exported one. A missing file is normal; a bad
    // one leaves the token unset and `Args::parse` reports any consequences.
    let _ = dotenvy::dotenv();

    let args = Args::parse();
    let format = args.output_format();

    // 1. Validate. Everything past this point works with domain types.
    let request = args.into_request()?;

    // 2. Fetch and analyse.
    let report = app::run_discogs(request)?;

    // 3. Render. Serialization failures become a `Result` rather than a panic.
    let output = render(&format, &report)?;

    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    use std::io::Write;
    writeln!(writer, "{output}")
        .map_err(|error| Error::msg(format!("could not write output: {error}")))?;
    writer
        .flush()
        .map_err(|error| Error::msg(format!("could not flush output: {error}")))?;

    Ok(())
}

/// Serialize a report in the requested format.
///
/// A future refactor worth its own decision: move this into [`Format`] with
/// the signature `fn render(&self, report: &Report) -> Result<String>`, so a
/// failing renderer produces a real error message instead of JSON-shaped
/// noise, and use `ui::json::write_to` for the streaming path — buffering the
/// whole report in a `String` costs memory for very large collections.
fn render(format: &Format, report: &app::Report) -> Result<String> {
    Ok(format.render(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use diggers_rarity::{
        adapters::testing::{FakeCollectionSource, FakeSaleSource, FixedClock, RecordingSleeper},
        app::Request,
        domain::{
            collection::Page,
            identity::ReleaseId,
            release::{Artist, Format as MediaFormat, Label, Release},
        },
    };

    fn release(id: u64) -> Release {
        Release {
            id: ReleaseId::new(id).unwrap(),
            title: "Kind of Blue".into(),
            year: Some(1959),
            artists: vec![Artist {
                id: 1,
                name: "Miles Davis".into(),
            }],
            formats: vec![MediaFormat {
                name: "Vinyl".into(),
                qty: "1".into(),
                descriptions: vec!["First Pressing".into()],
            }],
            labels: vec![Label {
                id: 1,
                name: "Columbia".into(),
                catno: "CL 1355".into(),
            }],
            thumb: String::new(),
            resource_url: String::new(),
            country: Some("US".into()),
            master_id: None,
        }
    }

    fn report() -> app::Report {
        let source = FakeCollectionSource::with_pages(vec![Page {
            number: 1,
            total_pages: 1,
            total_items: 1,
            releases: vec![release(101)],
        }]);
        let request = Request::new("arok", 0, 100, 365).unwrap();
        app::run(
            request,
            &source,
            &FakeSaleSource::new(),
            &FixedClock(1_700_000_000),
            &RecordingSleeper::new(),
            std::time::Duration::ZERO,
        )
        .unwrap()
    }

    #[test]
    fn render_produces_output_in_both_formats() {
        let report = report();
        assert!(render(&Format::Text, &report).unwrap().contains("arok"));
        assert!(render(&Format::Json, &report).unwrap().starts_with('{'));
    }

    #[test]
    fn main_can_be_built_without_panicking() {
        // Not a real run: just proves the wiring type-checks end to end.
        let args = Args::try_parse_from(["diggers-rarity", "arok", "--format", "json"]);
        assert!(args.is_ok());
    }
}
