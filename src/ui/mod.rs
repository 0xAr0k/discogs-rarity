//! Presentation: turning a [`Report`](crate::app::Report) into bytes.
//!
//! # Rules
//!
//! - **No business logic.** If a renderer decides what a number means, that
//!   decision belongs in the domain.
//! - **Every unknown is visible.** An unknown value prints as `unknown`, never
//!   as `0` or `-`. A collector reading "worthless" when the truth is "we could
//!   not find a price" will act on it.
//! - **Format once, at the edge.** `Money`'s `Display`, `f64` precision, string
//!   padding: all rendering, all here.

use crate::app::Report;

pub mod json;
pub mod text;

/// Which rendering to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Human-readable summary.
    Text,
    /// Machine-readable, for piping into other tools.
    Json,
}

impl Format {
    /// Render `report` in this format to a string.
    ///
    /// # Why a string and not a writer
    ///
    /// Returning `String` keeps the renderers pure and lets you assert on
    /// output in a one-line test. Serializing to `io::Write` is a
    /// three-line function in `main` — see `src/main.rs`.
    pub fn render(self, report: &Report) -> String {
        match self {
            Self::Text => text::render(report),
            Self::Json => json::render(report),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        adapters::testing::{FakeCollectionSource, FakeSaleSource, FixedClock, RecordingSleeper},
        app::{Request, run},
        domain::{
            collection::Page,
            identity::ReleaseId,
            release::{Artist, Format as MediaFormat, Label, Release},
        },
    };

    fn sample_release() -> Release {
        Release {
            id: ReleaseId::new(101).unwrap(),
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

    fn report() -> Report {
        let source = FakeCollectionSource::with_pages(vec![Page {
            number: 1,
            total_pages: 1,
            total_items: 1,
            releases: vec![sample_release()],
        }]);
        let request = Request::new("arok", 0, 100, 365).unwrap();
        run(
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
    fn both_formats_render_a_report() {
        let report = report();
        assert!(!Format::Text.render(&report).is_empty());
        assert!(Format::Json.render(&report).starts_with('{'));
    }
}
