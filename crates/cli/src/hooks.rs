use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use pickcheck_core::{
    BaseReads, CONFIG_CHANGED_NOTE, ChangedFiles, CommitConfigs, Config, DiffStatus, LineRange,
    Pathspec, Repository, ScanOptions, ScanResult, config_changed, diff_spans, diff_status,
    is_plainly_tracked, load_config, load_config_at, merge_base_with_head, physical, repository,
    resolve_path, scan, unmerged_files, untracked_files,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{Scope, report, session::Session};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
    Cursor,
    Grok,
}

#[derive(Debug)]
struct HookInput {
    hook_event_name: String,
    session_id: String,
    cwd: Option<PathBuf>,
    workspace_root: Option<PathBuf>,
    workspace_roots: Vec<PathBuf>,
    tool_name: Option<String>,
    tool_input: Value,
    file_path: Option<PathBuf>,
    status: Option<String>,
}

impl<'de> Deserialize<'de> for HookInput {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut map = Map::<String, Value>::deserialize(deserializer)?;
        let hook_event_name = take_string(&mut map, &["hook_event_name", "hookEventName"])
            .ok_or_else(|| serde::de::Error::missing_field("hook_event_name"))?;
        Ok(Self {
            hook_event_name,
            session_id: take_string(&mut map, &["session_id", "sessionId", "conversation_id"])
                .unwrap_or_default(),
            cwd: take_path(&mut map, &["cwd"]),
            workspace_root: take_path(&mut map, &["workspace_root", "workspaceRoot"]),
            workspace_roots: take_path_list(&mut map, &["workspace_roots", "workspaceRoots"]),
            tool_name: take_string(&mut map, &["tool_name", "toolName"]),
            tool_input: take_value(&mut map, &["tool_input", "toolInput"]).unwrap_or(Value::Null),
            file_path: take_path(&mut map, &["file_path", "filePath"]),
            status: take_string(&mut map, &["status"]),
        })
    }
}

fn take_value(map: &mut Map<String, Value>, keys: &[&str]) -> Option<Value> {
    keys.iter().find_map(|key| map.remove(*key))
}

fn take_string(map: &mut Map<String, Value>, keys: &[&str]) -> Option<String> {
    take_value(map, keys).and_then(|value| match value {
        Value::String(text) => Some(text),
        other => other.as_str().map(str::to_owned),
    })
}

fn take_path(map: &mut Map<String, Value>, keys: &[&str]) -> Option<PathBuf> {
    take_string(map, keys).map(PathBuf::from)
}

fn take_path_list(map: &mut Map<String, Value>, keys: &[&str]) -> Vec<PathBuf> {
    match take_value(map, keys) {
        Some(Value::Array(entries)) => entries
            .into_iter()
            .filter_map(|entry| entry.as_str().map(PathBuf::from))
            .collect(),
        _ => Vec::new(),
    }
}

enum Event {
    Ignore,
    File(PathBuf),
    Changed,
    Stop,
}

pub fn run(harness: Harness) -> Result<u8> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .context("cannot read hook input")?;
    let input: HookInput = serde_json::from_str(&input).context("invalid hook JSON")?;
    handle(harness, &input)
}

fn handle(harness: Harness, input: &HookInput) -> Result<u8> {
    let cwd = working_directory(input);
    match classify(harness, input) {
        Event::Ignore => {
            record_only(input, &cwd);
            Ok(0)
        }
        Event::File(path) => report_post_edit(harness, input, &cwd, path),
        Event::Changed => report_changed(harness, input, &cwd, false),
        Event::Stop => report_changed(harness, input, &cwd, true),
    }
}

fn classify(harness: Harness, input: &HookInput) -> Event {
    if stop_event(harness, input) {
        return Event::Stop;
    }
    if harness == Harness::Cursor && input.hook_event_name == "afterFileEdit" {
        return input.file_path.clone().map_or(Event::Changed, Event::File);
    }
    if post_tool_event(harness, &input.hook_event_name)
        && post_tool(harness, input.tool_name.as_deref())
    {
        return file_path(&input.tool_input)
            .or_else(|| input.file_path.clone())
            .map_or(Event::Changed, Event::File);
    }
    Event::Ignore
}

