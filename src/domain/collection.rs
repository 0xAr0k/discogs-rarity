//! The collection domain: the rules for taking pages of data and deciding
//! whether they form one trustworthy snapshot.
//!
//! # The problem this module exists to solve
//!
//! Discogs has no "give me a consistent snapshot" endpoint. You page through
//! `/collection/…/releases` and the collection can change underneath you. If
//! somebody sells a record mid-export, `items` drops from 800 to 799 and you
//! either emit a collection with a phantom entry or silently drop a real one.
//!
//! Both are worse than failing. A tool that says "I could not get a
//! consistent read, try again" is honest; one that emits 799 confident rows
//! while claiming 800 is lying. So every rule here is about *refusing*
//! plausible-looking but inconsistent input.
//!
//! # Functional style, concretely
//!
//! [`PageSet::assemble`] is the shape to copy in this codebase: take a
//! collection of pages, return `Result<Collection>`. No mutation of arguments,
//! no hidden state, no I/O. Every branch is a `match` returning early with a
//! reason. You can test it by handing it three literal structs, and you can
//! read it top-to-bottom and know what it does.

use crate::error::DomainError;

use super::release::{OwnedRelease, Release};

/// One page of collection entries, plus the pagination metadata that came with
/// it.
///
/// `reported_*` fields are the *server's* claim about the whole collection.
/// They are kept verbatim rather than recomputed, because the whole point is
/// to compare our observations against the server's claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// Page number this response claims to be.
    pub number: u32,
    /// Total pages the server claims exist.
    pub total_pages: u32,
    /// Total items the server claims exist.
    pub total_items: u64,
    /// Releases on this page, in server order.
    pub releases: Vec<Release>,
}

/// Every page of one fetch, kept in arrival order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSet {
    pub pages: Vec<Page>,
}

impl PageSet {
    /// Decide whether these pages form one consistent collection snapshot.
    ///
    /// # The checks, and why each exists
    ///
    /// 1. **Non-empty.** Zero pages means the fetch never happened; treat it
    ///    as a failure rather than an empty collection, because an empty
    ///    collection is a claim and this is not one.
    /// 2. **Page numbers are 1..=total_pages with no gaps or repeats.** A gap
    ///    means a lost response, and the data would be silently incomplete.
    /// 3. **Pagination metadata never changes.** If `total_items` shifts
    ///    mid-read, the collection moved under us: `PaginationDrifted`.
    /// 4. **A page that claims items must contain some.** Otherwise a zero-row
    ///    page would look like the end of the collection.
    /// 5. **The final count equals the claimed total.** Refuses both short and
    ///    over reads, which have different causes (a lost page vs. a lying
    ///    server) but the same danger.
    ///
    /// # Example
    /// ```ignore
    /// let pages = PageSet { pages: vec![page_one, page_two] };
    /// let collection = PageSet::assemble(pages)?;
    /// ```
    pub fn assemble(self) -> Result<Collection, DomainError> {
        let pages = self.pages;
        // The server's claims from the first page. Every later page must agree
        // with them, or the collection moved underneath us.
        let mut claimed: Option<(u32, u64)> = None;
        let mut observed = 0u64;

        for (index, page) in pages.iter().enumerate() {
            let asked = u32::try_from(index + 1).unwrap_or(u32::MAX);
            if page.number != asked {
                return Err(DomainError::WrongPage {
                    asked,
                    got: page.number,
                });
            }
            match claimed {
                Some(previous) if previous != (page.total_pages, page.total_items) => {
                    return Err(DomainError::PaginationDrifted);
                }
                _ => claimed = Some((page.total_pages, page.total_items)),
            }
            if page.releases.is_empty() && page.total_items > 0 {
                return Err(DomainError::EmptyPageWithItems { page: page.number });
            }
            observed += page.releases.len() as u64;
        }

        // No pages at all means the fetch never ran; that is a failure, not an
        // empty collection. An empty *collection* arrives as one page claiming
        // zero, which is handled fine below.
        let (total_pages, total_items) = claimed.ok_or(DomainError::ShortCollection {
            got: 0,
            expected: 0,
        })?;
        let fetched = u32::try_from(pages.len()).unwrap_or(u32::MAX);

        // Discogs answers an empty collection with one page claiming
        // `pages = 0, items = 0`. So a single zero-row page is the *empty
        // collection*, not a surplus page — special-case it before comparing
        // page counts, or the check below would reject a legitimate answer.
        let is_empty = total_pages == 0 && total_items == 0 && observed == 0;

        // More pages than the server promised: we asked for something we should
        // not have, and the data is not trustworthy.
        if !is_empty && fetched > total_pages {
            return Err(DomainError::PaginationDrifted);
        }
        // Fewer pages than promised: something was lost in transit. Report it
        // as a short read rather than accepting a truncated collection.
        if !is_empty && fetched < total_pages {
            return Err(DomainError::ShortCollection {
                got: observed,
                expected: total_items,
            });
        }
        if observed < total_items {
            return Err(DomainError::ShortCollection {
                got: observed,
                expected: total_items,
            });
        }
        if observed > total_items {
            return Err(DomainError::OverCollection {
                got: observed,
                expected: total_items,
            });
        }

        Ok(Collection {
            releases: pages.into_iter().flat_map(|page| page.releases).collect(),
            reported_items: total_items,
        })
    }
}

