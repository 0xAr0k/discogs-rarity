//! The wantlist adapter: the one file that remembers what to dig for.
//!
//! # Why a JSON file and not a database
//!
//! The wantlist is dozens of lines, written once in a while, read at every
//! startup. SQLite would buy indexes we never use and a hard dependency the
//! collection CLI did not need. A single JSON document, written through a
//! temp-file rename, survives crashes and is readable with `less` when the
//! user wants to see what they promised themselves.
//!
//! # Contract with the port
//!
//! [`WantlistStore`][crate::ports::WantlistStore] speaks domain types and
//! `&self`. So this adapter reads the file on every call and rewrites it whole
//! on mutation — no interior state, no lock of its own beyond what std gives,
//! and no way for the caller's view and the file's contents to diverge.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    domain::browse::{EntityKind, WantEntry},
    error::{Error, Result},
    ports::WantlistStore,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RawWant {
    kind: String,
    id: u64,
    name: String,
    added_at: i64,
}

/// A wantlist persisted as one JSON document at a fixed path.
#[derive(Debug, Clone)]
pub struct JsonWantlist {
    path: PathBuf,
}

impl JsonWantlist {
    /// The file to store at. Created implicitly on first write; an absent
    /// file reads as an empty wantlist, never an error.
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn read_all(&self) -> Result<Vec<RawWant>> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(Error::msg(format!(
                    "cannot read wantlist {}: {error}",
                    self.path.display()
                )));
            }
        };
        serde_json::from_slice(&bytes).map_err(|error| {
            Error::msg(format!(
                "wantlist {} is not valid JSON: {error}",
                self.path.display()
            ))
        })
    }

    /// Write through a temp file and rename: a crash mid-`write` leaves the
    /// previous file intact, never a truncated half-wantlist.
    fn write_all(&self, wants: &[RawWant]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::msg(format!("cannot create {}: {error}", parent.display()))
            })?;
        }
        let temporary = temporary_sibling(&self.path);
        let body = serde_json::to_vec_pretty(wants)
            .map_err(|error| Error::msg(format!("wantlist cannot serialize: {error}")))?;
        fs::write(&temporary, body).map_err(|error| {
            Error::msg(format!("cannot write {}: {error}", temporary.display()))
        })?;
        fs::rename(&temporary, &self.path).map_err(|error| {
            Error::msg(format!(
                "cannot move {} into place at {}: {error}",
                temporary.display(),
                self.path.display()
            ))
        })?;
        Ok(())
    }

    fn into_entries(wants: Vec<RawWant>) -> Vec<WantEntry> {
        wants
            .into_iter()
            .filter_map(|raw| {
                Some(WantEntry {
                    kind: EntityKind::parse(&raw.kind)?,
                    id: raw.id,
                    name: raw.name,
                    added_at: raw.added_at,
                })
            })
            // Newest first: the wantlist is a to-dig list, and the last thing
            // added is the thing still on the mind.
            .collect::<Vec<_>>()
    }

    fn into_raws(entries: &[WantEntry]) -> Vec<RawWant> {
        entries
            .iter()
            .map(|entry| RawWant {
                kind: entry.kind.as_str().to_string(),
                id: entry.id,
                name: entry.name.clone(),
                added_at: entry.added_at,
            })
            .collect()
    }
}

