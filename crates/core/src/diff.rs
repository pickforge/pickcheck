use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

use anyhow::{Context, Result, bail};

const REMOVED_GIT_ENV: &[&str] = &[
    "GIT_EXTERNAL_DIFF",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_COUNT",
    // Pathspec magic such as `:(literal)` must keep its meaning.
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// The pathspec that matches every repository config.
const REPO_CONFIGS: &str = ":(glob)**/.pickcheck.json";

/// Repository roots already located, by the directory asked for. The root
/// of a directory does not change while the process runs.
static ROOTS: Mutex<BTreeMap<PathBuf, Option<PathBuf>>> = Mutex::new(BTreeMap::new());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}

impl LineRange {
    pub fn intersects(self, start: usize, end: usize) -> bool {
        self.start <= end && start <= self.end
    }
}

#[derive(Clone, Debug, Default)]
pub struct ChangedFiles {
    pub repo_root: PathBuf,
    pub spans: BTreeMap<PathBuf, Vec<LineRange>>,
    pub untracked: Vec<PathBuf>,
    /// Renamed files, new path to old path.
    pub renames: BTreeMap<PathBuf, PathBuf>,
    /// Every tracked path the diff lists, old and new, including deletions,
    /// pure renames, and empty files that have no hunks.
    pub touched: Vec<PathBuf>,
    /// The merge-base commit with `--base`, resolved once for the diff and
    /// every base read.
    pub base: Option<String>,
    pub fallback: bool,
}

/// The paths that `git diff --name-status` lists.
#[derive(Clone, Debug, Default)]
pub struct DiffStatus {
    /// Renamed files, new path to old path.
    pub renames: BTreeMap<PathBuf, PathBuf>,
    /// Every path, old and new, including deletions, pure renames, and empty
    /// files that have no hunks.
    pub touched: Vec<PathBuf>,
}

/// Limits a diff. No pathspec means the whole tree.
#[derive(Clone, Copy, Debug)]
pub enum Pathspec<'a> {
    /// One path relative to the repository root, matched literally.
    File(&'a Path),
    /// Every `.pickcheck.json` in the tree.
    RepoConfigs,
}

impl Pathspec<'_> {
    fn arg(self) -> String {
        match self {
            Self::File(path) => format!(":(literal){}", slash_path(path)),
            Self::RepoConfigs => REPO_CONFIGS.to_owned(),
        }
    }
}

/// A Git work tree and the commit `HEAD` names.
#[derive(Clone, Debug)]
pub struct Repository {
    /// The physical root.
    pub root: PathBuf,
    pub head: String,
}

/// The repository around `cwd` with its `HEAD` commit, from one Git command.
/// `None` outside a repository and in one without commits.
pub fn repository(cwd: &Path) -> Result<Option<Repository>> {
    let lines = git_line(
        cwd,
        &[
            "rev-parse",
            "--show-toplevel",
            "--verify",
            "--quiet",
            "HEAD^{commit}",
        ],
    )?;
    let Some((root, head)) = lines.as_deref().and_then(|text| text.split_once('\n')) else {
        return Ok(None);
    };
    let root = physical(Path::new(root.trim()));
    remember_root(cwd, Some(root.clone()));
    Ok(Some(Repository {
        root,
        head: head.trim().to_owned(),
    }))
}

/// The merge base of `commit` and `HEAD`, or `None` when `commit` names no
/// commit or the two share no history.
pub fn merge_base_with_head(repo_root: &Path, commit: &str) -> Result<Option<String>> {
    if commit.is_empty() || commit.starts_with('-') {
        return Ok(None);
    }
    git_line(repo_root, &["merge-base", commit, "HEAD"])
}

/// A file's content at the merge base.
#[derive(Debug, PartialEq, Eq)]
pub enum BaseBlob {
    /// The base commit has no file at that path.
    Missing,
    /// The path exists but is not a readable file.
    Unreadable,
    Content(Vec<u8>),
}

/// Diffs the working tree against `HEAD`.
pub fn changed_files(cwd: &Path) -> Result<ChangedFiles> {
    collect_changes(cwd, None)
}

/// Diffs the working tree against the commit where `HEAD` forked from `base`,
/// the same range a pull request shows.
pub fn changed_files_since(cwd: &Path, base: &str) -> Result<ChangedFiles> {
    collect_changes(cwd, Some(base))
}

