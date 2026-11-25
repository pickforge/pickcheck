//! Session-anchored hooks: the acceptance criteria of issue #53.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Child, Command, Output, Stdio},
};

use serde_json::{Value, json};
use tempfile::TempDir;

const ALL_STATUSES: &str =
    r#"{"hook":{"fail_on":["new","worsened","unmatched","improved","unchanged"]}}"#;

/// A violation is the first criterion; every other criterion needs one too.
#[test]
fn a_committed_violation_still_blocks_stop() {
    let (dir, state) = repo(&[("a.js", &simple("a")), ("b.js", &simple("b"))]);
    let root = dir.path();
    let initial = head(root);

    fs::write(root.join("a.js"), violating("a", 16)).unwrap();
    edit(&state, root, "one", "a.js");
    commit_all(root, "one commit");
    let reason = block_reason(&stop(&state, root, "one"));
    assert!(reason.contains("FAIL a.js  1 function"), "{reason}");
    assert!(reason.contains(&format!("--base {initial} ")), "{reason}");

    // Several commits, including one that leaves the file unchanged now.
    start(&state, root, "several");
    fs::write(root.join("b.js"), violating("b", 16)).unwrap();
    commit_all(root, "first");
    fs::write(root.join("a.js"), violating("a", 17)).unwrap();
    edit(&state, root, "several", "a.js");
    commit_all(root, "second");
    fs::write(root.join("b.js"), violating("b", 18)).unwrap();
    stop(&state, root, "several");
    commit_all(root, "third");
    let reason = block_reason(&stop(&state, root, "several"));
    assert!(reason.starts_with("FAIL 2 changed files"), "{reason}");
}

#[test]
fn commits_merged_or_rebased_in_block_only_through_session_files() {
    let (dir, state) = repo(&[("a.js", &spaced("a")), ("other.js", &simple("other"))]);
    let root = dir.path();
    let initial = head(root);
    git(root, &["checkout", "-qb", "upstream"]);
    fs::write(root.join("other.js"), violating("other", 16)).unwrap();
    commit_all(root, "upstream touches another file");
    git(root, &["checkout", "-q", "main"]);

    fs::write(
        root.join("a.js"),
        spaced("a").replace("return 1", "return 3"),
    )
    .unwrap();
    edit(&state, root, "merge", "a.js");
    commit_all(root, "session");
    git(root, &["merge", "-q", "--no-edit", "upstream"]);
    let merged = stop(&state, root, "merge");
    assert!(merged.stdout.is_empty(), "{}", text(&merged.stdout));

    git(root, &["checkout", "-qb", "upstream-2", "upstream"]);
    let pulled = format!("{}{}", spaced("a"), violating("pulled", 16));
    fs::write(root.join("a.js"), pulled).unwrap();
    commit_all(root, "upstream touches the session file");
    git(root, &["checkout", "-q", "main"]);
    git(root, &["reset", "-q", "--hard", &initial]);
    start(&state, root, "rebase");
    fs::write(
        root.join("a.js"),
        spaced("a").replace("return 1", "return 3"),
    )
    .unwrap();
    edit(&state, root, "rebase", "a.js");
    commit_all(root, "session");
    git(root, &["rebase", "-q", "upstream-2"]);
    let reason = block_reason(&stop(&state, root, "rebase"));
    assert!(reason.starts_with("FAIL 1 changed file,"), "{reason}");
    assert!(reason.contains("FAIL a.js  1 function"), "{reason}");
    assert!(!reason.contains("other.js"), "{reason}");
}