fn stop_event(harness: Harness, input: &HookInput) -> bool {
    let event_matches = match harness {
        Harness::Claude | Harness::Codex => input.hook_event_name == "Stop",
        Harness::Cursor | Harness::Grok => {
            matches!(input.hook_event_name.as_str(), "Stop" | "stop")
        }
    };
    event_matches
        && input
            .status
            .as_deref()
            .is_none_or(|status| status == "completed")
}

fn post_tool_event(harness: Harness, event: &str) -> bool {
    match harness {
        Harness::Claude | Harness::Codex => event == "PostToolUse",
        Harness::Cursor | Harness::Grok => matches!(event, "PostToolUse" | "post_tool_use"),
    }
}

fn post_tool(harness: Harness, tool: Option<&str>) -> bool {
    match harness {
        Harness::Claude => matches!(tool, Some("Edit" | "Write" | "MultiEdit")),
        Harness::Codex => matches!(tool, Some("apply_patch" | "Edit" | "Write")),
        Harness::Cursor | Harness::Grok => matches!(
            tool,
            Some(
                "apply_patch"
                    | "edit"
                    | "write"
                    | "search_replace"
                    | "Edit"
                    | "Write"
                    | "MultiEdit"
            )
        ),
    }
}

fn file_path(input: &Value) -> Option<PathBuf> {
    input
        .get("file_path")
        .or_else(|| input.get("filePath"))
        .or_else(|| input.get("path"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

type Spans = BTreeMap<PathBuf, Vec<LineRange>>;

/// Recorded files that no longer exist, passed beyond this count, would make
/// a post-edit diff command too long; the diff then covers the whole tree.
const MAX_RENAME_SOURCES: usize = 100;

/// What a hook measures.
enum Mode {
    /// The whole edited file, as outside a session.
    Paths,
    /// The diff against `HEAD`; every violation blocks.
    Head(ChangedFiles),
    /// The session's diff since `base`, compared with `base`, where only
    /// `hook.fail_on` statuses block.
    Session {
        changes: ChangedFiles,
        base: String,
        /// Set when a `.pickcheck.json` differs from `base`: the gate then
        /// scans with the configs of `base` and with those of the working
        /// tree, and blocks when either scan blocks.
        configs: Option<CommitConfigs>,
    },
}

/// The configs of a scan: from the base commit when a session config
/// differs from it, otherwise from the working tree, as without a session.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Base,
    WorkingTree,
}

/// The first note of a block reason from the working-tree configs.
const WORKING_TREE_NOTE: &str =
    ".pickcheck.json changed in this diff; the working-tree config also applies";

struct Gate {
    mode: Mode,
    notes: Vec<String>,
}

impl Gate {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            notes: Vec::new(),
        }
    }

    fn scan(&self, cwd: &Path, paths: &[PathBuf], side: Side) -> Result<ScanResult> {
        let (changed, commit_configs) = match &self.mode {
            Mode::Paths => (None, None),
            Mode::Head(changes) => (Some(changes), None),
            Mode::Session {
                changes, configs, ..
            } => (
                Some(changes),
                configs.as_ref().filter(|_| side == Side::Base),
            ),
        };
        scan(&ScanOptions {
            cwd,
            paths,
            explicit_config: None,
            changed,
            commit_configs,
        })
    }

    /// Whether a session config differs from the base, so the gate scans
    /// with the configs of both sides.
    fn compares_configs(&self) -> bool {
        matches!(
            self.mode,
            Mode::Session {
                configs: Some(_),
                ..
            }
        )
    }

    fn config(&self, cwd: &Path, side: Side) -> Result<Config> {
        let resolution = match (&self.mode, side) {
            (
                Mode::Session {
                    configs: Some(configs),
                    ..
                },
                Side::Base,
            ) => load_config_at(cwd, configs)?,
            _ => load_config(cwd, None)?,
        };
        Ok(resolution.config)
    }

    /// The feedback for `result`, or `None` when no violation blocks.
    fn feedback(&self, cwd: &Path, result: &ScanResult, side: Side) -> Result<Option<String>> {
        if result.violations.is_empty() {
            return Ok(None);
        }
        let fail_on = match self.mode {
            Mode::Session { .. } => self.config(cwd, side)?.hook.fail_on,
            _ => Vec::new(),
        };
        let (scope, scan_notes) = match &self.mode {
            Mode::Paths => (Scope::Paths, &[][..]),
            Mode::Head(_) => (Scope::Changed, &[][..]),
            Mode::Session { base, .. } => (
                Scope::Base {
                    base,
                    fail_on: Some(&fail_on),
                },
                result.notes.as_slice(),
            ),
        };
        if !result.violations.iter().any(|item| scope.fails(item)) {
            return Ok(None);
        }
        let working_tree = (side == Side::WorkingTree).then_some(WORKING_TREE_NOTE);
        let scan_notes = scan_notes
            .iter()
            .map(String::as_str)
            .filter(|note| working_tree.is_none() || *note != CONFIG_CHANGED_NOTE);
        let notes = self.notes.iter().map(String::as_str).chain(working_tree);
        let notes = notes
            .chain(scan_notes)
            .map(|note| format!("note: {note}\n"));
        Ok(Some(
            notes.collect::<String>() + &report::summary(result, scope),
        ))
    }
}

