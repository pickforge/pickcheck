use std::{
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use pickcheck_core::{
    ChangedFiles, Config, ScanOptions, ScanResult, UnusableBase, changed_files,
    changed_files_since, config_changed, head_commit, load_config, load_config_at, physical,
    resolve_path, scan,
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
        Event::Ignore => record_only(input, &cwd),
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
        /// Set when a `.pickcheck.json` differs from `base`: the repository
        /// config is then read from `base`.
        config_commit: Option<String>,
    },
}

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

    /// Outside a session an edit checks the whole edited file.
    fn for_edit(self) -> Self {
        match self.mode {
            Mode::Head(_) => Self {
                mode: Mode::Paths,
                notes: self.notes,
            },
            _ => self,
        }
    }

    fn scan(&self, cwd: &Path, paths: &[PathBuf]) -> Result<ScanResult> {
        let (changed, config_commit) = match &self.mode {
            Mode::Paths => (None, None),
            Mode::Head(changes) => (Some(changes), None),
            Mode::Session {
                changes,
                config_commit,
                ..
            } => (Some(changes), config_commit.as_deref()),
        };
        scan(&ScanOptions {
            cwd,
            paths,
            explicit_config: None,
            changed,
            config_commit,
        })
    }

    fn config(&self, cwd: &Path) -> Result<Config> {
        let resolution = match &self.mode {
            Mode::Session {
                changes,
                config_commit: Some(commit),
                ..
            } => load_config_at(cwd, &changes.repo_root, commit)?,
            _ => load_config(cwd, None)?,
        };
        Ok(resolution.config)
    }

    /// The feedback for `result`, or `None` when no violation blocks.
    fn feedback(&self, cwd: &Path, result: &ScanResult) -> Result<Option<String>> {
        if result.violations.is_empty() {
            return Ok(None);
        }
        let fail_on = match self.mode {
            Mode::Session { .. } => self.config(cwd)?.hook.fail_on,
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
        let notes = self.notes.iter().chain(scan_notes);
        let notes = notes.map(|note| format!("note: {note}\n"));
        Ok(Some(
            notes.collect::<String>() + &report::summary(result, scope),
        ))
    }
}

fn record_only(input: &HookInput, cwd: &Path) -> Result<u8> {
    if !input.session_id.is_empty() {
        let head = changed_files(cwd)?;
        if !head.fallback {
            open_session(input, cwd, &head, None)?;
        }
    }
    Ok(0)
}

fn report_post_edit(harness: Harness, input: &HookInput, cwd: &Path, path: PathBuf) -> Result<u8> {
    let gate = if input.session_id.is_empty() {
        None
    } else {
        open_gate(input, cwd, Some(&path))?
    };
    let gate = gate.map_or_else(|| Gate::new(Mode::Paths), Gate::for_edit);
    let result = gate.scan(cwd, &[path])?;
    match gate.feedback(cwd, &result)? {
        Some(report) => emit_feedback(harness, &report, false),
        None => Ok(0),
    }
}

/// `Stop`, and an edit event that names no file.
fn report_changed(harness: Harness, input: &HookInput, cwd: &Path, stop: bool) -> Result<u8> {
    let Some(gate) = open_gate(input, cwd, None)? else {
        eprintln!(
            "note: hook skipped: {} is not inside a Git repository with HEAD",
            cwd.display()
        );
        return pass(input, stop);
    };
    let result = gate.scan(cwd, &[])?;
    let Some(report) = gate.feedback(cwd, &result)? else {
        return pass(input, stop);
    };
    if !stop {
        return emit_feedback(harness, &report, false);
    }
    let max = gate.config(cwd)?.hook.max_blocks;
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

/// The gate for this event, after recording it in the session. `None`
/// outside a Git repository with `HEAD`. Without a session id, or when the
/// anchor is unusable, the gate is the `HEAD` diff.
fn open_gate(input: &HookInput, cwd: &Path, edited: Option<&Path>) -> Result<Option<Gate>> {
    let head = changed_files(cwd)?;
    if head.fallback {
        return Ok(None);
    }
    if input.session_id.is_empty() {
        return Ok(Some(Gate::new(Mode::Head(head))));
    }
    let session = open_session(input, cwd, &head, edited)?;
    match changed_files_since(cwd, session.anchor()) {
        Ok(changes) => Ok(Some(session_gate(changes, &session))),
        Err(error) if error.is::<UnusableBase>() => Ok(Some(Gate {
            notes: vec![format!(
                "session anchor {} does not resolve or has no merge base with HEAD; the hook checks the diff against HEAD",
                session.anchor()
            )],
            ..Gate::new(Mode::Head(head))
        })),
        Err(error) => Err(error),
    }
}

/// Opens the session and records the edited file and every file that is
/// changed or untracked now.
fn open_session(
    input: &HookInput,
    cwd: &Path,
    head: &ChangedFiles,
    edited: Option<&Path>,
) -> Result<Session> {
    let commit = head_commit(&head.repo_root)?.context("cannot resolve HEAD")?;
    let mut session = Session::open(
        &state_dir()?,
        &sanitized_session_id(&input.session_id),
        &head.repo_root,
        &commit,
    )?;
    let edited = edited.and_then(|path| {
        let path = resolve_path(cwd, path);
        path.strip_prefix(&head.repo_root)
            .ok()
            .map(Path::to_path_buf)
    });
    session.record(edited.iter().chain(&head.touched).chain(&head.untracked))?;
    Ok(session)
}

/// Keeps the files the session recorded, under their current or old name,
/// and reads the config from the base when any `.pickcheck.json` changed.
fn session_gate(mut changes: ChangedFiles, session: &Session) -> Gate {
    let base = changes.base.clone().unwrap_or_default();
    let config_commit = config_changed(&changes).then(|| base.clone());
    let renames = &changes.renames;
    let kept = |path: &PathBuf| {
        session.contains(path) || renames.get(path).is_some_and(|old| session.contains(old))
    };
    changes.spans.retain(|path, _| kept(path));
    changes.untracked.retain(kept);
    Gate::new(Mode::Session {
        changes,
        base,
        config_commit,
    })
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

fn sanitized_session_id(session_id: &str) -> String {
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