/// The config is created, edited, and deleted in the session; the limits stay
/// those of the base.
#[test]
fn loosening_the_config_in_the_session_keeps_the_base_limits() {
    let config = r#"{"limits":{"depth":1},"hook":{"max_blocks":10}}"#;
    let (dir, state) = repo(&[
        (".pickcheck.json", config),
        ("a.js", &simple("a")),
        ("pkg/b.js", &simple("b")),
    ]);
    let root = dir.path();
    let initial = head(root);
    let note =
        format!("note: .pickcheck.json changed in this diff; the config comes from {initial}\n");

    fs::write(root.join("a.js"), nested("a")).unwrap();
    edit(&state, root, "config", "a.js");
    let loose = r#"{"limits":{"depth":null},"hook":{"fail_on":["improved"],"max_blocks":1}}"#;
    fs::write(root.join(".pickcheck.json"), loose).unwrap();
    edit(&state, root, "config", ".pickcheck.json");
    let reason = block_reason(&stop(&state, root, "config"));
    assert!(reason.starts_with(&note), "{reason}");
    assert!(reason.contains("FAIL a.js  1 function"), "{reason}");

    commit_all(root, "loosen");
    fs::create_dir_all(root.join("pkg")).unwrap();
    fs::write(
        root.join("pkg/.pickcheck.json"),
        r#"{"limits":{"depth":null}}"#,
    )
    .unwrap();
    fs::write(root.join("pkg/b.js"), nested("b")).unwrap();
    edit(&state, root, "config", "pkg/b.js");
    let reason = block_reason(&stop(&state, root, "config"));
    assert!(reason.starts_with(&note), "{reason}");
    assert!(
        reason.contains(&native("FAIL pkg/b.js  1 function")),
        "{reason}"
    );

    fs::remove_file(root.join(".pickcheck.json")).unwrap();
    commit_all(root, "delete the config");
    let reason = block_reason(&stop(&state, root, "config"));
    assert!(reason.starts_with(&note), "{reason}");
    assert!(
        reason.starts_with(&format!("{note}FAIL 2 changed files")),
        "{reason}"
    );

    // `check` keeps reading the working tree.
    let check = command(root, &["check", "--base", &initial]);
    assert_eq!(check.status.code(), Some(0), "{}", text(&check.stdout));
}

#[test]
fn legacy_violations_block_only_with_their_status_in_fail_on() {
    for (config, blocks) in [("{}", false), (ALL_STATUSES, true)] {
        let legacy = format!("{}{}", violating("old", 18), violating("kept", 16));
        let (dir, state) = repo(&[(".pickcheck.json", config), ("legacy.js", &legacy)]);
        let root = dir.path();
        let unchanged = legacy.replacen("x === 1) x++", "x === 1) x += 1", 2);
        let improved = unchanged.replacen("if (x === 0) x++;\n", "", 1);
        assert!(improved.contains("function kept") && improved != unchanged);

        fs::write(root.join("legacy.js"), &improved).unwrap();
        let output = edit(&state, root, "legacy", "legacy.js");
        assert_eq!(!output.stdout.is_empty(), blocks, "{config}");
        commit_all(root, "edit legacy functions");
        let output = stop(&state, root, "legacy");
        assert_eq!(!output.stdout.is_empty(), blocks, "{config}");
        if blocks {
            let reason = block_reason(&output);
            assert!(reason.contains("FAIL legacy.js  2 functions"), "{reason}");
            continue;
        }

        // Something else blocks: the legacy violations are listed as `WARN`.
        fs::write(root.join("new.js"), violating("fresh", 16)).unwrap();
        let reason = block_reason(&stop(&state, root, "legacy"));
        assert!(reason.starts_with("FAIL 1 changed file,"), "{reason}");
        assert!(
            reason.contains("WARN 1 changed file, 2 functions"),
            "{reason}"
        );
        assert!(reason.contains("FAIL new.js  1 function"), "{reason}");
        assert!(reason.contains("WARN legacy.js  2 functions"), "{reason}");
        assert!(
            reason.contains("--fail-on new,worsened,unmatched "),
            "{reason}"
        );
    }
}