/// The repository around `cwd` at this event: `HEAD`, and the files that are
/// changed or untracked against it.
#[derive(Clone)]
struct Snapshot {
    repo: Repository,
    status: DiffStatus,
    untracked: Vec<PathBuf>,
}

impl Snapshot {
    fn read(repo: Repository) -> Result<Self> {
        let status = diff_status(&repo.root, &repo.head, &[])?;
        let untracked = untracked_files(&repo.root)?;
        Ok(Self {
            repo,
            status,
            untracked,
        })
    }

    /// The `HEAD` diff, as outside a session.
    fn head_changes(self) -> Result<ChangedFiles> {
        let spans = diff_spans(&self.repo.root, &self.repo.head, &[])?;
        Ok(self.changes(spans, None, None))
    }

    /// `status` replaces the names diff against `HEAD` when given.
    fn changes(
        self,
        spans: Spans,
        status: Option<DiffStatus>,
        base: Option<String>,
    ) -> ChangedFiles {
        let status = status.unwrap_or(self.status);
        ChangedFiles {
            repo_root: self.repo.root,
            spans,
            untracked: self.untracked,
            renames: status.renames,
            touched: status.touched,
            base,
            fallback: false,
            base_reads: None,
            measure_untouched: false,
        }
    }
}

/// Records an event that the hook does not check. A failure is only a note.
fn record_only(input: &HookInput, cwd: &Path) {
    if input.session_id.is_empty() {
        return;
    }
    let recorded = repository(cwd).and_then(|repo| {
        let Some(repo) = repo else {
            return Ok(());
        };
        let snapshot = Snapshot::read(repo)?;
        open_session(input, &snapshot, None).map(drop)
    });
    if let Err(error) = recorded {
        note_without_session(&error);
    }
}

fn report_post_edit(harness: Harness, input: &HookInput, cwd: &Path, path: PathBuf) -> Result<u8> {
    let session = post_edit_gate(input, cwd, &path);
    let paths = [path];
    let outcome = Outcome::of(session, || Ok(Gate::new(Mode::Paths)), cwd, &paths)?;
    match outcome.report {
        Some(report) => emit_feedback(harness, &report, false),
        None => {
            outcome.note_pass();
            Ok(0)
        }
    }
}