fn collect_changes(cwd: &Path, base: Option<&str>) -> Result<ChangedFiles> {
    let Some(repo_root) = repository_root(cwd)? else {
        return Ok(ChangedFiles {
            repo_root: cwd.to_path_buf(),
            fallback: true,
            ..ChangedFiles::default()
        });
    };
    if !git_ok(&repo_root, &["rev-parse", "--verify", "HEAD"]) {
        return Ok(ChangedFiles {
            repo_root,
            fallback: true,
            ..ChangedFiles::default()
        });
    }
    let base = base.map(|base| merge_base(&repo_root, base)).transpose()?;
    let from = base.clone().unwrap_or_else(|| "HEAD".to_owned());
    let spans = diff_spans(&repo_root, &from, &[])?;
    let DiffStatus { renames, touched } = diff_status(&repo_root, &from, &[])?;
    Ok(ChangedFiles {
        spans,
        untracked: untracked_files(&repo_root)?,
        renames,
        touched,
        base,
        repo_root,
        fallback: false,
    })
}

/// The changed line ranges of each file from `from` to the working tree.
pub fn diff_spans(
    repo_root: &Path,
    from: &str,
    paths: &[Pathspec<'_>],
) -> Result<BTreeMap<PathBuf, Vec<LineRange>>> {
    let hunks = git_diff(repo_root, from, &["--unified=0"], paths)?;
    Ok(parse_diff_hunks(&hunks))
}

/// The paths that differ from `from` to the working tree. Name-status lists
/// deletions, pure renames, and empty files, which have no hunks, and it
/// never quotes paths. A rename is found only when both paths are in
/// `paths`.
pub fn diff_status(repo_root: &Path, from: &str, paths: &[Pathspec<'_>]) -> Result<DiffStatus> {
    let output = git_diff(repo_root, from, &["--name-status", "-z"], paths)?;
    Ok(parse_name_status(&output))
}

/// Reads `path`, relative to the repository root, at `commit` without
/// textconv or filters. Only an entry that `ls-tree` does not list is
/// missing; every Git failure makes the base unreadable.
pub fn base_blob(repo_root: &Path, commit: &str, path: &Path) -> BaseBlob {
    let path = slash_path(path);
    let listing = match git_command(repo_root)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args(["ls-tree", "-z", commit, "--", path.as_str()])
        .output()
    {
        Ok(output) if output.status.success() => output.stdout,
        _ => return BaseBlob::Unreadable,
    };
    let listing = String::from_utf8_lossy(&listing);
    let Some(object) =
        tree_entries(&listing).find_map(|(object, name)| (name == path).then_some(object))
    else {
        return BaseBlob::Missing;
    };
    object
        .and_then(|object| read_blob(repo_root, object))
        .map_or(BaseBlob::Unreadable, BaseBlob::Content)
}

/// Every entry named `name` in the tree of `commit`, by its directory
/// relative to the repository root: the blob's object name, or `None` for an
/// entry that is not a file. One `ls-tree` lists the whole tree.
pub(crate) fn named_entries(
    repo_root: &Path,
    commit: &str,
    name: &str,
) -> Result<BTreeMap<PathBuf, Option<String>>> {
    let output = git_command(repo_root)
        .args(["ls-tree", "-r", "-z", commit])
        .output()
        .context("failed to execute git ls-tree")?;
    if !output.status.success() {
        bail!("cannot list the tree of {commit}");
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    Ok(tree_entries(&listing)
        .filter_map(|(object, path)| {
            let (directory, file) = path.rsplit_once('/').unwrap_or(("", path));
            (file == name).then(|| (git_path(directory), object.map(str::to_owned)))
        })
        .collect())
}

/// `ls-tree -z` entries, `<mode> <type> <object>\t<path>`, as the object
/// name of a blob (`None` for other types) and the path.
fn tree_entries(listing: &str) -> impl Iterator<Item = (Option<&str>, &str)> {
    listing.split('\0').filter_map(|entry| {
        let (meta, path) = entry.split_once('\t')?;
        let mut fields = meta.split(' ').skip(1);
        let object = match (fields.next(), fields.next()) {
            (Some("blob"), Some(object)) => Some(object),
            _ => None,
        };
        Some((object, path))
    })
}

/// The content of blob `object`, or `None` when Git cannot read it.
pub(crate) fn read_blob(repo_root: &Path, object: &str) -> Option<Vec<u8>> {
    match git_command(repo_root)
        .args(["cat-file", "blob", object])
        .output()
    {
        Ok(output) if output.status.success() => Some(output.stdout),
        _ => None,
    }
}

fn git_diff(
    repo_root: &Path,
    from: &str,
    format: &[&str],
    paths: &[Pathspec<'_>],
) -> Result<String> {
    let output = git_command(repo_root)
        .args([
            "-c",
            "core.quotePath=false",
            "-c",
            "diff.mnemonicPrefix=false",
            "-c",
            "diff.noprefix=false",
            "-c",
            "diff.srcPrefix=a/",
            "-c",
            "diff.dstPrefix=b/",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--find-renames",
        ])
        .args(format)
        .args([from, "--"])
        .args(paths.iter().map(|path| path.arg()))
        .output()
        .context("failed to execute git diff")?;
    if !output.status.success() {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Reads `git diff --name-status -z` fields: a status, then one path, or the
/// old and new paths for a rename or copy.
fn parse_name_status(output: &str) -> DiffStatus {
    let mut renames = BTreeMap::new();
    let mut touched = Vec::new();
    let mut fields = output.split('\0').filter(|field| !field.is_empty());
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        let path = git_path(path);
        if matches!(status.chars().next(), Some('R' | 'C'))
            && let Some(new) = fields.next().map(git_path)
        {
            if status.starts_with('R') {
                renames.insert(new.clone(), path.clone());
            }
            touched.push(new);
        }
        touched.push(path);
    }
    DiffStatus { renames, touched }
}

pub fn repository_root(cwd: &Path) -> Result<Option<PathBuf>> {
    if let Some(root) = lock_roots().get(cwd) {
        return Ok(root.clone());
    }
    let output = git_command(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("failed to locate Git repository root")?;
    let root = if output.status.success() {
        let root = String::from_utf8(output.stdout).context("Git repository root was not UTF-8")?;
        Some(physical(Path::new(root.trim())))
    } else {
        None
    };
    remember_root(cwd, root.clone());
    Ok(root)
}

fn remember_root(cwd: &Path, root: Option<PathBuf>) {
    lock_roots().insert(cwd.to_path_buf(), root);
}

fn lock_roots() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, Option<PathBuf>>> {
    ROOTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The physical spelling of an existing path: symbolic links and Windows
/// short names resolved, as Git prints the repository root. Config lookup,
/// globs, the changed set, and reports all use this one spelling. A path that
/// does not resolve is returned as given.
pub fn physical(path: &Path) -> PathBuf {
    path.canonicalize()
        .map_or_else(|_| path.to_path_buf(), plain)
}

/// On Windows `canonicalize` returns a verbatim path (`\\?\C:\repo`), which
/// never compares equal to the `C:/repo` that Git prints.
#[cfg(windows)]
fn plain(path: PathBuf) -> PathBuf {
    let plain = path
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
        .filter(|rest| rest.as_bytes().get(1) == Some(&b':'))
        .map(PathBuf::from);
    plain.unwrap_or(path)
}

#[cfg(not(windows))]
fn plain(path: PathBuf) -> PathBuf {
    path
}

/// A path Git printed, which always uses `/`, with native separators. The
/// separators change in the string, because pushing a component such as
/// `a:b.js` would replace the path on Windows.
fn git_path(path: &str) -> PathBuf {
    PathBuf::from(path.replace('/', std::path::MAIN_SEPARATOR_STR))
}

/// A relative path in the `/` form Git expects. The separators change in
/// the string, for the reason `git_path` gives.
fn slash_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn merge_base(repo_root: &Path, base: &str) -> Result<String> {
    // A leading dash would reach Git as an option instead of a revision.
    if base.is_empty() {
        bail!("--base needs a Git ref");
    }
    if base.starts_with('-') {
        bail!("--base {base} is not a Git ref");
    }
    let commit = git_line(
        repo_root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{base}^{{commit}}"),
        ],
    )?
    .with_context(|| format!("--base {base} does not name a commit"))?;
    git_line(repo_root, &["merge-base", &commit, "HEAD"])?
        .with_context(|| format!("--base {base} has no merge base with HEAD; fetch more history"))
}

/// Trimmed stdout of a Git command, or `None` when Git exits nonzero.
fn git_line(cwd: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = git_command(cwd)
        .args(args)
        .output()
        .with_context(|| format!("failed to execute git {}", args[0]))?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
}

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    git_command(cwd)
        .args(args)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git_command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(cwd).arg("--no-pager").args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.useBuiltinFSMonitor=false",
        "-c",
        "diff.external=",
        "-c",
        "core.hooksPath=/dev/null",
    ]);
    for name in REMOVED_GIT_ENV {
        command.env_remove(name);
    }
    command
}

/// Whether the index tracks `path`, relative to the repository root, as a
/// regular file whose changes a diff shows: one stage-0 entry at exactly
/// that path, with a file mode and the tag `H`, so neither a symbolic link
/// nor a skip-worktree or assume-unchanged entry. A Git failure, such as a
/// path inside a submodule, is `false`.
pub fn is_plainly_tracked(repo_root: &Path, path: &Path) -> bool {
    let path = slash_path(path);
    let Ok(output) = git_command(repo_root)
        .args(["ls-files", "--stage", "-v", "-z", "--"])
        .arg(Pathspec::File(Path::new(&path)).arg())
        .output()
    else {
        return false;
    };
    let listing = String::from_utf8_lossy(&output.stdout);
    let mut entries = listing.split('\0').filter(|entry| !entry.is_empty());
    let entry = match (entries.next(), entries.next()) {
        (Some(entry), None) if output.status.success() => entry,
        _ => return false,
    };
    entry.split_once('\t').is_some_and(|(meta, name)| {
        let fields = meta.split(' ').collect::<Vec<_>>();
        name == path && matches!(fields[..], ["H", "100644" | "100755", _, "0"])
    })
}

/// The untracked files that Git does not ignore.
pub fn untracked_files(cwd: &Path) -> Result<Vec<PathBuf>> {
    let output = git_command(cwd)
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .output()
        .context("failed to list untracked files")?;
    if !output.status.success() {
        bail!("git ls-files failed")
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| git_path(&String::from_utf8_lossy(part)))
        .collect())
}

pub fn parse_diff_hunks(diff: &str) -> BTreeMap<PathBuf, Vec<LineRange>> {
    let mut result = BTreeMap::new();
    let mut file = None;
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            file = diff_path(path);
            if let Some(path) = &file {
                result.entry(path.clone()).or_insert_with(Vec::new);
            }
            continue;
        }
        if !line.starts_with("@@") {
            continue;
        }
        let Some(path) = file.as_ref() else { continue };
        if let Some(range) = post_image_range(line) {
            result
                .entry(path.clone())
                .or_insert_with(Vec::new)
                .push(range);
        }
    }
    result
}

