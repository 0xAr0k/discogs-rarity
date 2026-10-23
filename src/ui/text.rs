//! The human-readable renderer.

use crate::app::Report;

/// Render a report as a table a person can read.
///
/// # Design notes
///
/// - Widths are fixed, not computed. A report column that jitters between rows
///   is unreadable, and dynamic padding is a lot of code for little gain.
/// - Unknowns print as the literal word `unknown`. See the module docs on why.
/// - Counts come first because they answer "did this work?" before anyone cares
///   about value.
pub fn render(report: &Report) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "Collection of {} · folder {}\n",
        report.request.username, report.request.folder
    ));
    out.push_str(&format!(
        "Priced as of {}\n\n",
        crate::domain::market::days_between(report.priced_at_unix, report.priced_at_unix)
    ));

    out.push_str(&format!(
        "{:<5} {:<4} {:>10}  {}\n",
        "COPY", "QTY", "VALUE", "PRESSING"
    ));

    for valuation in &report.valuations {
        out.push_str(&format!(
            "{:<5} {:<4} {:>10}  {}\n",
            valuation.release_id,
            valuation.copies,
            valuation.value.map_or_else(
                || "unknown".to_owned(),
                |range| range.midpoint().to_string()
            ),
            rarity_label(valuation),
        ));
    }

    out.push('\n');
    out.push_str(&format!("Unique pressings: {}\n", report.valuations.len()));
    out.push_str(&format!(
        "Copies: {}\n",
        report
            .valuations
            .iter()
            .map(|valuation| valuation.copies)
            .sum::<u32>()
    ));

    // The total is deliberately absent (see `CollectionValuation`): a value
    // without its coverage figure reads as more complete than it is, so until
    // both exist together they print as their honest unknowns.
    out.push_str(&format!(
        "Estimated value: {}\n",
        report
            .collection_total
            .total
            .map_or_else(|| "unknown".to_owned(), |money| money.to_string())
    ));

    out
}

/// The rarity band, or a placeholder when rarity was not assessed.
fn rarity_label(valuation: &crate::domain::valuation::Valuation) -> String {
    valuation.rarity.as_ref().map_or_else(
        || "rarity unknown".to_owned(),
        |rarity| format!("rarity {}", rarity.band().as_str()),
    )
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
            release::{Artist, Format, Label, Release},
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
            formats: vec![Format {
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

    fn report_for(releases: Vec<Release>) -> Report {
        let items = releases.len() as u64;
        let source = FakeCollectionSource::with_pages(vec![Page {
            number: 1,
            total_pages: 1,
            total_items: items,
            releases,
        }]);
        run(
            Request::new("arok", 0, 100, 365).unwrap(),
            &source,
            &FakeSaleSource::new(),
            &FixedClock(1_700_000_000),
            &RecordingSleeper::new(),
            std::time::Duration::ZERO,
        )
        .unwrap()
    }

    #[test]
    fn an_empty_collection_still_renders_a_readable_header() {
        let text = render(&report_for(vec![]));
        assert!(text.contains("Collection of arok"));
        assert!(text.contains("Unique pressings: 0"));
    }

    #[test]
    fn unknown_values_never_print_as_zero() {
        let text = render(&report_for(vec![release(101)]));
        assert!(
            text.contains("unknown"),
            "an unpriced collection must say so explicitly"
        );
        assert!(
            !text.contains("€0.00"),
            "zero would read as worthless, which is a different claim"
        );
    }

    #[test]
    fn duplicate_copies_are_shown_as_a_count() {
        let text = render(&report_for(vec![release(101), release(101), release(102)]));
        assert!(text.contains("Unique pressings: 2"));
        assert!(text.contains("Copies: 3"));
    }
}