/// `Stop`, and an edit event that names no file.
fn report_changed(harness: Harness, input: &HookInput, cwd: &Path, stop: bool) -> Result<u8> {
    let Some(repo) = repository(cwd)? else {
        eprintln!(
            "note: hook skipped: {} is not inside a Git repository with HEAD",
            cwd.display()
        );
        return pass(input, stop);
    };
    let snapshot = Snapshot::read(repo)?;
    let session = changed_gate(input, cwd, &snapshot);
    let head = || Ok(Gate::new(Mode::Head(snapshot.clone().head_changes()?)));
    let outcome = Outcome::of(session, head, cwd, &[])?;
    let Some(report) = &outcome.report else {
        outcome.note_pass();
        return pass(input, stop);
    };
    if !stop {
        return emit_feedback(harness, report, false);
    }
    // From the base when a config differs, so the working tree cannot lower it.
    let max = outcome.gate.config(cwd, Side::Base)?.hook.max_blocks;
    if increment_counter(&input.session_id)? > max {
        eprintln!("UNRESOLVED\n{report}");
        return Ok(0);
    }
    emit_feedback(
        harness,
        &format!("{report}Fix the listed files, then finish."),
        true,
    )
}

/// Only a clean `Stop` resets the loop guard.
fn pass(input: &HookInput, stop: bool) -> Result<u8> {
    if stop {
        reset_counter(&input.session_id)?;
    }
    Ok(0)
}

/// A scan and its block reason, `None` when no violation blocks.
struct Outcome {
    gate: Gate,
    result: ScanResult,
    report: Option<String>,
}

impl Outcome {
    /// Scans with the session gate. Without one, or after a note when the
    /// session gate or its scan fails, scans with the `plain` gate of the
    /// hook without a session id. An error of the plain gate is returned
    /// without the note, as the hook without a session id returns it.
    fn of(
        session: Result<Option<Gate>>,
        plain: impl FnOnce() -> Result<Gate>,
        cwd: &Path,
        paths: &[PathBuf],
    ) -> Result<Self> {
        let session =
            session.and_then(|gate| gate.map(|gate| Self::scan(gate, cwd, paths)).transpose());
        let failure = match session {
            Ok(Some(outcome)) => return Ok(outcome),
            Ok(None) => None,
            Err(error) => Some(error),
        };
        let outcome = Self::scan(plain()?, cwd, paths)?;
        if let Some(error) = failure {
            note_without_session(&error);
        }
        Ok(outcome)
    }

    /// Writes the notes of a run that does not block to stderr: the gate's
    /// own notes, which a block reason would start with, then the unverified
    /// files.
    fn note_pass(&self) {
        for text in &self.gate.notes {
            note(&format!("note: {text}\n"));
        }
        note_unverified(&self.result);
    }

    /// Scans with the gate's configs. When a session config differs from the
    /// base and that scan does not block, scans again with the working-tree
    /// configs, which then give the block reason.
    fn scan(gate: Gate, cwd: &Path, paths: &[PathBuf]) -> Result<Self> {
        let mut result = gate.scan(cwd, paths, Side::Base)?;
        let mut report = gate.feedback(cwd, &result, Side::Base)?;
        if report.is_none() && gate.compares_configs() {
            let working = gate.scan(cwd, paths, Side::WorkingTree)?;
            report = gate.feedback(cwd, &working, Side::WorkingTree)?;
            for item in working.unverified {
                if !result.unverified.contains(&item) {
                    result.unverified.push(item);
                }
            }
            result.unverified.sort_by(|a, b| a.file.cmp(&b.file));
        }
        Ok(Self {
            gate,
            result,
            report,
        })
    }
}

/// The session gate for an edit of `path`: the session's changes in that
/// file. `None` without a session id, outside a Git repository with
/// `HEAD`, and for a file outside the repository. Every error, such as
/// unusable session state, is the session's.
fn post_edit_gate(input: &HookInput, cwd: &Path, path: &Path) -> Result<Option<Gate>> {
    if input.session_id.is_empty() {
        return Ok(None);
    }
    let Some(repo) = repository(cwd)? else {
        return Ok(None);
    };
    let edited = resolve_path(cwd, path)
        .strip_prefix(&repo.root)
        .ok()
        .map(Path::to_path_buf);
    let snapshot = Snapshot::read(repo)?;
    let session = open_session(input, &snapshot, edited.as_deref())?;
    edited
        .map(|edited| session_gate(cwd, snapshot, &session, Some(&edited)))
        .transpose()
}

