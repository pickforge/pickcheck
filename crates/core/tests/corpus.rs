//! Pinned corpus snapshot. Ignored by default, so plain `cargo test` never
//! needs the corpus. Fetch it and compare:
//!
//! ```sh
//! scripts/fetch-corpus.sh <dir>
//! PICKCHECK_CORPUS_DIR=<dir> cargo test -p pickcheck-core --test corpus --release -- --ignored
//! ```
//!
//! Add `PICKCHECK_CORPUS_UPDATE=1` to rewrite `tests/corpus/snapshot.json`.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

use pickcheck_core::{Language, Metric, ScanOptions, scan};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_DIFF_LINES: usize = 200;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    repositories: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    url: String,
    commit: String,
    license: String,
}

#[derive(Serialize)]
struct Snapshot {
    repositories: BTreeMap<String, RepositoryStats>,
}

#[derive(Serialize)]
struct RepositoryStats {
    commit: String,
    unverified: usize,
    languages: BTreeMap<String, LanguageStats>,
}

/// `files` counts files with at least one measured unit.
#[derive(Serialize)]
struct LanguageStats {
    files: usize,
    functions: usize,
    violations: BTreeMap<&'static str, usize>,
    histograms: BTreeMap<&'static str, BTreeMap<usize, usize>>,
}

#[test]
#[ignore = "needs the pinned corpus; see the module docs"]
fn corpus_matches_snapshot() {
    let corpus = PathBuf::from(
        env::var_os("PICKCHECK_CORPUS_DIR")
            .expect("set PICKCHECK_CORPUS_DIR to a directory filled by scripts/fetch-corpus.sh"),
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus");
    let manifest: Manifest =
        serde_json::from_str(&fs::read_to_string(root.join("manifest.json")).unwrap()).unwrap();
    let isolation = isolate_config();
    let snapshot = Snapshot {
        repositories: manifest
            .repositories
            .iter()
            .map(|entry| {
                let stats = measure(&corpus, entry, &isolation.path().join("empty.json"));
                (entry.name.clone(), stats)
            })
            .collect(),
    };
    let actual = serde_json::to_string_pretty(&snapshot).unwrap() + "\n";
    let path = root.join("snapshot.json");
    if env::var_os("PICKCHECK_CORPUS_UPDATE").is_some_and(|value| value == "1") {
        fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_default();
    if actual != expected {
        let diff = describe_diff(&expected, &actual);
        panic!(
            "corpus snapshot differs from {} (snapshot -> measured):\n{diff}\nregenerate it with PICKCHECK_CORPUS_UPDATE=1",
            path.display()
        );
    }
}

/// Built-in defaults only: an empty user config directory, and an empty
/// explicit config that replaces any `.pickcheck.json` in a repository.
fn isolate_config() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("xdg")).unwrap();
    fs::write(directory.path().join("empty.json"), "{}").unwrap();
    // SAFETY: this test binary runs one test, before any other thread reads
    // the environment.
    unsafe { env::set_var("XDG_CONFIG_HOME", directory.path().join("xdg")) };
    directory
}

fn measure(corpus: &Path, entry: &Entry, config: &Path) -> RepositoryStats {
    assert!(
        !entry.url.is_empty() && entry.commit.len() == 40 && !entry.license.is_empty(),
        "incomplete manifest entry {}",
        entry.name
    );
    let directory = corpus.join(&entry.name);
    let head = git_head(&directory);
    assert_eq!(
        head,
        entry.commit,
        "{} is not at its pinned commit; run scripts/fetch-corpus.sh",
        directory.display()
    );
    let result = scan(&ScanOptions {
        cwd: &directory,
        paths: &[],
        explicit_config: Some(config),
        changed: None,
    })
    .unwrap_or_else(|error| panic!("scan of {} failed: {error:#}", entry.name));

    let mut languages = BTreeMap::<String, LanguageStats>::new();
    let mut files = BTreeMap::<String, BTreeSet<&Path>>::new();
    for (file, function) in &result.functions {
        let label = language_label(file);
        files.entry(label.clone()).or_default().insert(file);
        let stats = languages.entry(label).or_insert_with(LanguageStats::empty);
        stats.functions += 1;
        for metric in Metric::ALL {
            *stats
                .histograms
                .get_mut(metric.name())
                .unwrap()
                .entry(function.value(metric))
                .or_default() += 1;
        }
    }
    for violation in &result.violations {
        let stats = languages.get_mut(&language_label(&violation.file)).unwrap();
        *stats.violations.get_mut(violation.metric.as_str()).unwrap() += 1;
    }
    for (label, set) in files {
        languages.get_mut(&label).unwrap().files = set.len();
    }
    RepositoryStats {
        commit: head,
        unverified: result.unverified.len(),
        languages,
    }
}

impl LanguageStats {
    /// Every metric present, so a count that leaves zero still shows.
    fn empty() -> Self {
        Self {
            files: 0,
            functions: 0,
            violations: Metric::ALL
                .iter()
                .map(|metric| (metric.name(), 0))
                .collect(),
            histograms: Metric::ALL
                .iter()
                .map(|metric| (metric.name(), BTreeMap::new()))
                .collect(),
        }
    }
}

/// TSX gets its own label; `Language::name` folds it into `typescript`.
fn language_label(file: &Path) -> String {
    let language = Language::from_path(file).expect("measured files have a language");
    format!("{language:?}").to_lowercase()
}

fn git_head(directory: &Path) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap_or_else(|error| panic!("cannot run git in {}: {error}", directory.display()));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// One line per changed leaf, such as
/// `zod/languages/typescript/histograms/cognitive/5: 12 -> 14`.
fn describe_diff(expected: &str, actual: &str) -> String {
    let expected = serde_json::from_str(expected).map_or_else(|_| BTreeMap::new(), leaves);
    let actual = leaves(serde_json::from_str(actual).unwrap());
    let keys: BTreeSet<_> = expected.keys().chain(actual.keys()).collect();
    let lines: Vec<_> = keys
        .into_iter()
        .filter(|key| expected.get(*key) != actual.get(*key))
        .map(|key| {
            let show = |value: Option<&Value>| value.map_or("absent".to_owned(), Value::to_string);
            format!(
                "{key}: {} -> {}",
                show(expected.get(key)),
                show(actual.get(key))
            )
        })
        .collect();
    let mut text = lines
        .iter()
        .take(MAX_DIFF_LINES)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    if lines.len() > MAX_DIFF_LINES {
        text += &format!("\n... and {} more", lines.len() - MAX_DIFF_LINES);
    }
    if lines.is_empty() {
        text = "formatting differs; regenerate the snapshot".to_owned();
    }
    text
}

fn leaves(value: Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    collect_leaves(String::new(), value, &mut out);
    out
}

fn collect_leaves(prefix: String, value: Value, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key
                } else {
                    format!("{prefix}/{key}")
                };
                collect_leaves(path, child, out);
            }
        }
        // `repositories` is the only top-level key.
        leaf => {
            out.insert(
                prefix
                    .strip_prefix("repositories/")
                    .unwrap_or(&prefix)
                    .to_owned(),
                leaf,
            );
        }
    }
}