#[test]
fn post_tool_use_reports_only_functions_the_session_changed() {
    let app = format!("{}{}", violating("old", 16), simple("fresh"));
    let (dir, state) = repo(&[("app.js", &app)]);
    let root = dir.path();

    let touched = format!(
        "{}{}",
        violating("old", 16),
        simple("fresh").replace('1', "2")
    );
    fs::write(root.join("app.js"), touched).unwrap();
    let output = edit(&state, root, "edit", "app.js");
    assert!(output.stdout.is_empty(), "{}", text(&output.stdout));

    let grown = format!("{}{}", violating("old", 16), violating("fresh", 16));
    fs::write(root.join("app.js"), grown).unwrap();
    let reason = block_reason(&edit(&state, root, "edit", "app.js"));
    assert!(
        reason.starts_with("FAIL 1 changed file, 1 function, 1 violation\n"),
        "{reason}"
    );
    assert!(!reason.contains("WARN"), "{reason}");
}

/// Many first events of one session race: one anchor wins, every process
/// uses it, and every recorded file survives.
#[test]
fn concurrent_hook_events_share_one_anchor_and_lose_no_file() {
    const EDITS: usize = 16;
    const PATCHES: usize = 8;
    let (dir, state) = repo(&[(".gitignore", "gen/\n"), ("shared.js", &simple("shared"))]);
    let root = dir.path();
    let initial = head(root);
    fs::create_dir(root.join("gen")).unwrap();
    for index in 0..EDITS {
        let path = root.join("gen").join(format!("f{index}.js"));
        fs::write(path, violating(&format!("f{index}"), 16)).unwrap();
    }
    fs::write(root.join("shared.js"), violating("shared", 16)).unwrap();

    // Ignored files reach the log only through their own edit event.
    let mut inputs = (0..EDITS)
        .map(|index| {
            let file = root.join("gen").join(format!("f{index}.js"));
            ("claude", edit_input(root, "race", &file))
        })
        .collect::<Vec<_>>();
    let patch = json!({"hook_event_name":"PostToolUse", "session_id":"race", "cwd":root,
        "tool_name":"apply_patch", "tool_input":{"command":"patch"}});
    inputs.extend((0..PATCHES).map(|_| ("codex", patch.clone())));
    let children = inputs
        .iter()
        .map(|(harness, _)| spawn_hook(state.path(), harness))
        .collect::<Vec<_>>();
    let outputs = children
        .into_iter()
        .zip(&inputs)
        .map(|(mut child, (_, input))| {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(input.to_string().as_bytes()).unwrap();
            child
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect::<Vec<_>>();

    for output in &outputs {
        assert!(output.status.success(), "{}", text(&output.stderr));
    }
    for output in &outputs[EDITS..] {
        let reason = block_reason(output);
        assert!(!reason.contains("note:"), "{reason}");
        assert!(reason.contains(&format!("--base {initial} ")), "{reason}");
    }
    let state_files = fs::read_dir(state.path().join("sessions"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    // No temporary anchor file remains.
    assert_eq!(state_files.len(), 2, "{state_files:?}");
    let anchors = state_files
        .into_iter()
        .filter(|path| path.extension().is_some_and(|kind| kind == "anchor"))
        .collect::<Vec<_>>();
    assert_eq!(anchors.len(), 1, "{anchors:?}");
    assert_eq!(
        fs::read_to_string(&anchors[0]).unwrap(),
        format!("{initial}\n")
    );

    git(root, &["add", "-f", "gen"]);
    commit_all(root, "generated");
    let reason = block_reason(&stop(&state, root, "race"));
    let total = format!("FAIL {} changed files,", EDITS + 1);
    assert!(reason.starts_with(&total), "{reason}");
    for index in 0..EDITS {
        let row = native(&format!("FAIL gen/f{index}.js  1 function"));
        assert!(reason.contains(&row), "{reason}");
    }
}

#[test]
fn hooks_without_a_session_id_or_head_work_as_before() {
    let legacy = format!("{}{}", violating("old", 16), simple("fresh"));
    let (dir, state) = repo(&[("app.js", &legacy)]);
    let root = dir.path();
    let edited = format!(
        "{}{}",
        touched("old", 16),
        simple("fresh").replace('1', "2")
    );
    fs::write(root.join("app.js"), &edited).unwrap();

    // Without a session id an edit checks the whole file and `Stop` the
    // diff against `HEAD`, where every violation blocks.
    let reason = block_reason(&edit(&state, root, "", "app.js"));
    assert!(reason.starts_with("FAIL 1 file, 1 function"), "{reason}");
    assert!(
        reason.contains("DETAILS pickcheck check --verbose <file>"),
        "{reason}"
    );
    let reason = block_reason(&stop(&state, root, ""));
    assert!(
        reason.contains("DETAILS pickcheck check --changed --verbose"),
        "{reason}"
    );
    commit_all(root, "commit");
    assert!(stop(&state, root, "").stdout.is_empty());
    assert!(!state.path().join("sessions").exists());

    // Outside Git and without `HEAD`, as before.
    for with_git in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        if with_git {
            init_repo(root);
        }
        fs::write(root.join("app.js"), &legacy).unwrap();
        let reason = block_reason(&edit(&state, root, "nohead", "app.js"));
        assert!(reason.starts_with("FAIL 1 file, 1 function"), "{reason}");
        let output = stop(&state, root, "nohead");
        assert!(output.stdout.is_empty());
        assert!(text(&output.stderr).contains("note: hook skipped"));
    }
    assert!(!state.path().join("sessions").exists());
}

#[test]
fn an_unusable_anchor_falls_back_to_head_with_a_note() {
    let (dir, state) = repo(&[("a.js", &simple("a")), ("old.js", &violating("old", 16))]);
    let root = dir.path();
    git(root, &["checkout", "-qb", "work"]);
    fs::write(root.join("a.js"), simple("a").replace('1', "2")).unwrap();
    commit_all(root, "work");
    let lost = head(root);
    start(&state, root, "lost");
    git(root, &["checkout", "-q", "main"]);
    git(root, &["branch", "-qD", "work"]);
    git(root, &["reflog", "expire", "--expire=now", "--all"]);
    git(root, &["gc", "-q", "--prune=now"]);

    // The fallback keeps the full strictness of the `HEAD` diff.
    fs::write(root.join("old.js"), touched("old", 16)).unwrap();
    let reason = block_reason(&stop(&state, root, "lost"));
    let note = format!(
        "note: session anchor {lost} does not resolve or has no merge base with HEAD; the hook checks the diff against HEAD\n"
    );
    assert!(reason.starts_with(&note), "{reason}");
    assert!(reason.contains("FAIL old.js  1 function"), "{reason}");
    assert!(reason.contains("--changed --verbose"), "{reason}");
    let reason = block_reason(&edit(&state, root, "lost", "old.js"));
    assert!(reason.starts_with(&note), "{reason}");

    // An anchor with no merge base.
    let initial = head(root);
    start(&state, root, "orphan");
    git(root, &["checkout", "-q", "--orphan", "unrelated"]);
    commit_all(root, "unrelated root");
    fs::write(root.join("old.js"), violating("old", 17)).unwrap();
    let reason = block_reason(&stop(&state, root, "orphan"));
    assert!(
        reason.starts_with(&format!("note: session anchor {initial} ")),
        "{reason}"
    );
}

#[test]
fn session_stops_keep_the_loop_guard_and_harness_contracts() {
    let config = r#"{"hook":{"max_blocks":2}}"#;
    let (dir, state) = repo(&[(".pickcheck.json", config), ("a.js", &simple("a"))]);
    let root = dir.path();
    let cursor = json!({"hook_event_name":"beforeSubmitPrompt", "conversation_id":"cursor",
        "workspace_roots":[root]});
    let grok = json!({"hookEventName":"user_prompt_submit", "sessionId":"grok",
        "workspaceRoot":root});
    // Events the hooks do not check still record the files changed now.
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();
    start(&state, root, "guard");
    assert!(hook(&state, "cursor", &cursor).stdout.is_empty());
    assert!(hook(&state, "grok", &grok).stdout.is_empty());
    commit_all(root, "violation");

    for index in 0..4 {
        let output = stop(&state, root, "guard");
        assert!(output.status.success());
        if index < 2 {
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["decision"], "block");
            assert!(output.stderr.is_empty());
        } else {
            assert!(output.stdout.is_empty());
            assert!(text(&output.stderr).starts_with("UNRESOLVED\nFAIL"));
        }
    }
    let count = state.path().join("guard.count");
    assert_eq!(fs::read_to_string(count).unwrap(), "4");

    let cursor_stop = json!({"hook_event_name":"stop", "conversation_id":"cursor",
        "workspace_roots":[root], "status":"completed"});
    let output = hook(&state, "cursor", &cursor_stop);
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let message = value["followup_message"].as_str().unwrap();
    assert!(message.contains("FAIL a.js"), "{message}");
    let grok_stop = json!({"hookEventName":"stop", "sessionId":"grok", "workspaceRoot":root});
    let output = hook(&state, "grok", &grok_stop);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).ends_with("Fix the listed files, then finish.\n"));
}