/// The session gate for `Stop` and for an edit event that names no file.
/// `None` without a session id. Every error, such as unusable session
/// state, is the session's.
fn changed_gate(input: &HookInput, cwd: &Path, snapshot: &Snapshot) -> Result<Option<Gate>> {
    if input.session_id.is_empty() {
        return Ok(None);
    }
    let session = open_session(input, snapshot, None)?;
    session_gate(cwd, snapshot.clone(), &session, None).map(Some)
}

/// The session's changes since its base, restricted to the files it
/// recorded, or to `edited` for an edit. When the anchor is unusable the
/// gate is the one outside a session, with a note.
fn session_gate(
    cwd: &Path,
    snapshot: Snapshot,
    session: &Session,
    edited: Option<&Path>,
) -> Result<Gate> {
    let repo = &snapshot.repo;
    let base = if session.anchor() == repo.head {
        Some(repo.head.clone())
    } else {
        merge_base_with_head(&repo.root, session.anchor())?
    };
    let Some(base) = base else {
        let mode = match edited {
            Some(_) => Mode::Paths,
            None => Mode::Head(snapshot.head_changes()?),
        };
        return Ok(Gate {
            notes: vec![format!(
                "session anchor {} does not resolve or has no merge base with HEAD; the hook checks the diff against HEAD",
                session.anchor()
            )],
            ..Gate::new(mode)
        });
    };
    let (mut spans, status) = session_diff(&snapshot, session, &base, edited)?;
    // Hunks can miss changed lines, so every file that the names diff lists
    // with a post-image is scanned, also when it has no hunk.
    let names = status.as_ref().unwrap_or(&snapshot.status);
    for path in &names.post_images {
        spans.entry(path.clone()).or_default();
    }
    let mut changes = snapshot.changes(spans, status, Some(base.clone()));
    changes.measure_untouched = true;
    let renames = &changes.renames;
    let kept = |path: &PathBuf| {
        session.contains(path) || renames.get(path).is_some_and(|old| session.contains(old))
    };
    changes.spans.retain(|path, _| kept(path));
    changes.untracked.retain(kept);
    if let Some(edited) = edited {
        add_edited_outside_diff(&mut changes, edited);
    }
    check_links_whole(&mut changes);
    keep_only_files(&mut changes);
    let configs = base_configs(cwd, &changes, &base, edited)?;
    if configs.is_some() {
        changes.base_reads = Some(BaseReads::default());
    }
    Ok(Gate::new(Mode::Session {
        changes,
        base,
        configs,
    }))
}

/// Moves every file with a post-image that is a symbolic link on disk to the
/// untracked files. Git diffs the link text, not the target, so such a file
/// is checked whole with no base.
fn check_links_whole(changes: &mut ChangedFiles) {
    let root = &changes.repo_root;
    let links = changes
        .spans
        .keys()
        .filter(|path| {
            fs::symlink_metadata(root.join(path)).is_ok_and(|meta| meta.file_type().is_symlink())
        })
        .cloned()
        .collect::<Vec<_>>();
    for link in links {
        changes.spans.remove(&link);
        changes.untracked.push(link);
    }
}

/// Keeps only the changed entries that are files. Git lists an untracked
/// nested repository or work tree as its directory, and a submodule, a
/// dangling link, or a link to a directory is no file either. Such an entry
/// is neither scanned nor a start of a config lookup.
fn keep_only_files(changes: &mut ChangedFiles) {
    let root = &changes.repo_root;
    let is_file = |path: &PathBuf| root.join(path).is_file();
    changes.spans.retain(|path, _| is_file(path));
    changes.untracked.retain(is_file);
}

