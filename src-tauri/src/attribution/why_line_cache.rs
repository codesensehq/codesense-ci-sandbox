// SPDX-License-Identifier: MIT
//! Rebuildable whole-file Git attribution, before span filtering or live session joins.
use super::ranges::LineRange;
use crate::shared::sqlite::{open_wal_db, quarantine_corrupt_db, OpenError};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

const FORMAT_VERSION: u32 = 2;
const ROW_LIMIT: usize = 2000;
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS file_ranges (
 repo_key TEXT NOT NULL, commit_sha TEXT NOT NULL, path TEXT NOT NULL,
 notes_sha TEXT NOT NULL, ranges TEXT NOT NULL, touched INTEGER NOT NULL,
 PRIMARY KEY(repo_key, commit_sha, path, notes_sha));
 CREATE INDEX IF NOT EXISTS ranges_lru ON file_ranges(touched);";

pub(super) struct Key<'a> {
    pub repo: &'a str,
    pub commit: &'a str,
    pub path: &'a str,
    pub notes: Option<&'a str>,
}
impl Key<'_> {
    fn notes_key(&self) -> &str {
        self.notes.unwrap_or("no-notes-ref")
    }
}

type Error = OpenError;
fn corrupt(error: &Error) -> bool {
    match error {
        Error::Sqlite(rusqlite::Error::SqliteFailure(error, _)) => matches!(
            error.code,
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
        ),
        Error::Io(error) => error.kind() == std::io::ErrorKind::InvalidData,
        _ => false,
    }
}
fn json_error(error: serde_json::Error) -> Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error).into()
}

pub(super) struct Cache {
    conn: Connection,
}
impl Cache {
    pub fn open(path: &Path) -> Result<Self, Error> {
        match Self::open_once(path) {
            Err(error) if corrupt(&error) => {
                quarantine_corrupt_db(path)?;
                Self::open_once(path)
            }
            result => result,
        }
    }
    fn open_once(path: &Path) -> Result<Self, Error> {
        let mut conn = open_wal_db(path)?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if check != "ok" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "cache integrity check failed",
            )
            .into());
        }
        let version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != FORMAT_VERSION {
            let tx = conn.transaction()?;
            tx.execute_batch("DROP TABLE IF EXISTS file_ranges;")?;
            tx.execute_batch(SCHEMA)?;
            tx.pragma_update(None, "user_version", FORMAT_VERSION)?;
            tx.commit()?;
        } else {
            conn.execute_batch(SCHEMA)?;
        }
        Ok(Self { conn })
    }
    /// Lookup never writes: a notes-snapshot retry must not touch or fill an abandoned key.
    pub fn get(&self, key: &Key<'_>) -> Result<Option<Vec<LineRange>>, Error> {
        let json: Option<String> = self.conn.query_row(
            "SELECT ranges FROM file_ranges WHERE repo_key=?1 AND commit_sha=?2 AND path=?3 AND notes_sha=?4",
            params![key.repo, key.commit, key.path, key.notes_key()], |row| row.get(0)).optional()?;
        json.map(|value| serde_json::from_str(&value).map_err(json_error))
            .transpose()
    }
    pub fn touch(&self, key: &Key<'_>) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE file_ranges SET touched=(SELECT COALESCE(MAX(touched),0)+1 FROM file_ranges)
            WHERE repo_key=?1 AND commit_sha=?2 AND path=?3 AND notes_sha=?4",
            params![key.repo, key.commit, key.path, key.notes_key()],
        )?;
        Ok(())
    }
    pub fn put(&mut self, key: &Key<'_>, ranges: &[LineRange]) -> Result<(), Error> {
        let value = serde_json::to_string(ranges).map_err(json_error)?;
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO file_ranges(repo_key,commit_sha,path,notes_sha,ranges,touched)
            VALUES(?1,?2,?3,?4,?5,(SELECT COALESCE(MAX(touched),0)+1 FROM file_ranges))
            ON CONFLICT(repo_key,commit_sha,path,notes_sha) DO UPDATE SET ranges=excluded.ranges,touched=excluded.touched",
            params![key.repo, key.commit, key.path, key.notes_key(), value])?;
        tx.execute(
            "DELETE FROM file_ranges WHERE rowid IN
            (SELECT rowid FROM file_ranges ORDER BY touched DESC,rowid DESC LIMIT -1 OFFSET ?1)",
            [ROW_LIMIT],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::{
        git_ai::{
            tests::{git, rebuild},
            GitAiSource,
        },
        ranges::AttributionSource,
    };
    #[test]
    fn cache_lru_is_bounded_and_survives_reopen_with_distinct_null_notes_key() {
        let home = tempfile::tempdir().unwrap();
        let repo = rebuild();
        let ranges = GitAiSource::default()
            .ranges(repo.path(), "HEAD", "a.txt")
            .unwrap();
        let commit = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
        let path = home.path().join("cache.sqlite3");
        let mut cache = Cache::open(&path).unwrap();
        for n in 0..ROW_LIMIT {
            cache
                .put(
                    &Key {
                        repo: "github.com/acme/api",
                        commit: &commit,
                        path: &format!("file-{n}.txt"),
                        notes: None,
                    },
                    &ranges,
                )
                .unwrap();
        }
        let key = |path| Key {
            repo: "github.com/acme/api",
            commit: &commit,
            path,
            notes: None,
        };
        cache.touch(&key("file-0.txt")).unwrap();
        cache.put(&key("new.txt"), &ranges).unwrap();
        let count: i64 = cache
            .conn
            .query_row("SELECT COUNT(*) FROM file_ranges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, ROW_LIMIT as i64);
        assert!(cache.get(&key("file-1.txt")).unwrap().is_none());
        assert_eq!(cache.get(&key("file-0.txt")).unwrap(), Some(ranges.clone()));
        drop(cache);
        let cache = Cache::open(&path).unwrap();
        assert_eq!(cache.get(&key("new.txt")).unwrap(), Some(ranges));
        assert!(cache
            .get(&Key {
                notes: Some(git(repo.path(), &["rev-parse", "refs/notes/ai"]).trim()),
                ..key("new.txt")
            })
            .unwrap()
            .is_none());
    }
}