#[test]
fn a_deleted_session_file_leaves_the_rest_of_the_session_in_scope() {
    let (dir, state) = repo(&[("a.js", &simple("a")), ("b.js", &simple("b"))]);
    let root = dir.path();
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();
    fs::write(root.join("b.js"), violating("b", 16)).unwrap();
    edit(&state, root, "delete", "a.js");
    edit(&state, root, "delete", "b.js");
    commit_all(root, "both");
    fs::remove_file(root.join("a.js")).unwrap();
    commit_all(root, "delete one");
    let reason = block_reason(&stop(&state, root, "delete"));
    assert!(reason.starts_with("FAIL 1 changed file,"), "{reason}");
    assert!(reason.contains("FAIL b.js  1 function"), "{reason}");
}

#[test]
fn a_renamed_session_file_stays_in_scope() {
    let (dir, state) = repo(&[("a.js", &violating("a", 15))]);
    let root = dir.path();
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();
    edit(&state, root, "rename", "a.js");
    commit_all(root, "violation");
    git(root, &["mv", "a.js", "renamed.js"]);
    commit_all(root, "rename");
    let reason = block_reason(&stop(&state, root, "rename"));
    assert!(reason.contains("FAIL renamed.js  1 function"), "{reason}");
}

#[test]
fn one_session_id_keeps_an_anchor_per_repository() {
    let (first, state) = repo(&[("a.js", &simple("a"))]);
    let (second, _) = repo(&[("b.js", &simple("b"))]);
    for (root, file) in [(first.path(), "a.js"), (second.path(), "b.js")] {
        start(&state, root, "shared");
        let name = file.trim_end_matches(".js");
        fs::write(root.join(file), violating(name, 16)).unwrap();
        edit(&state, root, "shared", file);
    }
    for (root, file) in [(first.path(), "a.js"), (second.path(), "b.js")] {
        let initial = git_output(root, &["rev-parse", "HEAD"]);
        commit_all(root, "violation");
        let reason = block_reason(&stop(&state, root, "shared"));
        assert!(!reason.contains("note:"), "{reason}");
        assert!(
            reason.contains(&format!("FAIL {file}  1 function")),
            "{reason}"
        );
        assert!(reason.contains(&format!("--base {initial} ")), "{reason}");
    }
    let anchors = fs::read_dir(state.path().join("sessions")).unwrap().count();
    assert_eq!(anchors, 4);
}