/// The configs of `base` when the config of a checked file or of `cwd`
/// differs from it, or when the diff changes any config. `None` also when
/// no file is checked.
fn base_configs(
    cwd: &Path,
    changes: &ChangedFiles,
    base: &str,
    edited: Option<&Path>,
) -> Result<Option<CommitConfigs>> {
    let root = &changes.repo_root;
    let checked = match edited {
        Some(edited) => vec![edited]
            .into_iter()
            .filter(|path| in_diff(changes, path))
            .collect(),
        None => changes
            .spans
            .keys()
            .chain(&changes.untracked)
            .map(PathBuf::as_path)
            .collect::<Vec<_>>(),
    };
    // Every changed entry is a file (see `keep_only_files`).
    let checked = checked
        .into_iter()
        .map(|path| root.join(path))
        .collect::<Vec<_>>();
    if checked.is_empty() {
        return Ok(None);
    }
    let mut starts = vec![cwd.to_path_buf()];
    starts.extend(checked);
    let configs = CommitConfigs::read(root, base, &starts)?;
    let changed = config_changed(changes) || configs.differs_from_working_tree();
    Ok(changed.then_some(configs))
}

/// Adds `edited` when it exists and is in no diff. A plainly tracked file is
/// scanned with its base and no touched unit, so only `new` and `worsened`
/// violations remain; Git can hide a change, for example through a clean
/// filter. Any other such file, as an ignored file, a file of a nested
/// repository or a submodule, a skip-worktree entry, or a link, is checked
/// whole with no base, as an untracked file is.
fn add_edited_outside_diff(changes: &mut ChangedFiles, edited: &Path) {
    let root = &changes.repo_root;
    if in_diff(changes, edited) || fs::symlink_metadata(root.join(edited)).is_err() {
        return;
    }
    if is_plainly_tracked(root, edited) {
        changes.spans.insert(edited.to_path_buf(), Vec::new());
    } else {
        changes.untracked.push(edited.to_path_buf());
    }
}

/// Whether the diff has a post-image of `path` or lists it as untracked.
fn in_diff(changes: &ChangedFiles, path: &Path) -> bool {
    changes.spans.contains_key(path) || changes.untracked.iter().any(|item| item == path)
}

/// The diff from `base`, with the names diff when it differs from the one
/// against `HEAD`. For an edit it covers only the edited file, its rename
/// source, and the repository configs.
fn session_diff(
    snapshot: &Snapshot,
    session: &Session,
    base: &str,
    edited: Option<&Path>,
) -> Result<(Spans, Option<DiffStatus>)> {
    let root = &snapshot.repo.root;
    let candidates =
        edited.map_or_else(Vec::new, |edited| rename_candidates(root, session, edited));
    let status = if base == snapshot.repo.head {
        None
    } else {
        Some(diff_status(root, base, &candidates)?)
    };
    let Some(edited) = edited else {
        return Ok((diff_spans(root, base, &[])?, status));
    };
    let renames = &status.as_ref().unwrap_or(&snapshot.status).renames;
    let mut paths = vec![Pathspec::File(edited)];
    paths.extend(renames.get(edited).map(|old| Pathspec::File(old)));
    Ok((diff_spans(root, base, &paths)?, status))
}

/// The edited file, the recorded files that no longer exist, among which a
/// rename source would be, and the repository configs. Too many sources
/// widen the diff to the whole tree.
fn rename_candidates<'a>(root: &Path, session: &'a Session, edited: &'a Path) -> Vec<Pathspec<'a>> {
    let gone = session
        .files()
        .filter(|path| fs::symlink_metadata(root.join(path)).is_err())
        .map(|path| Pathspec::File(path))
        .collect::<Vec<_>>();
    if gone.len() > MAX_RENAME_SOURCES {
        return Vec::new();
    }
    let mut paths = vec![Pathspec::File(edited), Pathspec::RepoConfigs];
    paths.extend(gone);
    paths
}

