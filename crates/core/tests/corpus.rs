//! Pinned corpus snapshot. Ignored by default, so plain `cargo test` never
//! needs the corpus. Fetch it and compare, from the workspace root:
//!
//! ```sh
//! scripts/fetch-corpus.sh target/corpus
//! PICKCHECK_CORPUS_DIR=target/corpus cargo test -p pickcheck-core --test corpus --release -- --ignored
//! ```
//!
//! A relative `PICKCHECK_CORPUS_DIR` or `PICKCHECK_CORPUS_LISTING` resolves
//! against the workspace root. Add `PICKCHECK_CORPUS_UPDATE=1` to rewrite
//! `tests/corpus/snapshot.json`. Set `PICKCHECK_CORPUS_LISTING=<file>` to write
//! one line per measured function, so two builds can be compared with `diff`.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use ignore::WalkBuilder;
use pickcheck_core::{Config, FunctionMetrics, Language, Metric, ScanOptions, load_config, scan};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_DIFF_LINES: usize = 200;

/// Every language label; the corpus must have a function in each.
const LANGUAGES: [&str; 8] = [
    "dart",
    "go",
    "javascript",
    "python",
    "rust",
    "svelte",
    "tsx",
    "typescript",
];

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
    languages: BTreeMap<String, LanguageStats>,
    unverified: usize,
}

/// `files` counts every measured source file, with or without a function.
/// `digest` covers every function record, so it moves when a value moves
/// between functions and the histograms stay the same.
#[derive(Serialize)]
struct LanguageStats {
    digest: String,
    files: usize,
    functions: usize,
    histograms: BTreeMap<&'static str, BTreeMap<usize, usize>>,
    violations: BTreeMap<&'static str, usize>,
}

/// One measured function. The field order is the sort order.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Record {
    file: String,
    line: usize,
    function: String,
    values: [usize; Metric::ALL.len()],
}

impl Record {
    fn new(file: &Path, function: &FunctionMetrics) -> Self {
        Self {
            file: slash_path(file),
            line: function.line,
            function: function.function.clone(),
            values: Metric::ALL.map(|metric| function.value(metric)),
        }
    }

    /// `file<TAB>line<TAB>function<TAB>complexity=1 cognitive=0 ...`
    fn text(&self) -> String {
        let values = Metric::ALL
            .iter()
            .zip(self.values)
            .map(|(metric, value)| format!("{}={value}", metric.name()))
            .collect::<Vec<_>>()
            .join(" ");
        format!("{}\t{}\t{}\t{values}", self.file, self.line, self.function)
    }
}

/// 64-bit FNV-1a over the sorted record lines, as 16 hex digits.
fn digest(records: &[Record]) -> String {
    let mut sorted: Vec<_> = records.iter().collect();
    sorted.sort();
    let hash = sorted
        .iter()
        .fold(0xCBF2_9CE4_8422_2325_u64, |hash, record| {
            (record.text() + "\n").bytes().fold(hash, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3)
            })
        });
    format!("{hash:016x}")
}

#[test]
#[ignore = "needs the pinned corpus; see the module docs"]
fn corpus_matches_snapshot() {
    let corpus = workspace_path(
        env::var_os("PICKCHECK_CORPUS_DIR")
            .expect("set PICKCHECK_CORPUS_DIR to a directory filled by scripts/fetch-corpus.sh"),
    );
    let root = workspace().join("tests/corpus");
    let manifest: Manifest =
        serde_json::from_str(&fs::read_to_string(root.join("manifest.json")).unwrap()).unwrap();
    assert!(
        !manifest.repositories.is_empty(),
        "tests/corpus/manifest.json lists no repository"
    );
    let isolation = isolate_environment();
    let config = isolation.path().join("empty.json");
    let directories: Vec<_> = manifest
        .repositories
        .iter()
        .map(|entry| check_checkout(&corpus, entry))
        .collect();
    let mut listing = Vec::new();
    let snapshot = Snapshot {
        repositories: manifest
            .repositories
            .iter()
            .zip(&directories)
            .map(|(entry, directory)| {
                let stats = measure(directory, entry, &config, &mut listing);
                (entry.name.clone(), stats)
            })
            .collect(),
    };
    check_complete(&snapshot);
    if let Some(path) = env::var_os("PICKCHECK_CORPUS_LISTING") {
        listing.sort();
        fs::write(workspace_path(path), listing.concat()).unwrap();
    }
    let actual = serde_json::to_value(&snapshot).unwrap();
    let path = root.join("snapshot.json");
    if env::var_os("PICKCHECK_CORPUS_UPDATE").is_some_and(|value| value == "1") {
        fs::write(&path, canonical(&snapshot)).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    if expected != actual {
        panic!(
            "corpus snapshot differs from {} (snapshot -> measured):\n{}\nregenerate it with PICKCHECK_CORPUS_UPDATE=1",
            path.display(),
            describe_diff(&expected, &actual)
        );
    }
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Cargo runs the test from `crates/core`, so a relative path would depend
/// on the directory. Resolve it against the workspace root instead.
fn workspace_path(value: OsString) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        workspace().join(path)
    }
}