/// A file outside the repository around `cwd` has no base: the edit checks
/// the whole file, exactly as without a session.
#[test]
fn an_edit_outside_the_repository_checks_the_whole_file() {
    let (dir, state) = repo(&[("a.js", &simple("a"))]);
    let root = dir.path();
    let loose = tempfile::tempdir().unwrap();
    let (other, _) = repo(&[("b.js", &simple("b"))]);
    fs::write(loose.path().join("x.js"), violating("x", 16)).unwrap();
    fs::write(other.path().join("b.js"), violating("b", 16)).unwrap();
    for file in [loose.path().join("x.js"), other.path().join("b.js")] {
        let without = hook(&state, "claude", &edit_input(root, "", &file));
        let output = hook(&state, "claude", &edit_input(root, "outside", &file));
        let reason = block_reason(&output);
        assert!(reason.starts_with("FAIL 1 file, 1 function"), "{reason}");
        assert_eq!(output.stdout, without.stdout);
        assert_eq!(output.stderr, without.stderr);
    }
    assert!(state.path().join("sessions").exists());
}

/// Unusable session state is a note on stderr, never a hook error: an event
/// the hook ignores stays silent, and the others work without a session.
#[test]
fn unusable_session_state_is_a_note_and_the_hook_works_without_a_session() {
    let (dir, state) = repo(&[("a.js", &simple("a"))]);
    let root = dir.path();
    fs::write(root.join("a.js"), touched_simple("a")).unwrap();
    fs::write(root.join("b.js"), violating("b", 16)).unwrap();
    let file = state.path().join("file");
    fs::write(&file, "").unwrap();
    assert_without_session(&file, root, "a-file");

    // A corrupt anchor.
    start(&state, root, "corrupt");
    let anchor = session_file(&state, "anchor");
    fs::write(&anchor, "not a commit\n").unwrap();
    assert_without_session(state.path(), root, "corrupt");
    fs::remove_file(&anchor).unwrap();
    fs::remove_file(session_file(&state, "files")).unwrap();

    // A log that cannot be read.
    start(&state, root, "log");
    let log = session_file(&state, "files");
    fs::remove_file(&log).unwrap();
    fs::create_dir(&log).unwrap();
    assert_without_session(state.path(), root, "log");
}

