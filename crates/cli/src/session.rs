//! Hook session state: the commit a session started from and the files it
//! touched in one repository. See "Session state" in `docs/spec.md`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    process,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};

use crate::hooks::sanitized_session_id;

/// Session state untouched for this long is removed when another session
/// starts.
const STALE_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const ANCHOR: &str = "anchor";
const FILES: &str = "files";
const TEMPORARY: &str = "tmp";

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
        let name = sanitized_session_id(id);
        let key = format!("{name}.{:016x}", state_hash(id, repo_root));
        let anchor_path = directory.join(format!("{key}.{ANCHOR}"));
        let anchor = match read_anchor(&anchor_path)? {
            Some(anchor) => anchor,
            None if publish_anchor(&anchor_path, head)? => {
                prune(&directory, &name, SystemTime::now());
                head.to_owned()
            }
            None => read_anchor(&anchor_path)?
                .with_context(|| format!("missing state {}", anchor_path.display()))?,
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

    pub fn files(&self) -> impl Iterator<Item = &PathBuf> {
        self.files.iter()
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

/// The anchor, or `None` when no event has published it yet. A published
/// anchor is always complete, so content that names no commit is corrupt.
fn read_anchor(path: &Path) -> Result<Option<String>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read state {}", path.display()));
        }
    };
    match text.strip_suffix('\n') {
        Some(anchor) if is_commit_name(anchor) => Ok(Some(anchor.to_owned())),
        _ => bail!("corrupt state {}", path.display()),
    }
}

/// A full SHA-1 or SHA-256 object name.
fn is_commit_name(text: &str) -> bool {
    matches!(text.len(), 40 | 64)
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Writes `head` to a temporary file and links it to the anchor name.
/// Whether this event published the anchor: the link fails when another
/// event published first. Readers therefore see a complete anchor or none.
fn publish_anchor(anchor: &Path, head: &str) -> Result<bool> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut temporary = anchor.as_os_str().to_owned();
    temporary.push(format!(".{}-{nanos}.{TEMPORARY}", process::id()));
    let temporary = PathBuf::from(temporary);
    let published =
        write_new(&temporary, head).and_then(|()| match fs::hard_link(&temporary, anchor) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => {
                Err(error).with_context(|| format!("cannot publish state {}", anchor.display()))
            }
        });
    let _ = fs::remove_file(&temporary);
    published
}

fn write_new(path: &Path, head: &str) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| file.write_all(format!("{head}\n").as_bytes()))
        .with_context(|| format!("cannot write state {}", path.display()))
}

fn open_log(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("cannot write state {}", path.display()))
}

/// Every complete record in the log that names a path inside the
/// repository. A truncated record does not parse and is skipped, as is a
/// path that is empty, has a NUL byte, or has a component other than a
/// normal one, such as an absolute path or `..`. Duplicates collapse.
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
        .filter(|path| is_relative_inside(path))
        .map(PathBuf::from)
        .collect())
}

