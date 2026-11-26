use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
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
    /// When set, each base read is kept, so a second scan of this diff runs
    /// no Git command twice.
    pub base_reads: Option<BaseReads>,
}

/// The base reads of one diff, shared by its clones.
#[derive(Clone, Debug, Default)]
pub struct BaseReads(Arc<Mutex<BTreeMap<PathBuf, BaseBlob>>>);

impl ChangedFiles {
    /// `path` at `commit`, as `base_blob` reads it, from `base_reads` when
    /// it holds the path.
    pub fn base_blob(&self, commit: &str, path: &Path) -> BaseBlob {
        let Some(BaseReads(reads)) = &self.base_reads else {
            return base_blob(&self.repo_root, commit, path);
        };
        let mut reads = reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reads
            .entry(path.to_path_buf())
            .or_insert_with(|| base_blob(&self.repo_root, commit, path))
            .clone()
    }
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

/// The files in the Git directory that mark a merge, cherry-pick, revert,
/// rebase, or `am` that stopped before its commit.
const OPERATION_MARKERS: &[&str] = &[
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "rebase-merge",
    "rebase-apply",
];

/// A Git work tree and the commit `HEAD` names.
#[derive(Clone, Debug)]
pub struct Repository {
    /// The physical root.
    pub root: PathBuf,
    /// The Git directory of this work tree.
    pub git_dir: PathBuf,
    pub head: String,
}

impl Repository {
    /// Whether a merge, cherry-pick, revert, or rebase is in progress.
    pub fn operation_in_progress(&self) -> bool {
        OPERATION_MARKERS
            .iter()
            .any(|marker| fs::symlink_metadata(self.git_dir.join(marker)).is_ok())
    }
}

/// The repository around `cwd` with its Git directory and `HEAD` commit,
/// from one Git command. `None` outside a repository and in one without
/// commits.
pub fn repository(cwd: &Path) -> Result<Option<Repository>> {
    let lines = git_line(
        cwd,
        &[
            "rev-parse",
            "--show-toplevel",
            "--absolute-git-dir",
            "--verify",
            "--quiet",
            "HEAD^{commit}",
        ],
    )?;
    let Some([root, git_dir, head]) = lines.as_deref().and_then(|text| {
        let lines = text.lines().map(str::trim).collect::<Vec<_>>();
        <[&str; 3]>::try_from(lines).ok()
    }) else {
        return Ok(None);
    };
    let root = physical(Path::new(root));
    remember_root(cwd, Some(root.clone()));
    Ok(Some(Repository {
        root,
        git_dir: PathBuf::from(git_dir),
        head: head.to_owned(),
    }))
}

/// The paths with unmerged index entries, each once.
pub fn unmerged_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let output = git_command(repo_root)
        .args(["ls-files", "--unmerged", "-z"])
        .output()
        .context("failed to list unmerged files")?;
    if !output.status.success() {
        bail!("git ls-files failed");
    }
    let mut paths = String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter_map(|entry| entry.split_once('\t').map(|(_, path)| git_path(path)))
        .collect::<Vec<_>>();
    paths.dedup();
    Ok(paths)
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
        base_reads: None,
    })
}