#[cfg(unix)]
#[test]
fn an_unwritable_state_directory_is_a_note() {
    use std::os::unix::fs::PermissionsExt;

    let (dir, state) = repo(&[("a.js", &simple("a"))]);
    let root = dir.path();
    fs::write(root.join("b.js"), violating("b", 16)).unwrap();
    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o555)).unwrap();
    // A superuser writes anyway; there is nothing to test then.
    if fs::write(state.path().join("probe"), "").is_err() {
        assert_without_session(state.path(), root, "readonly");
    }
    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o755)).unwrap();
}

/// Garbage in the log is skipped record by record; the session goes on.
#[test]
fn a_corrupt_log_keeps_its_readable_records() {
    let (dir, state) = repo(&[("a.js", &simple("a")), ("b.js", &simple("b"))]);
    let root = dir.path();
    let initial = head(root);
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();
    edit(&state, root, "garbage", "a.js");
    let log = session_file(&state, "files");
    let mut bytes = fs::read(&log).unwrap();
    bytes.extend_from_slice(b"\xff\xfe{not json\n\"b.js\n\0\n");
    fs::write(&log, bytes).unwrap();
    commit_all(root, "violation");
    let output = stop(&state, root, "garbage");
    assert!(output.stderr.is_empty(), "{}", text(&output.stderr));
    let reason = block_reason(&output);
    assert!(reason.contains("FAIL a.js  1 function"), "{reason}");
    assert!(reason.contains(&format!("--base {initial} ")), "{reason}");
}

/// The Git commands of one event, each at most once. See "Hook sessions"
/// in `docs/spec.md` for the list.
#[test]
fn one_event_runs_each_git_command_once() {
    let (dir, state) = repo(&[("a.js", &simple("a")), ("b.js", &simple("b"))]);
    let root = dir.path();
    start(&state, root, "count");
    fs::write(root.join("b.js"), simple("b").replace('1', "2")).unwrap();
    commit_all(root, "a commit in the session");
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();

    let input = edit_input(root, "count", &root.join("a.js"));
    let (output, commands) = traced(&state, &input);
    block_reason(&output);
    // Locate, names and untracked for the record, merge base, the edited
    // file's names and hunks, and its base content.
    assert_eq!(commands.len(), 8, "{commands:#?}");

    let input = json!({"hook_event_name":"Stop", "session_id":"count", "cwd":root});
    let (output, commands) = traced(&state, &input);
    block_reason(&output);
    // The same, with whole-tree names and hunks from the base.
    assert_eq!(commands.len(), 8, "{commands:#?}");
}