/// LF line ends, keys in a fixed sorted order, and a trailing newline.
fn canonical(snapshot: &Snapshot) -> String {
    serde_json::to_string_pretty(snapshot).unwrap() + "\n"
}

/// Built-in defaults only. Empty home and config directories hide the user's
/// pickcheck config and global Git ignore file. An empty explicit config
/// replaces any `.pickcheck.json` in or above a repository.
fn isolate_environment() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    for name in ["home", "xdg"] {
        fs::create_dir(path.join(name)).unwrap();
    }
    fs::write(path.join("empty.json"), "{}").unwrap();
    fs::write(path.join("gitconfig"), "").unwrap();
    // SAFETY: only `--ignored` selects this test, so it runs alone in its
    // binary, before any other thread reads the environment.
    unsafe {
        env::set_var("HOME", path.join("home"));
        env::set_var("XDG_CONFIG_HOME", path.join("xdg"));
        env::set_var("GIT_CONFIG_GLOBAL", path.join("gitconfig"));
        env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    }
    directory
}

/// The scan also reads `.git/info/exclude` and `.ignore` files above the
/// repository. Neither is part of the pinned tree, so refuse to measure with
/// either present.
fn assert_no_outside_ignores(directory: &Path) {
    let exclude = directory.join(".git/info/exclude");
    if let Ok(text) = fs::read_to_string(&exclude) {
        assert!(
            text.lines()
                .map(str::trim)
                .all(|line| line.is_empty() || line.starts_with('#')),
            "{} has ignore rules; run scripts/fetch-corpus.sh again",
            exclude.display()
        );
    }
    for parent in directory.ancestors().skip(1) {
        let ignore = parent.join(".ignore");
        assert!(
            !ignore.exists(),
            "{} would hide corpus files; move the corpus elsewhere",
            ignore.display()
        );
    }
}

/// Fails before any measurement when a checkout is missing, moved, or sits
/// under ignore rules from outside its tree.
fn check_checkout(corpus: &Path, entry: &Entry) -> PathBuf {
    assert!(
        !entry.name.is_empty()
            && !entry.url.is_empty()
            && entry.commit.len() == 40
            && !entry.license.is_empty(),
        "incomplete manifest entry {}",
        entry.name
    );
    let directory = corpus.join(&entry.name);
    assert!(
        directory.is_dir(),
        "{} is missing; run scripts/fetch-corpus.sh {}",
        directory.display(),
        corpus.display()
    );
    assert_eq!(
        git_head(&directory),
        entry.commit,
        "{} is not at its pinned commit; run scripts/fetch-corpus.sh",
        directory.display()
    );
    let directory = pickcheck_core::physical(&directory);
    assert_no_outside_ignores(&directory);
    directory
}

fn measure(
    directory: &Path,
    entry: &Entry,
    config: &Path,
    listing: &mut Vec<String>,
) -> RepositoryStats {
    let result = scan(&ScanOptions {
        cwd: directory,
        paths: &[],
        explicit_config: Some(config),
        changed: None,
    })
    .unwrap_or_else(|error| panic!("scan of {} failed: {error:#}", entry.name));

    let unreadable: BTreeSet<_> = result.unverified.iter().map(|item| &item.file).collect();
    let mut languages = BTreeMap::<String, LanguageStats>::new();
    for file in source_files(directory, config) {
        if !unreadable.contains(&&file) {
            languages
                .entry(language_label(&file))
                .or_insert_with(LanguageStats::empty)
                .files += 1;
        }
    }
    let mut records = BTreeMap::<String, Vec<Record>>::new();
    for (file, function) in &result.functions {
        let label = language_label(file);
        let stats = languages.get_mut(&label).unwrap_or_else(|| {
            panic!(
                "{} measured {}, a file outside the walk",
                entry.name,
                file.display()
            )
        });
        stats.functions += 1;
        for metric in Metric::ALL {
            *stats
                .histograms
                .get_mut(metric.name())
                .unwrap()
                .entry(function.value(metric))
                .or_default() += 1;
        }
        records
            .entry(label)
            .or_default()
            .push(Record::new(file, function));
    }
    for violation in &result.violations {
        let stats = languages.get_mut(&language_label(&violation.file)).unwrap();
        *stats.violations.get_mut(violation.metric.as_str()).unwrap() += 1;
    }
    for (label, stats) in &mut languages {
        let records = records.remove(label).unwrap_or_default();
        stats.digest = digest(&records);
        listing.extend(
            records
                .iter()
                .map(|record| format!("{}\t{label}\t{}\n", entry.name, record.text())),
        );
    }
    RepositoryStats {
        commit: entry.commit.clone(),
        languages,
        unverified: result.unverified.len(),
    }
}

