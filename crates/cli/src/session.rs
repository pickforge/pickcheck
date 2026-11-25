//! Hook session state: the commit a session started from and the files it
//! touched in one repository. See "Session state" in `docs/spec.md`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result};

/// Session state untouched for this long is removed when another session
/// starts.
const STALE_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// How long a reader waits for the anchor's writer to finish its one write.
const ANCHOR_WAIT: Duration = Duration::from_secs(2);
const ANCHOR: &str = "anchor";
const FILES: &str = "files";

pub struct Session {
    anchor: String,
    log: PathBuf,
    files: BTreeSet<PathBuf>,
}

impl Session {
    /// Opens the state of session `id` for the repository at `repo_root`. The
    /// first event of the session there records `head` as the anchor.
    pub fn open(state_dir: &Path, id: &str, repo_root: &Path, head: &str) -> Result<Self> {
        let directory = state_dir.join("sessions");
        fs::create_dir_all(&directory)
            .with_context(|| format!("cannot create state {}", directory.display()))?;
        let key = format!("{id}.{:016x}", fnv1a(repo_root));
        let anchor_path = directory.join(format!("{key}.{ANCHOR}"));
        let anchor = if create_anchor(&anchor_path, head)? {
            prune(&directory, id, SystemTime::now());
            head.to_owned()
        } else {
            read_anchor(&anchor_path)?
        };
        let log = directory.join(format!("{key}.{FILES}"));
        let files = read_log(&log)?;
        Ok(Self { anchor, log, files })
    }

    pub fn anchor(&self) -> &str {
        &self.anchor
    }

    /// Whether the session recorded `path`, relative to the repository root.
    pub fn contains(&self, path: &Path) -> bool {
        self.files.contains(path)
    }

    /// Appends the paths the log does not hold yet, one record per write.
    pub fn record<'a>(&mut self, paths: impl IntoIterator<Item = &'a PathBuf>) -> Result<()> {
        let mut log = None;
        for path in paths {
            if self.files.contains(path) {
                continue;
            }
            let file = match &mut log {
                Some(file) => file,
                None => log.insert(open_log(&self.log)?),
            };
            // The leading newline ends a record that a crash left truncated.
            let record = format!("\n{}\n", serde_json::to_string(&path.to_string_lossy())?);
            file.write_all(record.as_bytes())
                .with_context(|| format!("cannot write state {}", self.log.display()))?;
            self.files.insert(path.clone());
        }
        Ok(())
    }
}

/// Creates the anchor unless it exists. The exclusive create lets exactly
/// one of several racing first events write it.
fn create_anchor(path: &Path, head: &str) -> Result<bool> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot write state {}", path.display()));
        }
    };
    file.write_all(format!("{head}\n").as_bytes())
        .with_context(|| format!("cannot write state {}", path.display()))?;
    Ok(true)
}

/// Reads the anchor, waiting while its writer has created the file but not
/// yet finished the line. Content that never completes is returned as read;
/// it names no commit, so the hook falls back with a note.
fn read_anchor(path: &Path) -> Result<String> {
    let started = Instant::now();
    loop {
        let mut text = String::new();
        File::open(path)
            .and_then(|mut file| file.read_to_string(&mut text))
            .with_context(|| format!("cannot read state {}", path.display()))?;
        if text.ends_with('\n') || started.elapsed() >= ANCHOR_WAIT {
            return Ok(text.trim().to_owned());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn open_log(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("cannot write state {}", path.display()))
}

/// Every complete record in the log. A truncated record does not parse and
/// is skipped; duplicates collapse.
fn read_log(path: &Path) -> Result<BTreeSet<PathBuf>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read state {}", path.display()));
        }
    };
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| serde_json::from_str::<String>(line).ok())
        .map(PathBuf::from)
        .collect())
}

/// Removes the anchor and log of every other session whose newest file is
/// older than `STALE_AFTER`. Only names this module writes are touched, and
/// a failure leaves the files for a later prune.
fn prune(directory: &Path, current: &str, now: SystemTime) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut sessions = BTreeMap::<String, (SystemTime, Vec<PathBuf>)>::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((key, kind)) = name.rsplit_once('.') else {
            continue;
        };
        let owner = key.rsplit_once('.').map_or(key, |(id, _)| id);
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if !matches!(kind, ANCHOR | FILES) || owner == current {
            continue;
        }
        let session = sessions
            .entry(key.to_owned())
            .or_insert((SystemTime::UNIX_EPOCH, Vec::new()));
        session.0 = session.0.max(modified);
        session.1.push(entry.path());
    }
    for (newest, paths) in sessions.into_values() {
        if now
            .duration_since(newest)
            .is_ok_and(|age| age > STALE_AFTER)
        {
            for path in paths {
                let _ = fs::remove_file(path);
            }
        }
    }
}

/// 64-bit FNV-1a of the root's bytes: stable across toolchains, unlike the
/// standard hasher, so a new build finds the same state.
fn fnv1a(path: &Path) -> u64 {
    path.as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_hash_is_fixed_fnv1a() {
        assert_eq!(fnv1a(Path::new("")), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(Path::new("a")), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn the_log_skips_truncated_records_and_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("s.files");
        fs::write(&log, "\n\"a.js\"\n\n\"b.js\"\n\n\"a.js\"\n\n\"src/c").unwrap();
        let files = read_log(&log).unwrap();
        assert_eq!(files, BTreeSet::from(["a.js", "b.js"].map(PathBuf::from)));
        // A record appended after a truncated one is still read.
        open_log(&log).unwrap().write_all(b"\n\"d.js\"\n").unwrap();
        assert!(read_log(&log).unwrap().contains(Path::new("d.js")));
    }

    #[test]
    fn the_anchor_is_written_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new("repo");
        let first = Session::open(dir.path(), "s", root, "aaa").unwrap();
        let second = Session::open(dir.path(), "s", root, "bbb").unwrap();
        assert_eq!((first.anchor(), second.anchor()), ("aaa", "aaa"));
        let other = Session::open(dir.path(), "s", Path::new("other"), "ccc").unwrap();
        assert_eq!(other.anchor(), "ccc");
    }

    #[test]
    fn an_unfinished_anchor_is_returned_after_the_wait() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.anchor");
        fs::write(&path, "").unwrap();
        assert_eq!(read_anchor(&path).unwrap(), "");
    }

    #[test]
    fn prune_removes_only_stale_state_of_other_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let names = [
            "old.0000000000000001.anchor",
            "old.0000000000000001.files",
            "mine.0000000000000002.anchor",
            "fresh.0000000000000003.anchor",
            "notes.txt",
            "unrelated",
        ];
        for name in names {
            fs::write(dir.path().join(name), "").unwrap();
        }
        let later = SystemTime::now() + STALE_AFTER + Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(dir.path().join("fresh.0000000000000003.anchor"))
            .unwrap()
            .set_modified(later)
            .unwrap();
        prune(dir.path(), "mine", later);
        let left = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            left,
            BTreeSet::from(
                [
                    "mine.0000000000000002.anchor",
                    "fresh.0000000000000003.anchor",
                    "notes.txt",
                    "unrelated"
                ]
                .map(str::to_owned)
            )
        );
    }
}