/// Past 100 recorded files that no longer exist, the names diff of an edit
/// covers the whole tree instead of naming each of them.
#[test]
fn many_gone_files_widen_the_names_diff_of_an_edit() {
    let names = (0..101)
        .map(|index| format!("gone{index}.js"))
        .collect::<Vec<_>>();
    let mut files = names
        .iter()
        .map(|name| (name.as_str(), "// gone\n"))
        .collect::<Vec<_>>();
    let app = simple("a");
    files.push(("a.js", &app));
    let (dir, state) = repo(&files);
    let root = dir.path();
    for name in &names {
        fs::remove_file(root.join(name)).unwrap();
    }
    start(&state, root, "gone");
    commit_all(root, "delete");
    let base = git_output(root, &["rev-parse", "HEAD~1"]);
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();
    let (output, commands) = traced(&state, &edit_input(root, "gone", &root.join("a.js")));
    block_reason(&output);
    let names_diff = commands
        .iter()
        .find(|command| command.contains(&format!("--name-status -z {base}")))
        .unwrap();
    assert!(names_diff.ends_with(" --"), "{names_diff}");
    assert!(
        commands
            .iter()
            .any(|command| command.ends_with(&format!("--unified=0 {base} -- ':(literal)a.js'"))),
        "{commands:#?}"
    );
}

/// The Git commands that one hook event runs, from `GIT_TRACE`, after
/// checking that none of them ran twice.
fn traced(state: &TempDir, input: &Value) -> (Output, Vec<String>) {
    let trace = state.path().join("trace");
    let _ = fs::remove_file(&trace);
    let mut child = Command::new(env!("CARGO_BIN_EXE_pickcheck"))
        .args(["hook", "claude"])
        .env("PICKCHECK_HOME", state.path())
        .env("GIT_TRACE", &trace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.to_string().as_bytes()).unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let commands = fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .filter_map(|line| {
            line.split_once("trace: built-in: git ")
                .map(|(_, args)| args.to_owned())
        })
        .collect::<Vec<_>>();
    let unique = commands.iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), commands.len(), "{commands:#?}");
    (output, commands)
}

/// Every hook event with session state at `state` that cannot be used.
fn assert_without_session(state: &Path, root: &Path, session: &str) {
    let note = "note: hook session state unavailable, the hook runs without a session: ";
    let bash = json!({"hook_event_name":"PostToolUse", "session_id":session, "cwd":root,
        "tool_name":"Bash", "tool_input":{"command":"true"}});
    let output = hook_at(state, "claude", &bash);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(output.stdout.is_empty());
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(note) && stderr.lines().count() == 1,
        "{stderr}"
    );

    // An edit checks the whole file.
    let output = hook_at(
        state,
        "claude",
        &edit_input(root, session, &root.join("b.js")),
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let reason = block_reason(&output);
    assert!(reason.starts_with("FAIL 1 file, 1 function"), "{reason}");
    assert!(text(&output.stderr).starts_with(note));

    // `Stop` checks the diff against `HEAD`.
    let stop = json!({"hook_event_name":"Stop", "session_id":session, "cwd":root});
    let output = hook_at(state, "claude", &stop);
    if output.status.code() == Some(0) {
        let reason = block_reason(&output);
        assert!(reason.contains("--changed --verbose"), "{reason}");
    } else {
        // The loop guard keeps its own error when it cannot count the block.
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(text(&output.stderr).contains("\nerror: "));
    }
    assert!(text(&output.stderr).starts_with(note));
}