/// Every file with a language that the scan reads, relative to `directory`.
/// The walk uses the scan's filters, minus the inputs from outside the tree.
fn source_files(directory: &Path, config: &Path) -> Vec<PathBuf> {
    let ignores = Config::matcher(&load_config(directory, Some(config)).unwrap().config.ignore)
        .expect("built-in ignore globs compile");
    WalkBuilder::new(directory)
        .standard_filters(true)
        .hidden(false)
        .parents(false)
        .git_global(false)
        .git_exclude(false)
        .build()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.path().strip_prefix(directory).unwrap().to_path_buf())
        .filter(|path| Language::from_path(path).is_some() && !ignores.is_match(path))
        .collect()
}

/// Refuses a snapshot that a missing or broken checkout could produce.
fn check_complete(snapshot: &Snapshot) {
    let mut seen = BTreeSet::new();
    for (name, repository) in &snapshot.repositories {
        let functions: usize = repository
            .languages
            .values()
            .map(|stats| stats.functions)
            .sum();
        assert!(functions > 0, "{name} yielded no function");
        seen.extend(
            repository
                .languages
                .iter()
                .filter(|(_, stats)| stats.functions > 0)
                .map(|(label, _)| label.as_str()),
        );
    }
    let missing: Vec<_> = LANGUAGES
        .iter()
        .filter(|label| !seen.contains(*label))
        .collect();
    assert!(
        missing.is_empty(),
        "the corpus has no function in {missing:?}"
    );
}

impl LanguageStats {
    /// Every metric present, so a count that leaves zero still shows.
    fn empty() -> Self {
        Self {
            digest: String::new(),
            files: 0,
            functions: 0,
            histograms: Metric::ALL
                .iter()
                .map(|metric| (metric.name(), BTreeMap::new()))
                .collect(),
            violations: Metric::ALL
                .iter()
                .map(|metric| (metric.name(), 0))
                .collect(),
        }
    }
}

/// TSX gets its own label; `Language::name` folds it into `typescript`.
fn language_label(file: &Path) -> String {
    let language = Language::from_path(file).expect("measured files have a language");
    format!("{language:?}").to_lowercase()
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
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
fn describe_diff(expected: &Value, actual: &Value) -> String {
    let expected = leaves(expected);
    let actual = leaves(actual);
    let keys: BTreeSet<_> = expected.keys().chain(actual.keys()).collect();
    let lines: Vec<_> = keys
        .into_iter()
        .filter(|key| expected.get(*key) != actual.get(*key))
        .map(|key| {
            let show =
                |value: Option<&&Value>| value.map_or("absent".to_owned(), |v| v.to_string());
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
    text
}

fn leaves(value: &Value) -> BTreeMap<String, &Value> {
    let mut out = BTreeMap::new();
    collect_leaves(String::new(), value, &mut out);
    out
}

fn collect_leaves<'a>(prefix: String, value: &'a Value, out: &mut BTreeMap<String, &'a Value>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
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

fn record(function: &str, line: usize, complexity: usize) -> Record {
    Record {
        file: "src/a.js".to_owned(),
        line,
        function: function.to_owned(),
        values: [complexity, 0, 0, 1, 0, 0, 0],
    }
}

#[test]
fn swapping_values_between_functions_changes_digest() {
    let before = [record("first", 1, 1), record("second", 5, 2)];
    let swapped = [record("first", 1, 2), record("second", 5, 1)];
    let reordered = [before[1].clone(), before[0].clone()];
    assert_ne!(digest(&before), digest(&swapped));
    assert_eq!(digest(&before), digest(&reordered));
}

#[test]
fn crlf_snapshot_matches_parsed_value() {
    let mut stats = LanguageStats::empty();
    stats.functions = 2;
    stats.digest = digest(&[record("first", 1, 1)]);
    let snapshot = Snapshot {
        repositories: BTreeMap::from([(
            "demo".to_owned(),
            RepositoryStats {
                commit: "0".repeat(40),
                languages: BTreeMap::from([("javascript".to_owned(), stats)]),
                unverified: 0,
            },
        )]),
    };
    let text = canonical(&snapshot);
    assert!(text.ends_with("}\n") && !text.contains('\r'));
    let crlf: Value = serde_json::from_str(&text.replace('\n', "\r\n")).unwrap();
    assert_eq!(crlf, serde_json::to_value(&snapshot).unwrap());
}