/// A validated, coherent snapshot of one user's collection for one folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    /// Every entry as fetched, including duplicate copies of a pressing.
    pub releases: Vec<Release>,
    /// How many entries the server claimed, which the constructor has already
    /// proven equal to `releases.len()`.
    pub reported_items: u64,
}

impl Collection {
    /// Collapse multiple copies of the same pressing, counting them.
    ///
    /// # Why this is not a `HashMap::dedup`
    ///
    /// Two things matter and a naive `dedup_by_key` gets one of them wrong:
    ///
    /// - **Copies are data.** Owning three copies is worth three times as much
    ///   in market value. Losing the count silently deflates a valuation.
    /// - **Order must survive.** A collection view is read by humans; sorting
    ///   by ID changes which pressing the user sees first. The output keeps
    ///   first-seen order, and the *last* copy's details win (see
    ///   [`OwnedRelease`]).
    ///
    /// `IndexMap`-like behaviour implemented with a `HashMap` + `Vec` pair:
    /// the map gives O(1) lookup, the vec gives insertion order.
    ///
    /// # Example
    /// ```ignore
    /// let unique = collection.dedupe();
    /// assert_eq!(unique.iter().map(|o| o.copies).sum::<u32>(), collection.releases.len() as u32);
    /// ```
    pub fn dedupe(&self) -> Vec<OwnedRelease> {
        let mut positions = std::collections::HashMap::with_capacity(self.releases.len());
        let mut unique: Vec<OwnedRelease> = Vec::with_capacity(self.releases.len());

        for release in &self.releases {
            match positions.get(&release.id) {
                // First sighting: append and remember where it landed.
                None => {
                    positions.insert(release.id, unique.len());
                    unique.push(OwnedRelease {
                        copies: 1,
                        release: release.clone(),
                    });
                }
                Some(&index) => {
                    let owned = &mut unique[index];
                    owned.copies += 1;
                    // Keep the later copy's details; see `OwnedRelease`.
                    owned.release = release.clone();
                }
            }
        }
        unique
    }

    /// Total physical units owned, counting every copy of every pressing.
    pub fn total_units(&self) -> u32 {
        self.releases.iter().map(Release::unit_count).sum()
    }