/// The one state file of `kind` in the sessions directory.
fn session_file(state: &TempDir, kind: &str) -> std::path::PathBuf {
    let mut files = fs::read_dir(state.path().join("sessions"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == kind));
    let file = files.next().unwrap();
    assert!(files.next().is_none());
    file
}

fn touched_simple(name: &str) -> String {
    simple(name).replace('1', "2")
}

fn repo(files: &[(&str, &str)]) -> (TempDir, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let path = dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    init_repo(dir.path());
    commit_all(dir.path(), "initial");
    (dir, tempfile::tempdir().unwrap())
}

fn simple(name: &str) -> String {
    format!("function {name}(x) {{\n  return 1;\n}}\n")
}

/// `simple` with room after it, so a commit that appends to the file does
/// not conflict with an edit of the function.
fn spaced(name: &str) -> String {
    format!("{}// one\n// two\n// three\n// four\n", simple(name))
}

/// A function whose cognitive complexity is `score`.
fn violating(name: &str, score: usize) -> String {
    let decisions = (0..score)
        .map(|index| format!("  if (x === {index}) x++;\n"))
        .collect::<String>();
    format!("function {name}(x) {{\n{decisions}  return x;\n}}\n")
}

/// `violating` with an edit that keeps the score.
fn touched(name: &str, score: usize) -> String {
    violating(name, score).replacen("x++", "x += 1", 1)
}

/// A function at depth 2.
fn nested(name: &str) -> String {
    format!(
        "function {name}(x) {{\n  if (x) {{\n    if (x > 1) {{\n      return 1;\n    }}\n  }}\n  return 0;\n}}\n"
    )
}

fn start(state: &TempDir, root: &Path, session: &str) {
    let input = json!({"hook_event_name":"SessionStart", "session_id":session, "cwd":root});
    let output = hook(state, "claude", &input);
    assert!(output.status.success() && output.stdout.is_empty());
}

fn edit_input(root: &Path, session: &str, file: &Path) -> Value {
    json!({"hook_event_name":"PostToolUse", "session_id":session, "cwd":root,
        "tool_name":"Edit", "tool_input":{"file_path":file}})
}

fn edit(state: &TempDir, root: &Path, session: &str, file: &str) -> Output {
    let output = hook(
        state,
        "claude",
        &edit_input(root, session, &root.join(file)),
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    output
}

fn stop(state: &TempDir, root: &Path, session: &str) -> Output {
    let input = json!({"hook_event_name":"Stop", "session_id":session, "cwd":root});
    let output = hook(state, "claude", &input);
    assert!(output.status.success(), "{}", text(&output.stderr));
    output
}

fn block_reason(output: &Output) -> String {
    let value: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("no block: {}", text(&output.stderr)));
    assert_eq!(value["decision"], "block");
    value["reason"].as_str().unwrap().to_owned()
}

fn hook(state: &TempDir, harness: &str, input: &Value) -> Output {
    hook_at(state.path(), harness, input)
}

fn hook_at(state: &Path, harness: &str, input: &Value) -> Output {
    let mut child = spawn_hook(state, harness);
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.to_string().as_bytes()).unwrap();
    drop(stdin);
    child.wait_with_output().unwrap()
}

fn spawn_hook(state: &Path, harness: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_pickcheck"))
        .args(["hook", harness])
        .env("PICKCHECK_HOME", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn command(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pickcheck"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn head(root: &Path) -> String {
    git_output(root, &["rev-parse", "HEAD"])
}

fn init_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
}

fn commit_all(root: &Path, message: &str) {
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", message]);
}

fn git(root: &Path, args: &[&str]) {
    git_output(root, args);
}

fn git_output(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        text(&output.stderr)
    );
    text(&output.stdout).trim().to_owned()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A `/`-separated relative path as the platform reports it.
fn native(path: &str) -> String {
    path.replace('/', std::path::MAIN_SEPARATOR_STR)
}