/// Opens the session and records the edited file and every file that is
/// changed or untracked now. While a merge, cherry-pick, revert, or rebase
/// is in progress, the changed files come from it, so only the edited file
/// and the unmerged files are recorded.
fn open_session(input: &HookInput, snapshot: &Snapshot, edited: Option<&Path>) -> Result<Session> {
    let mut session = Session::open(
        &state_dir()?,
        &input.session_id,
        &snapshot.repo.root,
        &snapshot.repo.head,
    )?;
    let edited = edited.map(Path::to_path_buf);
    if snapshot.repo.operation_in_progress() {
        let unmerged = unmerged_files(&snapshot.repo.root)?;
        session.record(edited.iter().chain(&unmerged))?;
    } else {
        let changed = snapshot.status.touched.iter().chain(&snapshot.untracked);
        session.record(edited.iter().chain(changed))?;
    }
    Ok(session)
}

fn note_without_session(error: &anyhow::Error) {
    note(&format!(
        "note: hook session state unavailable, the hook runs without a session: {error:#}\n"
    ));
}

/// Notes the unverified files of a run that does not block on stderr.
fn note_unverified(result: &ScanResult) {
    note(&report::unverified_notes(result));
}

/// Writes `text` to stderr. A failed write is ignored, so it never changes
/// the hook's result.
fn note(text: &str) {
    let _ = io::stderr().write_all(text.as_bytes());
}

fn emit_feedback(harness: Harness, reason: &str, stop: bool) -> Result<u8> {
    match (harness, stop) {
        (Harness::Grok, true) => {
            eprintln!("{reason}");
            Ok(2)
        }
        (Harness::Grok | Harness::Cursor, false) => {
            eprintln!("{reason}");
            Ok(0)
        }
        (Harness::Cursor, true) => {
            println!(
                "{}",
                serde_json::to_string(&json!({"followup_message":reason}))?
            );
            Ok(0)
        }
        (Harness::Claude | Harness::Codex, _) => {
            println!(
                "{}",
                serde_json::to_string(&json!({"decision":"block", "reason":reason}))?
            );
            Ok(0)
        }
    }
}

pub fn state_dir() -> Result<PathBuf> {
    state_dir_from(env::var_os("PICKCHECK_HOME"), dirs::home_dir())
}

fn state_dir_from(
    pickcheck_home: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = pickcheck_home {
        return Ok(PathBuf::from(path));
    }
    let home = home.context("cannot determine home directory")?;
    Ok(home.join(".pickforge").join("pickcheck"))
}

/// The physical working directory, so the config lookup for the loop guard
/// walks the same ancestors as the scan.
fn working_directory(input: &HookInput) -> PathBuf {
    physical(&requested_directory(input))
}

fn requested_directory(input: &HookInput) -> PathBuf {
    input
        .cwd
        .clone()
        .or_else(|| input.workspace_root.clone())
        .or_else(|| input.workspace_roots.first().cloned())
        .or_else(|| env::var_os("CURSOR_PROJECT_DIR").map(PathBuf::from))
        .or_else(|| env::var_os("GROK_WORKSPACE_ROOT").map(PathBuf::from))
        .or_else(|| env::var_os("CLAUDE_PROJECT_DIR").map(PathBuf::from))
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn state_file(session_id: &str) -> Result<PathBuf> {
    Ok(state_dir()?.join(format!("{}.count", sanitized_session_id(session_id))))
}

pub(crate) fn sanitized_session_id(session_id: &str) -> String {
    let sanitized: String = session_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(128)
        .collect();
    if sanitized.is_empty() {
        "unkeyed".to_owned()
    } else {
        sanitized
    }
}

fn increment_counter(session_id: &str) -> Result<usize> {
    let path = state_file(session_id)?;
    let current = read_counter(&path).saturating_add(1);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, current.to_string())
        .with_context(|| format!("cannot write state {}", path.display()))?;
    Ok(current)
}