fn is_relative_inside(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Removes the files of every other session whose newest file is older than
/// `STALE_AFTER`. Only regular files with a name this module writes are
/// touched, and a failure leaves the files for a later prune.
fn prune(directory: &Path, current: &str, now: SystemTime) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut sessions = BTreeMap::<String, (SystemTime, Vec<PathBuf>)>::new();
    for (key, modified, path) in entries
        .flatten()
        .filter_map(|entry| state_file(&entry, current))
    {
        let session = sessions.entry(key).or_insert((UNIX_EPOCH, Vec::new()));
        session.0 = session.0.max(modified);
        session.1.push(path);
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

/// The key, modification time, and path of a regular file that this module
/// wrote for a session other than `current`.
fn state_file(entry: &fs::DirEntry, current: &str) -> Option<(String, SystemTime, PathBuf)> {
    let name = entry.file_name().to_string_lossy().into_owned();
    let (_, key) = state_key(&name).filter(|(owner, _)| *owner != current)?;
    let meta = entry
        .metadata()
        .ok()
        .filter(|meta| meta.file_type().is_file())?;
    Some((key.to_owned(), meta.modified().ok()?, entry.path()))
}

/// The session name and the `<name>.<hash>` key of a file this module
/// writes: `<key>.anchor`, `<key>.files`, or `<key>.anchor.<pid>-<time>.tmp`.
fn state_key(file: &str) -> Option<(&str, &str)> {
    let key = match file.strip_suffix(".tmp") {
        Some(stem) => {
            let (stem, unique) = stem.rsplit_once('.')?;
            let (pid, time) = unique.split_once('-')?;
            if !digits(pid) || !digits(time) {
                return None;
            }
            stem.strip_suffix(".anchor")?
        }
        None => file
            .strip_suffix(".anchor")
            .or_else(|| file.strip_suffix(".files"))?,
    };
    let (name, hash) = key.rsplit_once('.')?;
    let valid_hash = hash.len() == 16
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    (valid_hash && sanitized_session_id(name) == name).then_some((name, key))
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// 64-bit FNV-1a of the raw session id, a zero byte, and the root: stable
/// across toolchains, unlike the standard hasher, so a new build finds the
/// same state. The raw id keeps apart two ids that sanitize alike.
fn state_hash(id: &str, repo_root: &Path) -> u64 {
    let bytes = id
        .as_bytes()
        .iter()
        .chain(&[0])
        .chain(repo_root.as_os_str().as_encoded_bytes());
    fnv1a(bytes)
}

fn fnv1a<'a>(bytes: impl IntoIterator<Item = &'a u8>) -> u64 {
    bytes.into_iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AAA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BBB: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn state_hash_is_fixed_fnv1a_of_id_and_root() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(state_hash("s", Path::new("r")), fnv1a(b"s\0r"));
        // Ids that sanitize alike keep apart.
        assert_ne!(
            state_hash("a/b", Path::new("r")),
            state_hash("a:b", Path::new("r"))
        );
    }

    #[test]
    fn the_log_skips_truncated_and_unsafe_records_and_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("s.files");
        let unsafe_records = [
            "",
            "../outside.js",
            "/etc/x.js",
            "src/../d.js",
            "./c.js",
            "a\0b.js",
        ]
        .map(|path| format!("\n{}\n", serde_json::to_string(path).unwrap()))
        .concat();
        let records = "\n\"a.js\"\n\n\"b.js\"\n\n\"a.js\"\n";
        fs::write(&log, format!("{records}{unsafe_records}\n\"src/c")).unwrap();
        let files = read_log(&log).unwrap();
        assert_eq!(files, BTreeSet::from(["a.js", "b.js"].map(PathBuf::from)));
        // A record appended after a truncated one is still read.
        open_log(&log).unwrap().write_all(b"\n\"d.js\"\n").unwrap();
        assert!(read_log(&log).unwrap().contains(Path::new("d.js")));
    }

    #[test]
    fn the_anchor_is_written_once_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new("repo");
        let first = Session::open(dir.path(), "s", root, AAA).unwrap();
        let second = Session::open(dir.path(), "s", root, BBB).unwrap();
        assert_eq!((first.anchor(), second.anchor()), (AAA, AAA));
        let other = Session::open(dir.path(), "s", Path::new("other"), BBB).unwrap();
        assert_eq!(other.anchor(), BBB);
        let names = fs::read_dir(dir.path().join("sessions"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(
            names.iter().all(|name| name.ends_with(".anchor")),
            "{names:?}"
        );
    }

    #[test]
    fn a_lost_race_reads_the_winning_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path().join("s.0000000000000001.anchor");
        assert!(publish_anchor(&anchor, AAA).unwrap());
        assert!(!publish_anchor(&anchor, BBB).unwrap());
        assert_eq!(read_anchor(&anchor).unwrap().as_deref(), Some(AAA));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn an_anchor_that_names_no_commit_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.anchor");
        assert_eq!(read_anchor(&path).unwrap(), None);
        for text in ["", "abc\n", AAA, &format!("{}\n", AAA.to_uppercase())] {
            fs::write(&path, text).unwrap();
            let error = read_anchor(&path).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("corrupt state {}", path.display())
            );
        }
        fs::write(&path, format!("{}\n", "c".repeat(64))).unwrap();
        assert!(read_anchor(&path).unwrap().is_some());
    }

    #[test]
    fn only_names_this_module_writes_are_state() {
        let stem = "a.b_c.0123456789abcdef";
        for (name, expected) in [
            ("a.b_c.0123456789abcdef.anchor", Some(("a.b_c", stem))),
            ("a.b_c.0123456789abcdef.files", Some(("a.b_c", stem))),
            (
                "a.b_c.0123456789abcdef.anchor.12-345.tmp",
                Some(("a.b_c", stem)),
            ),
            ("a.b_c.0123456789abcdef.files.12-345.tmp", None),
            ("a.b_c.0123456789abcdef.anchor.12.tmp", None),
            ("a.b_c.0123456789ABCDEF.anchor", None),
            ("a.b_c.0123456789abcde.anchor", None),
            ("notes.anchor", None),
            (".0123456789abcdef.files", None),
            ("a b.0123456789abcdef.files", None),
            ("x.count", None),
            ("x.0123456789abcdef.count", None),
        ] {
            assert_eq!(state_key(name), expected, "{name}");
        }
    }

    #[test]
    fn prune_removes_only_stale_state_of_other_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let names = [
            "old.0000000000000001.anchor",
            "old.0000000000000001.files",
            "old.0000000000000001.anchor.7-8.tmp",
            "mine.0000000000000002.anchor",
            "fresh.0000000000000003.anchor",
            "notes.anchor",
            "x.count",
            "unrelated",
        ];
        for name in names {
            fs::write(dir.path().join(name), "").unwrap();
        }
        fs::create_dir(dir.path().join("dir.0000000000000004.files")).unwrap();
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
                    "dir.0000000000000004.files",
                    "notes.anchor",
                    "x.count",
                    "unrelated"
                ]
                .map(str::to_owned)
            )
        );
    }
}