/// The changed line ranges of each file from `from` to the working tree.
/// Every file is diffed as text, so neither a `-diff` or `binary` attribute
/// nor a NUL byte hides its hunks.
pub fn diff_spans(
    repo_root: &Path,
    from: &str,
    paths: &[Pathspec<'_>],
) -> Result<BTreeMap<PathBuf, Vec<LineRange>>> {
    let hunks = git_diff(repo_root, from, &["--unified=0", "--text"], paths)?;
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

/// A path's content in the tree of a commit, read through links that stay
/// inside the tree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TreeFile {
    /// No file: no entry, a directory, or a path through a file.
    Missing,
    Content(Vec<u8>),
    /// A link that leaves the tree, dangles, or loops, or a path that cannot
    /// be asked for.
    Unresolved,
}

/// One path of `files_at`: a line of the batch request, or an answer that
/// needs no request.
enum TreeRequest {
    Batch(String),
    Answer(TreeFile),
}

/// The content of each of `paths`, relative to the repository root, at
/// `commit`, from one `cat-file --batch --follow-symlinks`, or none when no
/// path needs it. The batch request is line-based, so a path with a newline
/// is first resolved by one `ls-tree` that takes the paths as arguments.
pub(crate) fn files_at(repo_root: &Path, commit: &str, paths: &[PathBuf]) -> Result<Vec<TreeFile>> {
    let paths = paths
        .iter()
        .map(|path| slash_path(path))
        .collect::<Vec<_>>();
    let entries = newline_entries(repo_root, commit, &paths)?;
    let requests = paths
        .iter()
        .map(|path| {
            if path.contains('\n') {
                newline_request(&entries, path)
            } else {
                TreeRequest::Batch(format!("{commit}:{path}\n"))
            }
        })
        .collect::<Vec<_>>();
    let input = requests
        .iter()
        .filter_map(|request| match request {
            TreeRequest::Batch(line) => Some(line.as_str()),
            TreeRequest::Answer(_) => None,
        })
        .collect::<String>();
    let output = if input.is_empty() {
        Vec::new()
    } else {
        batch(repo_root, input)?
    };
    let mut rest = output.as_slice();
    requests
        .into_iter()
        .map(|request| match request {
            TreeRequest::Batch(_) => next_tree_file(&mut rest),
            TreeRequest::Answer(answer) => Ok(answer),
        })
        .collect()
}

/// The output of `cat-file --batch --follow-symlinks` for `input`.
fn batch(repo_root: &Path, input: String) -> Result<Vec<u8>> {
    let mut child = git_command(repo_root)
        .args(["cat-file", "--batch", "--follow-symlinks"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to execute git cat-file")?;
    let mut stdin = child.stdin.take().context("cannot write to git cat-file")?;
    // Written apart from the read, so a full output pipe cannot block it.
    let writer = thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().context("git cat-file failed")?;
    let written = writer.join();
    if !output.status.success() || !matches!(written, Ok(Ok(()))) {
        bail!(
            "git cat-file failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

/// A tree entry that `ls-tree` lists: its mode, type, and object name.
struct TreeEntry {
    mode: String,
    kind: String,
    object: String,
}

/// The entries at `commit` of every path in `paths` that has a newline and
/// of each of its directories, by path, from one `ls-tree -t`. No such path
/// runs no command.
fn newline_entries(
    repo_root: &Path,
    commit: &str,
    paths: &[String],
) -> Result<BTreeMap<String, TreeEntry>> {
    let mut specs = std::collections::BTreeSet::new();
    for path in paths.iter().filter(|path| path.contains('\n')) {
        let mut end = 0;
        for part in path.split('/') {
            end += part.len();
            specs.insert(&path[..end]);
            end += 1;
        }
    }
    if specs.is_empty() {
        return Ok(BTreeMap::new());
    }
    let output = git_command(repo_root)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args(["ls-tree", "-z", "-t", commit, "--"])
        .args(specs)
        .output()
        .context("failed to execute git ls-tree")?;
    if !output.status.success() {
        bail!(
            "git ls-tree failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    Ok(listing
        .split('\0')
        .filter_map(|entry| {
            let (meta, path) = entry.split_once('\t')?;
            let mut fields = meta.split(' ').map(str::to_owned);
            let entry = TreeEntry {
                mode: fields.next()?,
                kind: fields.next()?,
                object: fields.next()?,
            };
            Some((path.to_owned(), entry))
        })
        .collect())
}

/// The request for `path`, which has a newline, from its `ls-tree`
/// entries: its blob by object name, or the answer when a directory on the
/// way is missing or not a tree. A link on the way cannot be followed here,
/// so it is unresolved.
fn newline_request(entries: &BTreeMap<String, TreeEntry>, path: &str) -> TreeRequest {
    let mut end = 0;
    for part in path.split('/') {
        end += part.len();
        let entry = entries.get(&path[..end]);
        let last = end == path.len();
        end += 1;
        match entry {
            None => return TreeRequest::Answer(TreeFile::Missing),
            Some(entry) if entry.mode == "120000" => {
                return TreeRequest::Answer(TreeFile::Unresolved);
            }
            Some(entry) if last && entry.kind == "blob" => {
                return TreeRequest::Batch(format!("{}\n", entry.object));
            }
            Some(entry) if last || entry.kind != "tree" => {
                return TreeRequest::Answer(TreeFile::Missing);
            }
            Some(_) => {}
        }
    }
    TreeRequest::Answer(TreeFile::Missing)
}

/// Reads one `cat-file --batch --follow-symlinks` answer from `rest`.
fn next_tree_file(rest: &mut &[u8]) -> Result<TreeFile> {
    let end = rest
        .iter()
        .position(|byte| *byte == b'\n')
        .context("truncated git cat-file output")?;
    let header = String::from_utf8_lossy(&rest[..end]).into_owned();
    *rest = &rest[end + 1..];
    if header.ends_with(" missing") {
        return Ok(TreeFile::Missing);
    }
    let fields = header.split(' ').collect::<Vec<_>>();
    let (kind, size) = match fields[..] {
        [_, kind, size] | [kind, size] => (kind, size),
        _ => bail!("unexpected git cat-file output {header}"),
    };
    let size = size
        .parse::<usize>()
        .with_context(|| format!("unexpected git cat-file output {header}"))?;
    let body = rest
        .get(..size)
        .filter(|_| rest.get(size) == Some(&b'\n'))
        .context("truncated git cat-file output")?;
    *rest = &rest[size + 1..];
    Ok(match (fields.len(), kind) {
        (3, "blob") => TreeFile::Content(body.to_vec()),
        (3, _) | (2, "notdir") => TreeFile::Missing,
        _ => TreeFile::Unresolved,
    })
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
fn read_blob(repo_root: &Path, object: &str) -> Option<Vec<u8>> {
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
    fn batch_answers_follow_links_only_inside_the_tree() {
        let output = concat!(
            "0123 blob 2\n{}\n",
            "c:x/.pickcheck.json missing\n",
            "4567 tree 3\nabc\n",
            "notdir 4\nc:ab\n",
            "symlink 9\n../out.js\n",
            "dangling 4\nc:ab\n",
            "loop 4\nc:ab\n",
        );
        let mut rest = output.as_bytes();
        let answers = (0..7)
            .map(|_| next_tree_file(&mut rest).unwrap())
            .collect::<Vec<_>>();
        assert!(rest.is_empty());
        use TreeFile::{Content, Missing, Unresolved};
        let expected = [Content(b"{}".to_vec()), Missing, Missing, Missing];
        assert_eq!(answers[..4], expected);
        assert_eq!(answers[4..], [Unresolved, Unresolved, Unresolved]);
        for broken in ["0123 blob 5\n{}\n", "odd\n", "0123 blob x\n", "no newline"] {
            assert!(next_tree_file(&mut broken.as_bytes()).is_err(), "{broken}");
        }
    }

    /// A path with a newline cannot be asked for in a batch line; its
    /// directories decide its answer from one `ls-tree`.
    #[cfg(unix)]
    #[test]
    fn paths_with_a_newline_resolve_through_their_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = physical(dir.path());
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        fs::create_dir_all(root.join("a\nb/sub")).unwrap();
        fs::create_dir(root.join("real")).unwrap();
        fs::write(root.join("a\nb/c.json"), "{}").unwrap();
        fs::write(root.join("a\nb/file"), "").unwrap();
        fs::write(root.join("real/c.json"), "{}").unwrap();
        std::os::unix::fs::symlink("real", root.join("l\nk")).unwrap();
        std::os::unix::fs::symlink("c.json", root.join("a\nb/link.json")).unwrap();
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@e.x",
            "commit",
            "-qm",
            "c",
        ]);
        let paths = [
            "a\nb/c.json",
            "a\nb/sub/c.json",
            "a\nb/sub",
            "a\nb/file/c.json",
            "a\nb/link.json",
            "l\nk/c.json",
            "gone\n/c.json",
            "real/c.json",
        ]
        .map(PathBuf::from);
        use TreeFile::{Content, Missing, Unresolved};
        assert_eq!(
            files_at(&root, "HEAD", &paths).unwrap(),
            [
                Content(b"{}".to_vec()),
                Missing,
                Missing,
                Missing,
                Unresolved,
                Unresolved,
                Missing,
                Content(b"{}".to_vec()),
            ]
        );
        let only = files_at(&root, "HEAD", &[PathBuf::from("gone\n/c.json")]).unwrap();
        assert_eq!(only, [Missing]);
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