/// `wantlist.json.dig-tmp` — same suffix so a rename stays on one filesystem.
fn temporary_sibling(path: &Path) -> PathBuf {
    let name = path.file_name().map_or_else(
        || "wantlist".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    path.with_file_name(format!("{name}.dig-tmp"))
}

impl WantlistStore for JsonWantlist {
    fn add(&self, entry: &WantEntry) -> Result<bool> {
        let mut wants = Self::into_entries(self.read_all()?);
        if wants
            .iter()
            .any(|want| want.kind == entry.kind && want.id == entry.id)
        {
            return Ok(false);
        }
        wants.push(entry.clone());
        wants.sort_by(|a, b| b.added_at.cmp(&a.added_at).then(a.name.cmp(&b.name)));
        self.write_all(&Self::into_raws(&wants))?;
        Ok(true)
    }

    fn remove(&self, kind: EntityKind, id: u64) -> Result<bool> {
        let all = Self::into_entries(self.read_all()?);
        let remaining: Vec<WantEntry> = all
            .iter()
            .filter(|want| !(want.kind == kind && want.id == id))
            .cloned()
            .collect();
        if remaining.len() == all.len() {
            return Ok(false);
        }
        self.write_all(&Self::into_raws(&remaining))?;
        Ok(true)
    }

    fn list(&self) -> Result<Vec<WantEntry>> {
        Ok(Self::into_entries(self.read_all()?))
    }

    fn contains(&self, kind: EntityKind, id: u64) -> Result<bool> {
        Ok(self
            .list()?
            .iter()
            .any(|want| want.kind == kind && want.id == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own temp file so a failure in one cannot corrupt
    /// another's assumptions.
    fn a_temp_wantlist(tag: &str) -> JsonWantlist {
        JsonWantlist::at(std::env::temp_dir().join(format!(
            "diggers-rarity-test-{tag}-{}.json",
            std::process::id()
        )))
    }

    fn a_want(kind: EntityKind, id: u64, name: &str, added_at: i64) -> WantEntry {
        WantEntry {
            kind,
            id,
            name: name.to_string(),
            added_at,
        }
    }

    #[test]
    fn an_absent_file_is_an_empty_list_not_an_error() {
        let store = a_temp_wantlist("absent");
        assert_eq!(store.list().unwrap(), Vec::new());
        assert!(!store.contains(EntityKind::Artist, 1).unwrap());
    }

    #[test]
    fn add_then_list_round_trips_and_sorts_newest_first() {
        let store = a_temp_wantlist("roundtrip");
        assert!(
            store
                .add(&a_want(EntityKind::Artist, 108713, "The Digger", 100))
                .unwrap()
        );
        assert!(
            store
                .add(&a_want(EntityKind::Release, 249504, "The Vinyl", 200))
                .unwrap()
        );

        let wants = store.list().unwrap();
        assert_eq!(wants[0].name, "The Vinyl", "newest first");
        assert_eq!(wants[0].kind, EntityKind::Release);
        assert_eq!(wants[1].added_at, 100);
    }

    #[test]
    fn adding_the_same_wish_twice_is_idempotent() {
        let store = a_temp_wantlist("idempotent");
        assert!(
            store
                .add(&a_want(EntityKind::Label, 1, "Some Label", 1))
                .unwrap()
        );
        assert!(
            !store
                .add(&a_want(EntityKind::Label, 1, "Some Label", 2))
                .unwrap()
        );
        let wants = store.list().unwrap();
        assert_eq!(wants.len(), 1);
        assert_eq!(wants[0].added_at, 1, "the first answer wins");
    }

    #[test]
    fn remove_reports_whether_anything_happened() {
        let store = a_temp_wantlist("remove");
        store
            .add(&a_want(EntityKind::Artist, 7, "Ars 7", 1))
            .unwrap();

        assert!(store.remove(EntityKind::Artist, 7).unwrap());
        assert!(!store.remove(EntityKind::Artist, 7).unwrap());
        assert!(!store.remove(EntityKind::Artist, 9).unwrap());
        assert_eq!(store.list().unwrap(), Vec::new());
    }

    #[test]
    fn a_removed_entry_in_a_shared_file_leaves_its_neighbours() {
        let store = a_temp_wantlist("neighbours");
        store.add(&a_want(EntityKind::Artist, 1, "One", 1)).unwrap();
        store.add(&a_want(EntityKind::Artist, 2, "Two", 2)).unwrap();
        store.remove(EntityKind::Artist, 1).unwrap();

        let wants = store.list().unwrap();
        assert_eq!(wants.len(), 1);
        assert_eq!(wants[0].name, "Two");
    }

    #[test]
    fn a_corrupt_file_is_an_error_naming_the_path() {
        let path =
            std::env::temp_dir().join(format!("diggers-corrupt-{}.json", std::process::id()));
        std::fs::write(&path, b"{ not json").unwrap();
        let store = JsonWantlist::at(path.clone());
        let error = store.list().expect_err("corrupt store must error");
        assert!(error.to_string().contains("not valid JSON"), "{error}");
        std::fs::remove_file(&path).ok();
    }
}