fn read_counter(path: &Path) -> usize {
    fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

fn reset_counter(session_id: &str) -> Result<()> {
    let path = state_file(session_id)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(event: &str, tool: Option<&str>, tool_input: Value) -> HookInput {
        HookInput {
            hook_event_name: event.to_owned(),
            session_id: "s".to_owned(),
            cwd: Some(PathBuf::from("/tmp")),
            workspace_root: None,
            workspace_roots: Vec::new(),
            tool_name: tool.map(str::to_owned),
            tool_input,
            file_path: None,
            status: None,
        }
    }

    #[test]
    fn grok_payload_with_both_event_name_keys_parses() {
        let input: HookInput = serde_json::from_str(
            r#"{"hook_event_name":"Stop","hookEventName":"stop","session_id":"s","sessionId":"s","cwd":"/tmp"}"#,
        )
        .expect("both event-name keys");
        assert_eq!(input.hook_event_name, "Stop");
        assert_eq!(input.session_id, "s");
    }

    #[test]
    fn claude_post_edit_uses_file_path() {
        let event = classify(
            Harness::Claude,
            &input("PostToolUse", Some("Edit"), json!({"file_path":"a.rs"})),
        );
        assert!(matches!(event, Event::File(path) if path == Path::new("a.rs")));
    }

    #[test]
    fn codex_apply_patch_without_path_falls_back_to_changed() {
        let event = classify(
            Harness::Codex,
            &input(
                "PostToolUse",
                Some("apply_patch"),
                json!({"command":"patch"}),
            ),
        );
        assert!(matches!(event, Event::Changed));
    }

    #[test]
    fn grok_accepts_native_camel_case_input() {
        let parsed: HookInput = serde_json::from_str(
            r#"{"hookEventName":"post_tool_use","sessionId":"s","workspaceRoot":"/repo","toolName":"search_replace","toolInput":{"path":"a.rs"}}"#,
        )
        .unwrap();
        let event = classify(Harness::Grok, &parsed);
        assert!(matches!(event, Event::File(path) if path == Path::new("a.rs")));
        assert_eq!(working_directory(&parsed), Path::new("/repo"));
    }

    #[test]
    fn cursor_uses_native_file_and_workspace_fields() {
        let parsed: HookInput = serde_json::from_str(
            r#"{"hook_event_name":"afterFileEdit","conversation_id":"s","workspace_roots":["/repo"],"file_path":"a.rs"}"#,
        )
        .unwrap();
        let event = classify(Harness::Cursor, &parsed);
        assert!(matches!(event, Event::File(path) if path == Path::new("a.rs")));
        assert_eq!(working_directory(&parsed), Path::new("/repo"));
    }

    #[test]
    fn cursor_ignores_aborted_stop() {
        let parsed: HookInput =
            serde_json::from_str(r#"{"hook_event_name":"stop","status":"aborted"}"#).unwrap();
        assert!(matches!(classify(Harness::Cursor, &parsed), Event::Ignore));
    }

    #[test]
    fn optional_hook_fields_default_before_classification() {
        let parsed: HookInput = serde_json::from_str(r#"{"hook_event_name":"Other"}"#).unwrap();
        assert!(parsed.session_id.is_empty());
        assert!(parsed.cwd.is_none());
        assert!(matches!(classify(Harness::Claude, &parsed), Event::Ignore));
    }

    #[test]
    fn state_dir_prefers_pickcheck_home_then_platform_home() {
        let home = PathBuf::from("home");
        assert_eq!(
            state_dir_from(Some("override".into()), Some(home.clone())).unwrap(),
            Path::new("override")
        );
        assert_eq!(
            state_dir_from(None, Some(home)).unwrap(),
            Path::new("home").join(".pickforge").join("pickcheck")
        );
        let error = state_dir_from(None, None).unwrap_err();
        assert_eq!(error.to_string(), "cannot determine home directory");
    }

    #[test]
    fn session_ids_are_sanitized_and_bounded() {
        assert_eq!(sanitized_session_id("agent/a:b"), "agent_a_b");
        assert_eq!(sanitized_session_id(""), "unkeyed");
        assert_eq!(sanitized_session_id(&"x".repeat(200)).len(), 128);
    }
}