    /// Number of entries fetched, duplicate copies included.
    pub fn item_count(&self) -> u64 {
        self.releases.len() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::ReleaseId;
    use crate::domain::release::{Artist, Format};

    fn release(id: u64, title: &str) -> Release {
        Release {
            id: ReleaseId::new(id).unwrap(),
            title: title.into(),
            year: Some(1959),
            artists: vec![Artist {
                id: 1,
                name: "Miles Davis".into(),
            }],
            formats: vec![Format {
                name: "Vinyl".into(),
                qty: "1".into(),
                descriptions: vec![],
            }],
            labels: vec![],
            thumb: String::new(),
            resource_url: String::new(),
            country: None,
            master_id: None,
        }
    }

    fn page(number: u32, total_pages: u32, total_items: u64, ids: &[u64]) -> Page {
        Page {
            number,
            total_pages,
            total_items,
            releases: ids.iter().map(|id| release(*id, "x")).collect(),
        }
    }

    fn assemble(pages: Vec<Page>) -> Result<Collection, DomainError> {
        PageSet { pages }.assemble()
    }

    #[test]
    fn a_single_complete_page_assembles() {
        let collection = assemble(vec![page(1, 1, 2, &[101, 102])]).unwrap();
        assert_eq!(collection.item_count(), 2);
        assert_eq!(collection.reported_items, 2);
    }

    #[test]
    fn multiple_pages_assemble_and_flatten() {
        let collection = assemble(vec![page(1, 2, 3, &[101, 102]), page(2, 2, 3, &[103])]).unwrap();
        assert_eq!(
            collection
                .releases
                .iter()
                .map(|r| r.id.get())
                .collect::<Vec<_>>(),
            vec![101, 102, 103]
        );
    }

    #[test]
    fn an_empty_collection_is_accepted() {
        // A genuinely empty collection is a real answer, not an error.
        let collection = assemble(vec![page(1, 0, 0, &[])]).unwrap();
        assert_eq!(collection.item_count(), 0);
    }

    #[test]
    fn no_pages_at_all_is_refused() {
        assert_eq!(
            assemble(vec![]),
            Err(DomainError::ShortCollection {
                got: 0,
                expected: 0
            })
        );
    }

    #[test]
    fn a_gap_in_page_numbers_is_refused() {
        // Page 2 missing: silently dropping it would under-report the collection.
        assert_eq!(
            assemble(vec![page(1, 3, 3, &[101]), page(3, 3, 3, &[103])]),
            Err(DomainError::WrongPage { asked: 2, got: 3 })
        );
    }

    #[test]
    fn an_out_of_order_page_is_refused() {
        assert_eq!(
            assemble(vec![page(2, 2, 2, &[102]), page(1, 2, 2, &[101])]),
            Err(DomainError::WrongPage { asked: 1, got: 2 })
        );
    }

    #[test]
    fn totals_changing_mid_fetch_is_refused_as_drift() {
        assert_eq!(
            assemble(vec![page(1, 2, 5, &[101]), page(2, 2, 4, &[102])]),
            Err(DomainError::PaginationDrifted)
        );
    }

    #[test]
    fn too_few_items_is_refused_rather_than_truncated() {
        assert_eq!(
            assemble(vec![page(1, 2, 3, &[101])]),
            Err(DomainError::ShortCollection {
                got: 1,
                expected: 3
            })
        );
    }

    #[test]
    fn too_many_items_is_refused() {
        assert_eq!(
            assemble(vec![page(1, 1, 1, &[101, 102])]),
            Err(DomainError::OverCollection {
                got: 2,
                expected: 1
            })
        );
    }

    #[test]
    fn a_page_claiming_items_but_returning_none_is_refused() {
        assert_eq!(
            assemble(vec![page(1, 1, 4, &[])]),
            Err(DomainError::EmptyPageWithItems { page: 1 })
        );
    }

    #[test]
    fn dedupe_counts_copies_and_keeps_first_seen_order() {
        let collection = assemble(vec![page(1, 1, 4, &[101, 102, 101, 101])]).unwrap();
        let unique = collection.dedupe();

        assert_eq!(
            unique
                .iter()
                .map(|o| o.release.id.get())
                .collect::<Vec<_>>(),
            vec![101, 102],
            "first-seen order must survive"
        );
        assert_eq!(unique[0].copies, 3);
        assert_eq!(unique[1].copies, 1);
        assert_eq!(
            unique.iter().map(|o| o.copies).sum::<u32>(),
            4,
            "copies must be conserved"
        );
    }

    #[test]
    fn dedupe_keeps_the_later_details_for_a_repeated_pressing() {
        let mut first = release(101, "Original");
        first.year = Some(1959);
        let mut second = release(101, "Corrected");
        second.year = Some(2009);

        let collection = Collection {
            releases: vec![first, second],
            reported_items: 2,
        };
        let unique = collection.dedupe();

        assert_eq!(unique[0].release.title, "Corrected");
        assert_eq!(unique[0].release.year, Some(2009));
        assert_eq!(unique[0].copies, 2);
    }

    #[test]
    fn dedupe_does_not_collapse_distinct_pressings_of_one_work() {
        // Two releases sharing a master are still two pressings, each worth its
        // own valuation. This is the test that protects the core concept.
        let mut japan = release(101, "Kind of Blue");
        japan.master_id = Some(999);
        japan.country = Some("Japan".into());
        let mut us = release(102, "Kind of Blue");
        us.master_id = Some(999);
        us.country = Some("US".into());

        let collection = Collection {
            releases: vec![japan, us],
            reported_items: 2,
        };
        let unique = collection.dedupe();

        assert_eq!(unique.len(), 2, "different pressings are not duplicates");
        assert!(unique.iter().all(|o| o.copies == 1));
    }

    #[test]
    fn total_units_sums_across_copies() {
        let mut double = release(101, "Twofer");
        double.formats[0].qty = "2".into();
        let collection = Collection {
            releases: vec![double.clone(), double, release(102, "Single")],
            reported_items: 3,
        };
        assert_eq!(collection.total_units(), 5);
    }
}