fn diff_path(value: &str) -> Option<PathBuf> {
    if value == "/dev/null" {
        return None;
    }
    let value = value.split('\t').next().unwrap_or(value);
    let path = value.strip_prefix("b/")?;
    Some(git_path(path))
}

fn post_image_range(header: &str) -> Option<LineRange> {
    let plus = header
        .split_whitespace()
        .find(|part| part.starts_with('+'))?;
    let mut values = plus.trim_start_matches('+').split(',');
    let start = values.next()?.parse().ok()?;
    let count = values
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    (count > 0).then_some(LineRange {
        start,
        end: start + count - 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_status_maps_renames_and_lists_every_path() {
        let output = "M\0kept.js\0R087\0old dir/a.js\0new\tdir/a.js\0C100\0src.js\0copy.js\0D\0gone.js\0A\0added.js\0";
        let DiffStatus { renames, touched } = parse_name_status(output);
        assert_eq!(
            renames,
            BTreeMap::from([(
                PathBuf::from("new\tdir/a.js"),
                PathBuf::from("old dir/a.js")
            )])
        );
        assert_eq!(
            touched,
            [
                "kept.js",
                "new\tdir/a.js",
                "old dir/a.js",
                "copy.js",
                "src.js",
                "gone.js",
                "added.js",
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn git_paths_round_trip_without_losing_components() {
        for git in ["src/sub/a.js", "a:b.js", "dir/a:b.js", "top.js"] {
            assert_eq!(slash_path(&git_path(git)), git);
        }
        assert_eq!(
            git_path("src/sub/a.js"),
            Path::new("src").join("sub").join("a.js")
        );
    }

    #[cfg(windows)]
    #[test]
    fn physical_paths_are_not_verbatim() {
        let dir = std::env::temp_dir();
        let text = physical(&dir).to_string_lossy().into_owned();
        assert!(!text.starts_with(r"\\?\"), "{text}");
        assert_eq!(physical(&physical(&dir)), physical(&dir));
    }

    #[test]
    fn synthetic_hunks_use_post_image_and_skip_deletions() {
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\t\n@@ -2,2 +2,3 @@\n@@ -10,2 +11,0 @@\n@@ -20 +19 @@\n";
        let spans = parse_diff_hunks(diff);
        assert_eq!(
            spans[Path::new("a.rs")],
            vec![
                LineRange { start: 2, end: 4 },
                LineRange { start: 19, end: 19 },
            ]
        );
    }
}
