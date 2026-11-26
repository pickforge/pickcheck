//! Session-anchored hooks: the acceptance criteria of issue #53.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Child, Command, Output, Stdio},
};

use serde_json::{Value, json};
use tempfile::TempDir;

/// A named change that a table test makes in a repository.
type Case = (&'static str, fn(&Path));

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

/// While a merge stops on a conflict, an event records only the file it
/// edits and the unmerged files, not the files that the merge stages.
#[test]
fn an_unfinished_merge_records_only_edited_and_unmerged_files() {
    let (dir, state) = repo(&[("c.js", &simple("c")), ("a.js", &simple("a"))]);
    let root = dir.path();
    git(root, &["checkout", "-qb", "upstream"]);
    fs::write(root.join("up.js"), violating("up", 16)).unwrap();
    fs::write(root.join("c.js"), simple("upstream")).unwrap();
    commit_all(root, "upstream");
    git(root, &["checkout", "-q", "main"]);
    fs::write(root.join("c.js"), simple("main")).unwrap();
    commit_all(root, "main");
    start(&state, root, "merge");
    let merge = Command::new("git")
        .current_dir(root)
        .args(["merge", "-q", "upstream"])
        .output()
        .unwrap();
    assert!(!merge.status.success(), "the merge must stop on c.js");

    let bash = json!({"hook_event_name":"PostToolUse", "session_id":"merge", "cwd":root,
        "tool_name":"Bash", "tool_input":{"command":"git merge upstream"}});
    assert!(hook(&state, "claude", &bash).status.success());
    fs::write(root.join("a.js"), touched_simple("a")).unwrap();
    edit(&state, root, "merge", "a.js");
    let log = fs::read_to_string(session_file(&state, "files")).unwrap();
    assert_eq!(log, "\n\"c.js\"\n\n\"a.js\"\n");

    fs::write(root.join("c.js"), violating("resolved", 16)).unwrap();
    git(root, &["add", "c.js", "a.js"]);
    git(root, &["commit", "-q", "--no-edit"]);
    let reason = block_reason(&stop(&state, root, "merge"));
    assert!(reason.starts_with("FAIL 1 changed file,"), "{reason}");
    assert!(reason.contains("FAIL c.js  1 function"), "{reason}");
    assert!(!reason.contains("up.js"), "{reason}");
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

/// The config that the working-tree lookup reads is compared with the base
/// at the same path, so a config in no diff still makes hooks read the base
/// config: one that Git ignores, one at the directory of the hook's `cwd`,
/// and a skip-worktree entry.
#[test]
fn a_config_in_no_diff_that_differs_from_the_base_keeps_the_base_limits() {
    let note = "note: .pickcheck.json changed in this diff; the config comes from";
    let loose = r#"{"limits":{"cognitive":null},"hook":{"fail_on":["improved"]}}"#;
    let strict = r#"{"hook":{"max_blocks":20}}"#;
    let (dir, state) = repo(&[(".pickcheck.json", strict), ("src/a.js", &simple("a"))]);
    let root = dir.path();
    start(&state, root, "configs");
    fs::write(root.join("src/a.js"), violating("a", 16)).unwrap();
    let blocks = |cwd: &Path| {
        let input = edit_input(cwd, "configs", &root.join("src/a.js"));
        let reason = block_reason(&hook(&state, "claude", &input));
        assert!(reason.starts_with(note), "{reason}");
        let input = json!({"hook_event_name":"Stop", "session_id":"configs", "cwd":cwd});
        let reason = block_reason(&hook(&state, "claude", &input));
        assert!(reason.starts_with(note), "{reason}");
        assert!(reason.contains(&native("src/a.js  1 function")), "{reason}");
    };

    // Ignored, through `.gitignore` and through the exclude file.
    fs::write(root.join(".gitignore"), "src/.pickcheck.json\n").unwrap();
    fs::write(root.join("src/.pickcheck.json"), loose).unwrap();
    blocks(root);
    fs::remove_file(root.join(".gitignore")).unwrap();
    fs::write(root.join(".git/info/exclude"), "src/.pickcheck.json\n").unwrap();
    blocks(root);
    fs::remove_file(root.join("src/.pickcheck.json")).unwrap();

    // Only at the directory of `cwd`, where the hook reads `hook.fail_on`.
    fs::create_dir(root.join("tools")).unwrap();
    fs::write(root.join("tools/.pickcheck.json"), loose).unwrap();
    blocks(&root.join("tools"));
    fs::remove_file(root.join("tools/.pickcheck.json")).unwrap();

    // A skip-worktree entry.
    git(
        root,
        &["update-index", "--skip-worktree", ".pickcheck.json"],
    );
    fs::write(root.join(".pickcheck.json"), loose).unwrap();
    blocks(root);

    // The same config as the base: no note.
    fs::write(root.join(".pickcheck.json"), strict).unwrap();
    let reason = block_reason(&edit(&state, root, "configs", "src/a.js"));
    assert!(reason.starts_with("FAIL 1 changed file"), "{reason}");
}

const WORKING_TREE_NOTE: &str =
    "note: .pickcheck.json changed in this diff; the working-tree config also applies\n";

/// When a config differs from the base, the gate also scans with the
/// working-tree configs, as the hook without a session id does, so a config
/// that the base lookup misses still applies: after a directory rename, and
/// in a nested repository.
#[test]
fn a_stricter_working_tree_config_still_blocks() {
    let strict = r#"{"limits":{"params":1}}"#;
    let (dir, state) = repo(&[
        ("src/.pickcheck.json", strict),
        ("src/a.js", &params("a", 1)),
    ]);
    let root = dir.path();
    start(&state, root, "rename");
    git(root, &["mv", "src", "lib"]);
    fs::write(root.join("lib/a.js"), params("a", 2)).unwrap();
    fs::write(root.join("lib/new.js"), params("n", 2)).unwrap();
    for file in ["lib/a.js", "lib/new.js"] {
        let reason = block_reason(&edit(&state, root, "rename", file));
        assert!(reason.starts_with(WORKING_TREE_NOTE), "{reason}");
        assert!(
            reason.contains(&native(&format!("FAIL {file}  "))),
            "{reason}"
        );
    }
    let reason = block_reason(&stop(&state, root, "rename"));
    assert!(
        reason.starts_with(&format!("{WORKING_TREE_NOTE}FAIL 2 changed files")),
        "{reason}"
    );

    let (dir, state) = repo(&[("a.js", &simple("a"))]);
    let root = dir.path();
    start(&state, root, "nested");
    fs::create_dir(root.join("nested")).unwrap();
    git(&root.join("nested"), &["init", "-q"]);
    fs::write(root.join("nested/.pickcheck.json"), strict).unwrap();
    fs::write(root.join("nested/b.js"), params("b", 2)).unwrap();
    let reason = block_reason(&edit(&state, root, "nested", "nested/b.js"));
    assert!(reason.starts_with(WORKING_TREE_NOTE), "{reason}");
}

/// Each scan blocks with the `hook.fail_on` of its own config, and the loop
/// guard keeps the `hook.max_blocks` of the base. The second scan reads no
/// base content twice.
#[test]
fn each_config_side_keeps_its_fail_on_and_the_base_keeps_max_blocks() {
    let legacy = params("old", 7);
    let (dir, state) = repo(&[(".pickcheck.json", "{}"), ("legacy.js", &legacy)]);
    let root = dir.path();
    start(&state, root, "sides");
    fs::write(
        root.join("legacy.js"),
        legacy.replace("return p0", "return p1"),
    )
    .unwrap();
    assert!(stop(&state, root, "sides").stdout.is_empty());
    let stricter = r#"{"hook":{"fail_on":["unchanged"],"max_blocks":1}}"#;
    fs::write(root.join(".pickcheck.json"), stricter).unwrap();
    let input = json!({"hook_event_name":"Stop", "session_id":"sides", "cwd":root});
    let (output, _) = traced(&state, &input);
    let reason = block_reason(&output);
    assert!(reason.starts_with(WORKING_TREE_NOTE), "{reason}");
    assert!(reason.contains("--fail-on unchanged "), "{reason}");
    // The base allows 3 blocks; the working tree asks for 1.
    for _ in 0..2 {
        assert!(block_reason(&stop(&state, root, "sides")).starts_with(WORKING_TREE_NOTE));
    }
    let output = stop(&state, root, "sides");
    assert!(output.stdout.is_empty());
    assert!(text(&output.stderr).starts_with("UNRESOLVED\n"));
}

/// The base config of a directory whose name has a newline is still read,
/// so loosening the config keeps the base limits there too.
#[cfg(unix)]
#[test]
fn a_newline_in_a_directory_name_keeps_the_base_config() {
    let strict = r#"{"limits":{"params":6}}"#;
    let (dir, state) = repo(&[(".pickcheck.json", strict), ("a.js", &simple("a"))]);
    let root = dir.path();
    start(&state, root, "newline");
    fs::write(root.join(".pickcheck.json"), r#"{"limits":{"params":20}}"#).unwrap();
    fs::create_dir(root.join("new\nfolder")).unwrap();
    fs::write(root.join("new\nfolder/b.js"), params("b", 7)).unwrap();
    let output = stop(&state, root, "newline");
    assert!(output.stderr.is_empty(), "{}", text(&output.stderr));
    let reason = block_reason(&output);
    let note = "note: .pickcheck.json changed in this diff; the config comes from ";
    assert!(reason.starts_with(note), "{reason}");
    assert!(
        reason.contains("FAIL new\nfolder/b.js  1 function"),
        "{reason}"
    );
}

/// A newline in the repository root path is no reason to skip the hook.
#[cfg(unix)]
#[test]
fn a_newline_in_the_repository_root_keeps_the_hook() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("re\npo");
    fs::create_dir(&root).unwrap();
    init_repo(&root);
    fs::write(root.join("a.js"), simple("a")).unwrap();
    commit_all(&root, "initial");
    let state = tempfile::tempdir().unwrap();
    start(&state, &root, "root");
    fs::write(root.join("a.js"), params("a", 7)).unwrap();
    let reason = block_reason(&edit(&state, &root, "root", "a.js"));
    assert!(reason.starts_with("FAIL 1 changed file"), "{reason}");
    for session in ["", "root"] {
        let reason = block_reason(&stop(&state, &root, session));
        assert!(reason.contains("FAIL a.js  1 function"), "{reason}");
    }
}

/// A config that `core.autocrlf` checks out with CRLF holds the same JSON
/// as its LF base, so it is no config change.
#[test]
fn a_line_ending_conversion_is_no_config_change() {
    let strict = "{\"hook\":{\"max_blocks\":5}}\n";
    let (dir, state) = repo(&[(".pickcheck.json", strict), ("a.js", &simple("a"))]);
    let root = dir.path();
    git(root, &["config", "core.autocrlf", "true"]);
    fs::remove_file(root.join(".pickcheck.json")).unwrap();
    git(root, &["checkout", "--", ".pickcheck.json"]);
    let converted = fs::read_to_string(root.join(".pickcheck.json")).unwrap();
    assert!(converted.ends_with("\r\n"), "{converted:?}");
    start(&state, root, "crlf");
    fs::write(root.join("a.js"), params("a", 7)).unwrap();
    let reason = block_reason(&stop(&state, root, "crlf"));
    assert!(reason.starts_with("FAIL 1 changed file"), "{reason}");
}

/// Git lists an untracked nested repository as its directory. That entry
/// is no checked file, so its config is not compared with the base.
#[test]
fn an_untracked_nested_repository_starts_no_config_lookup() {
    let strict = r#"{"hook":{"max_blocks":5}}"#;
    let (dir, state) = repo(&[(".pickcheck.json", strict), ("a.js", &simple("a"))]);
    let root = dir.path();
    start(&state, root, "nested");
    fs::create_dir(root.join("nested")).unwrap();
    git(&root.join("nested"), &["init", "-q"]);
    let loose = r#"{"limits":{"params":20}}"#;
    fs::write(root.join("nested/.pickcheck.json"), loose).unwrap();
    fs::write(root.join("nested/n.js"), simple("n")).unwrap();
    fs::write(root.join("a.js"), params("a", 7)).unwrap();
    let reason = block_reason(&stop(&state, root, "nested"));
    assert!(reason.starts_with("FAIL 1 changed file"), "{reason}");
}

/// A config link reads through its target, at the base as in the working
/// tree. A base link that leaves the repository is the state note.
#[cfg(unix)]
#[test]
fn config_links_are_compared_through_their_targets() {
    let note = "note: .pickcheck.json changed in this diff; the config comes from";
    let relaxed = r#"{"limits":{"cognitive":null}}"#;
    let (dir, state) = repo(&[("cfg/pc.json", "{}"), ("a.js", &simple("a"))]);
    let root = dir.path();
    std::os::unix::fs::symlink("cfg/pc.json", root.join(".pickcheck.json")).unwrap();
    commit_all(root, "link");
    start(&state, root, "links");
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();

    // An unchanged link: the working tree config, no note.
    let reason = block_reason(&edit(&state, root, "links", "a.js"));
    assert!(reason.starts_with("FAIL 1 changed file"), "{reason}");

    // The target loosens the link, then a plain file replaces the link.
    fs::write(root.join("cfg/pc.json"), relaxed).unwrap();
    let reason = block_reason(&edit(&state, root, "links", "a.js"));
    assert!(reason.starts_with(note), "{reason}");
    fs::remove_file(root.join(".pickcheck.json")).unwrap();
    fs::write(root.join(".pickcheck.json"), relaxed).unwrap();
    let reason = block_reason(&stop(&state, root, "links"));
    assert!(reason.starts_with(note), "{reason}");

    // A base link that leaves the repository cannot be read at the base.
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("pc.json"), "{}").unwrap();
    let (dir, state) = repo(&[("a.js", &simple("a"))]);
    let root = dir.path();
    let target = outside.path().join("pc.json");
    std::os::unix::fs::symlink(target, root.join(".pickcheck.json")).unwrap();
    commit_all(root, "outside link");
    start(&state, root, "outside");
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();
    let output = edit(&state, root, "outside", "a.js");
    let stderr = text(&output.stderr);
    let unavailable = "note: hook session state unavailable, the hook runs without a session: ";
    assert!(stderr.starts_with(unavailable), "{stderr}");
    assert!(stderr.contains("cannot read config"), "{stderr}");
    assert!(block_reason(&output).starts_with("FAIL 1 file, 1 function"));
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

    // A fallback that does not block writes the note to stderr.
    let (dir, state) = repo(&[("a.js", &simple("a"))]);
    let root = dir.path();
    let initial = head(root);
    start(&state, root, "quiet");
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();
    commit_all(root, "violation in the session");
    git(root, &["checkout", "-q", "--orphan", "fresh"]);
    commit_all(root, "unrelated root");
    let output = stop(&state, root, "quiet");
    assert!(output.stdout.is_empty(), "{}", text(&output.stdout));
    assert_eq!(
        text(&output.stderr),
        format!(
            "note: session anchor {initial} does not resolve or has no merge base with HEAD; the hook checks the diff against HEAD\n"
        )
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

/// When the hook without a session id fails too, the hook returns that
/// error without the state note. An event that only records keeps the note.
#[test]
fn a_failing_plain_gate_returns_its_error_without_the_state_note() {
    let (dir, state) = repo(&[("a.js", &simple("a"))]);
    let root = dir.path();
    fs::write(root.join(".pickcheck.json"), "{").unwrap();
    fs::write(root.join("a.js"), params("a", 7)).unwrap();
    let home = state.path().join("file");
    fs::write(&home, "").unwrap();
    let stop = |session: &str| json!({"hook_event_name":"Stop", "session_id":session, "cwd":root});
    let file = root.join("a.js");
    for (with, without) in [
        (edit_input(root, "both", &file), edit_input(root, "", &file)),
        (stop("both"), stop("")),
    ] {
        let output = hook_at(&home, "claude", &with);
        assert_eq!(output.status.code(), Some(2), "{with}");
        let stderr = text(&output.stderr);
        assert!(stderr.starts_with("error: invalid JSON in "), "{stderr}");
        assert_eq!(output.stderr, hook_at(&home, "claude", &without).stderr);
    }
    let bash = json!({"hook_event_name":"PostToolUse", "session_id":"both", "cwd":root,
        "tool_name":"Bash", "tool_input":{"command":"true"}});
    let output = hook_at(&home, "claude", &bash);
    assert_eq!(output.status.code(), Some(0));
    let unavailable = "note: hook session state unavailable, the hook runs without a session: ";
    assert!(text(&output.stderr).starts_with(unavailable));
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

/// Every error of the session gate is the state note and the hook without
/// a session id: a base config that does not load, and a Git command that
/// only the session runs. A log record outside the repository is skipped.
#[test]
fn session_gate_errors_are_a_note_and_the_hook_works_without_a_session() {
    let note = "note: hook session state unavailable, the hook runs without a session: ";
    let broken = r#"{"limits":{"bogus":1}}"#;
    let (dir, state) = repo(&[(".pickcheck.json", broken), ("a.js", &simple("a"))]);
    let root = dir.path();
    start(&state, root, "config");
    fs::write(root.join(".pickcheck.json"), "{}").unwrap();
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();
    let output = edit(&state, root, "config", "a.js");
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(note) && stderr.contains("bogus"),
        "{stderr}"
    );
    assert!(block_reason(&output).starts_with("FAIL 1 file, 1 function"));
    let output = stop(&state, root, "config");
    assert!(text(&output.stderr).starts_with(note));
    assert!(block_reason(&output).contains("--changed --verbose"));

    // The tree of the anchor is gone after a later commit.
    let (dir, state) = repo(&[("a.js", &simple("a"))]);
    let root = dir.path();
    start(&state, root, "tree");
    let tree = git_output(root, &["rev-parse", "HEAD^{tree}"]);
    fs::write(root.join("b.js"), simple("b")).unwrap();
    commit_all(root, "later");
    let object = root.join(".git/objects").join(&tree[..2]).join(&tree[2..]);
    let mut permissions = fs::metadata(&object).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&object, permissions).unwrap();
    fs::remove_file(object).unwrap();
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();
    let output = edit(&state, root, "tree", "a.js");
    assert!(
        text(&output.stderr).starts_with(note),
        "{}",
        text(&output.stderr)
    );
    assert!(block_reason(&output).starts_with("FAIL 1 file, 1 function"));
    let output = stop(&state, root, "tree");
    assert!(text(&output.stderr).starts_with(note));
    assert!(block_reason(&output).contains("--changed --verbose"));

    // A record outside the repository never reaches Git.
    let (dir, state) = repo(&[("a.js", &simple("a"))]);
    let root = dir.path();
    let initial = head(root);
    start(&state, root, "record");
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(session_file(&state, "anchor").with_extension("files"))
        .unwrap();
    log.write_all(b"\n\"../outside.js\"\n\n\"/abs.js\"\n")
        .unwrap();
    fs::write(root.join("b.js"), simple("b")).unwrap();
    commit_all(root, "later");
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();
    let output = edit(&state, root, "record", "a.js");
    assert!(output.stderr.is_empty(), "{}", text(&output.stderr));
    let reason = block_reason(&output);
    assert!(reason.contains(&format!("--base {initial} ")), "{reason}");
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
    // file's names and hunks, the base configs, and its base content.
    assert_eq!(commands.len(), 9, "{commands:#?}");
    assert!(commands[6].starts_with("cat-file --batch"), "{commands:#?}");

    let input = json!({"hook_event_name":"Stop", "session_id":"count", "cwd":root});
    let (output, commands) = traced(&state, &input);
    block_reason(&output);
    // The same, with whole-tree names and hunks from the base.
    assert_eq!(commands.len(), 9, "{commands:#?}");

    // A changed config adds no command: the base configs are already read.
    fs::write(root.join(".pickcheck.json"), "{}").unwrap();
    let (output, commands) = traced(&state, &input);
    assert!(block_reason(&output).starts_with("note: .pickcheck.json changed"));
    assert_eq!(commands.len(), 9, "{commands:#?}");
}

/// Git's binary decision never hides a change: a `-diff` or `binary`
/// attribute and a NUL byte still give the hunks of a source file.
#[test]
fn a_file_that_git_treats_as_binary_still_shows_its_changes() {
    let cases: [Case; 3] = [
        ("attribute", |root| {
            fs::write(root.join(".gitattributes"), "*.js -diff\n").unwrap();
            fs::write(root.join("app.js"), params("a", 7)).unwrap();
        }),
        ("info", |root| {
            fs::write(root.join(".git/info/attributes"), "*.js binary\n").unwrap();
            fs::write(root.join("app.js"), params("a", 7)).unwrap();
        }),
        ("nul", |root| {
            fs::write(root.join("app.js"), format!("// \0\n{}", params("a", 7))).unwrap();
        }),
    ];
    for (session, change) in cases {
        let (dir, state) = repo(&[("app.js", &params("a", 1))]);
        let root = dir.path();
        start(&state, root, session);
        change(root);
        let reason = block_reason(&edit(&state, root, session, "app.js"));
        assert!(
            reason.contains("FAIL app.js  1 function"),
            "{session}: {reason}"
        );
        let reason = block_reason(&stop(&state, root, session));
        assert!(
            reason.contains("FAIL app.js  1 function"),
            "{session}: {reason}"
        );
    }
}

/// Hunks can miss a change: an encoding attribute changes what Git diffs,
/// a body line can look like a file header, and a pure rename has no hunk.
/// Every file that the names diff lists is scanned against its base, so a
/// new function that no hunk touches still blocks.
#[test]
fn a_change_that_the_hunks_miss_still_blocks() {
    // The session's change, and the file that it edits.
    type Missed = (&'static str, fn(&Path), &'static str);
    let mut cases: Vec<Missed> = vec![
        (
            "encoding",
            |root| {
                let mut grown = format!("{}{}", simple("a"), params("bad", 7));
                if grown.len() % 2 == 1 {
                    grown.push('\n');
                }
                fs::write(root.join("a.js"), grown).unwrap();
                let attribute = "*.js working-tree-encoding=UTF-16LE\n";
                fs::write(root.join(".gitattributes"), attribute).unwrap();
            },
            "a.js",
        ),
        (
            "header",
            |root| {
                let base = fs::read_to_string(root.join("a.js")).unwrap();
                let grown = format!("/*\n++ /dev/null\n*/\n{base}// end\n{}", params("bad", 7));
                fs::write(root.join("a.js"), grown).unwrap();
            },
            "a.js",
        ),
    ];
    #[cfg(unix)]
    cases.extend::<[Missed; 2]>([
        (
            "quoted",
            |root| {
                git(root, &["mv", "a.js", "we\"ird.js"]);
                let grown = format!("{}{}", simple("a"), params("bad", 7));
                fs::write(root.join("we\"ird.js"), grown).unwrap();
            },
            "we\"ird.js",
        ),
        (
            "link",
            |root| {
                fs::write(root.join("target.txt"), params("bad", 7)).unwrap();
                std::os::unix::fs::symlink("target.txt", root.join("old.txt")).unwrap();
                commit_all(root, "a link");
                git(root, &["mv", "old.txt", "link.js"]);
            },
            "link.js",
        ),
    ]);
    for (session, change, file) in cases {
        let base = format!("{}{}", simple("a"), "\n".repeat(7));
        let (dir, state) = repo(&[("a.js", &base)]);
        let root = dir.path();
        start(&state, root, session);
        change(root);
        let listed = format!("FAIL {file}  1 function");
        let reason = block_reason(&edit(&state, root, session, file));
        assert!(reason.contains(&listed), "{session}: {reason}");
        let reason = block_reason(&stop(&state, root, session));
        assert!(reason.contains(&listed), "{session}: {reason}");
    }
}

/// A clean filter can make Git report no change at all. The edited file is
/// plainly tracked and in no diff, so it is scanned against its base, and a
/// new violation blocks the edit. This reads its base content too.
#[test]
fn an_edited_file_that_git_reports_unchanged_is_compared_with_its_base() {
    let (dir, state) = repo(&[("a.js", &simple("a"))]);
    let root = dir.path();
    start(&state, root, "filter");
    // The filter prints the committed content, so Git sees no change.
    fs::write(root.join(".git/clean.js"), simple("a")).unwrap();
    git(root, &["config", "filter.z.clean", "cat .git/clean.js"]);
    fs::write(root.join(".gitattributes"), "a.js filter=z\n").unwrap();
    let grown = format!("{}{}", simple("a"), params("bad", 7));
    fs::write(root.join("a.js"), grown).unwrap();
    let input = edit_input(root, "filter", &root.join("a.js"));
    let (output, commands) = traced(&state, &input);
    let reason = block_reason(&output);
    assert!(reason.contains("FAIL a.js  1 function"), "{reason}");
    // Locate, names and untracked for the record, the hunks, the index
    // entry, the base configs, and the base content.
    assert_eq!(commands.len(), 8, "{commands:#?}");
    assert!(commands[4].starts_with("ls-files --stage"), "{commands:#?}");
}

/// Old debt that the session leaves as it was never blocks and is never
/// listed, also when the session changes another function of the file, adds
/// a function of the same name, renames the file, or changes its mode.
#[test]
fn old_debt_that_the_session_leaves_as_it_was_is_never_listed() {
    let old = params("old", 7);
    let mut cases: Vec<Case> = vec![
        ("other", |root| {
            let content = fs::read_to_string(root.join("a.js")).unwrap();
            fs::write(root.join("a.js"), content.replace("return 1", "return 2")).unwrap();
        }),
        ("same name", |root| {
            let content = fs::read_to_string(root.join("a.js")).unwrap();
            fs::write(root.join("a.js"), format!("{content}{}", params("old", 1))).unwrap();
        }),
        ("rename", |root| git(root, &["mv", "a.js", "b.js"])),
    ];
    #[cfg(unix)]
    cases.push(("mode", |root| {
        use std::os::unix::fs::PermissionsExt;
        let file = root.join("a.js");
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
    }));
    for (session, change) in cases {
        let base = format!("{old}{}{}", params("old", 1), simple("a"));
        let (dir, state) = repo(&[("a.js", &base)]);
        let root = dir.path();
        start(&state, root, session);
        change(root);
        let file = if session == "rename" { "b.js" } else { "a.js" };
        let output = edit(&state, root, session, file);
        assert!(
            output.stdout.is_empty(),
            "{session}: {}",
            text(&output.stdout)
        );
        assert!(stop(&state, root, session).stdout.is_empty(), "{session}");
        // Something else blocks: the old function is not listed as `WARN`.
        fs::write(root.join("new.js"), params("fresh", 7)).unwrap();
        let reason = block_reason(&stop(&state, root, session));
        assert!(reason.contains("FAIL new.js  1 function"), "{reason}");
        assert!(!reason.contains("WARN"), "{session}: {reason}");
    }
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
            .any(|command| command
                .ends_with(&format!("--unified=0 --text {base} -- ':(literal)a.js'"))),
        "{commands:#?}"
    );
}

/// Every stderr note of a session, the unverified notes and the state
/// failure notes, is a failed write when stderr cannot be written, and the
/// exit code and stdout stay those of a writable stderr.
#[cfg(unix)]
#[test]
fn session_notes_ignore_an_unwritable_stderr() {
    let (dir, state) = repo(&[("a.js", &simple("a"))]);
    let root = dir.path();
    fs::write(root.join("note.kt"), "fun f() = 1\n").unwrap();
    fs::write(root.join("b.js"), violating("b", 16)).unwrap();
    let broken = state.path().join("file");
    fs::write(&broken, "").unwrap();
    let bash = json!({"hook_event_name":"PostToolUse", "session_id":"full", "cwd":root,
        "tool_name":"Bash", "tool_input":{"command":"true"}});
    let stop = json!({"hook_event_name":"Stop", "session_id":"full", "cwd":root});
    let note = "note: UNVERIFIED note.kt  no grammar for .kt\n";
    let unavailable = "note: hook session state unavailable, the hook runs without a session: ";
    let inputs = [
        bash,
        edit_input(root, "full", &root.join("note.kt")),
        edit_input(root, "full", &root.join("b.js")),
        stop,
    ];
    // In a session only the unverified edit writes a note; without usable
    // state every event starts with the state note. A blocking `Stop` then
    // fails on the loop guard's own state, which is not a session note.
    let cases = [
        (state.path(), &["", note, "", ""][..]),
        (broken.as_path(), &[unavailable; 3][..]),
    ];
    for (home, notes) in cases {
        for (input, expected) in inputs.iter().zip(notes) {
            let writable = hook_at(home, "claude", input);
            let stderr = text(&writable.stderr);
            if expected.is_empty() {
                assert!(stderr.is_empty(), "{input}: {stderr}");
            } else {
                assert!(stderr.starts_with(expected), "{input}: {stderr}");
            }
            let unwritable = hook_without_stderr(home, input);
            assert_eq!(unwritable.status.code(), writable.status.code(), "{input}");
            assert_eq!(unwritable.stdout, writable.stdout, "{input}");
        }
    }
}

/// A session started from a linked working directory and continued from
/// the physical one keeps one anchor.
#[cfg(unix)]
#[test]
fn a_linked_and_a_physical_cwd_share_the_session() {
    let (dir, state) = repo(&[("a.js", &simple("a"))]);
    let root = dir.path();
    let initial = head(root);
    let links = tempfile::tempdir().unwrap();
    let link = links.path().join("link");
    std::os::unix::fs::symlink(root, &link).unwrap();
    start(&state, &link, "linked");
    fs::write(root.join("a.js"), violating("a", 16)).unwrap();
    edit(&state, &link, "linked", "a.js");
    commit_all(root, "violation");
    let reason = block_reason(&stop(&state, root, "linked"));
    assert!(!reason.contains("note:"), "{reason}");
    assert!(reason.contains("FAIL a.js  1 function"), "{reason}");
    assert!(reason.contains(&format!("--base {initial} ")), "{reason}");
    let anchors = fs::read_dir(state.path().join("sessions"))
        .unwrap()
        .filter(|entry| {
            let path = entry.as_ref().unwrap().path();
            path.extension().is_some_and(|kind| kind == "anchor")
        })
        .count();
    assert_eq!(anchors, 1);
}

/// An edit of a file that Git ignores is in no diff: it is checked as a
/// whole file with no base, so its violations are new. An ignored file that
/// the session did not edit is never reported.
#[test]
fn an_edited_ignored_file_is_checked_whole_with_no_base() {
    let (dir, state) = repo(&[(".gitignore", "gen/\n"), ("a.js", &simple("a"))]);
    let root = dir.path();
    fs::create_dir(root.join("gen")).unwrap();
    fs::write(root.join("gen").join("old.js"), violating("old", 16)).unwrap();
    fs::write(root.join("gen").join("new.js"), simple("fresh")).unwrap();
    start(&state, root, "ignored");

    // Not edited: neither an edit of another file nor `Stop` reports it.
    fs::write(root.join("a.js"), touched_simple("a")).unwrap();
    assert!(edit(&state, root, "ignored", "a.js").stdout.is_empty());
    assert!(stop(&state, root, "ignored").stdout.is_empty());

    // Edited: the whole file, every violation new.
    let edited = format!("{}{}", simple("fresh"), violating("grown", 16));
    fs::write(root.join("gen").join("new.js"), edited).unwrap();
    let input = edit_input(root, "ignored", &root.join("gen").join("new.js"));
    let (output, commands) = traced(&state, &input);
    // Locate, names and untracked for the record, the hunks, the index
    // entry, and the base configs; a file with no base reads no content.
    assert_eq!(commands.len(), 6, "{commands:#?}");
    assert!(commands[4].starts_with("ls-files --stage"), "{commands:#?}");
    let reason = block_reason(&output);
    assert!(
        reason.starts_with("FAIL 1 changed file, 1 function, 1 violation\n"),
        "{reason}"
    );
    assert!(
        reason.contains(&native("FAIL gen/new.js  1 function")),
        "{reason}"
    );
    assert!(!reason.contains("old.js"), "{reason}");
    let verbose = command(
        root,
        &["check", "--verbose", "--base", "HEAD", "gen/new.js"],
    );
    assert!(
        text(&verbose.stdout).is_empty(),
        "ignored files are in no diff"
    );
}

/// Every edited file in no diff is checked whole with no base unless the
/// index tracks it as a plain file: a file of a nested repository or a
/// submodule, a skip-worktree entry, and a link all block as they do
/// without a session. A plain tracked file in no diff has nothing to check.
#[test]
fn an_edited_file_in_no_diff_is_checked_whole_unless_plainly_tracked() {
    let module = tempfile::tempdir().unwrap();
    init_repo(module.path());
    fs::write(module.path().join("m.js"), simple("m")).unwrap();
    commit_all(module.path(), "module");
    let files = [
        ("ok.js", simple("ok")),
        ("old.js", violating("old", 16)),
        ("real.js", simple("real")),
    ];
    let files = files
        .each_ref()
        .map(|(path, content)| (*path, content.as_str()));
    let (dir, state) = repo(&files);
    let root = dir.path();
    let module_path = module.path().to_str().unwrap();
    let add = ["-c", "protocol.file.allow=always", "submodule", "add", "-q"];
    git(root, &[&add[..], &[module_path, "sub"]].concat());
    #[cfg(unix)]
    std::os::unix::fs::symlink("real.js", root.join("link.js")).unwrap();
    commit_all(root, "submodule and link");
    start(&state, root, "outside");

    // An unchanged plain tracked file passes, old debt included.
    assert!(edit(&state, root, "outside", "old.js").stdout.is_empty());
    fs::create_dir(root.join("nested")).unwrap();
    git(&root.join("nested"), &["init", "-q"]);
    git(root, &["update-index", "--skip-worktree", "ok.js"]);
    let mut cases = vec![("nested/a.js", "nested/a.js"), ("sub/m.js", "sub/m.js")];
    cases.push(("ok.js", "ok.js"));
    if cfg!(unix) {
        cases.push(("link.js", "real.js"));
    }
    for (file, written) in cases {
        fs::write(root.join(written), violating("grown", 16)).unwrap();
        let reason = block_reason(&edit(&state, root, "outside", file));
        assert!(
            reason.starts_with("FAIL 1 changed file, 1 function, 1 violation\n"),
            "{file}: {reason}"
        );
        let listed = native(&format!("FAIL {file}  "));
        assert!(reason.contains(&listed), "{reason}");
    }
}

/// Git diffs a link as its one-line target text, so a changed link, a file
/// replaced by a link, and a new committed link are checked whole with no
/// base, as an untracked file is.
#[cfg(unix)]
#[test]
fn a_link_in_the_diff_is_checked_whole_with_no_base() {
    use std::os::unix::fs::symlink;

    let bad = format!("// first line\n{}", params("bad", 7));
    let files = [("ok.js", simple("ok")), ("bad.js", bad.clone())];
    let files = files
        .each_ref()
        .map(|(path, content)| (*path, content.as_str()));
    // The base has a link to a clean file, a plain file, or no file.
    let cases: [Case; 3] = [
        ("retarget", |_| {}),
        ("replace", |root| {
            fs::remove_file(root.join("link.js")).unwrap();
            fs::write(root.join("link.js"), simple("plain")).unwrap();
            commit_all(root, "a plain file");
        }),
        ("new", |root| {
            fs::remove_file(root.join("link.js")).unwrap();
            commit_all(root, "no link");
        }),
    ];
    for (session, base) in cases {
        let (dir, state) = repo(&files);
        let root = dir.path();
        symlink("ok.js", root.join("link.js")).unwrap();
        commit_all(root, "link");
        base(root);
        start(&state, root, session);
        let _ = fs::remove_file(root.join("link.js"));
        symlink("bad.js", root.join("link.js")).unwrap();
        let reason = block_reason(&edit(&state, root, session, "link.js"));
        assert!(
            reason.contains("FAIL link.js  1 function"),
            "{session}: {reason}"
        );
        commit_all(root, "the link");
        let reason = block_reason(&stop(&state, root, session));
        assert!(
            reason.contains("FAIL link.js  1 function"),
            "{session}: {reason}"
        );
    }
}

/// On a file system that ignores letter case, a tracked file under another
/// spelling is not plainly tracked, so its edit is checked whole, and a
/// config under another spelling still makes hooks read the base config.
#[cfg(any(windows, target_os = "macos"))]
#[test]
fn another_letter_case_neither_hides_an_edit_nor_a_config() {
    let (dir, state) = repo(&[("app.js", &simple("app")), ("src/a.js", &simple("a"))]);
    let root = dir.path();
    assert!(root.join("APP.JS").exists(), "volume is case-sensitive");
    start(&state, root, "case");
    fs::rename(root.join("app.js"), root.join("App.js")).unwrap();
    fs::write(root.join("App.js"), violating("grown", 16)).unwrap();
    let reason = block_reason(&edit(&state, root, "case", "App.js"));
    assert!(reason.starts_with("FAIL 1 changed file"), "{reason}");

    let loose = r#"{"limits":{"cognitive":null}}"#;
    fs::write(root.join("src").join(".Pickcheck.json"), loose).unwrap();
    fs::write(root.join("src").join("a.js"), violating("a", 16)).unwrap();
    let reason = block_reason(&edit(&state, root, "case", "src/a.js"));
    let note = "note: .pickcheck.json changed in this diff; the config comes from";
    assert!(reason.starts_with(note), "{reason}");
}

/// On a file system that ignores letter case, the base lookup misses a
/// config under another spelling or in a directory renamed to another case,
/// so the working-tree config applies too.
#[cfg(any(windows, target_os = "macos"))]
#[test]
fn another_letter_case_keeps_a_stricter_working_tree_config() {
    let strict = r#"{"limits":{"params":1}}"#;
    let (dir, state) = repo(&[
        ("src/.Pickcheck.json", strict),
        ("src/a.js", &params("a", 1)),
    ]);
    let root = dir.path();
    assert!(root.join("SRC").exists(), "volume is case-sensitive");
    start(&state, root, "spelling");
    fs::write(root.join("src/a.js"), params("a", 2)).unwrap();
    let reason = block_reason(&edit(&state, root, "spelling", "src/a.js"));
    assert!(reason.starts_with(WORKING_TREE_NOTE), "{reason}");
    let reason = block_reason(&stop(&state, root, "spelling"));
    assert!(reason.starts_with(WORKING_TREE_NOTE), "{reason}");

    let (dir, state) = repo(&[
        ("src/.pickcheck.json", strict),
        ("src/a.js", &params("a", 1)),
    ]);
    let root = dir.path();
    start(&state, root, "directory");
    fs::rename(root.join("src"), root.join("Src")).unwrap();
    fs::write(root.join("Src").join("a.js"), params("a", 2)).unwrap();
    let reason = block_reason(&edit(&state, root, "directory", "Src/a.js"));
    assert!(reason.contains("FAIL "), "{reason}");
}

/// The hook with stderr that cannot be written: `/dev/full` where it exists,
/// a closed descriptor elsewhere.
#[cfg(unix)]
fn hook_without_stderr(state: &Path, input: &Value) -> Output {
    let full = Path::new("/dev/full");
    let mut command = if full.exists() {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pickcheck"));
        let stderr = fs::OpenOptions::new().write(true).open(full).unwrap();
        command.args(["hook", "claude"]).stderr(stderr);
        command
    } else {
        let mut command = Command::new("sh");
        let script = "exec \"$0\" hook claude 2>&-";
        command.args(["-c", script, env!("CARGO_BIN_EXE_pickcheck")]);
        command
    };
    let mut child = command
        .env("PICKCHECK_HOME", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.to_string().as_bytes()).unwrap();
    drop(stdin);
    child.wait_with_output().unwrap()
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

/// A function with `count` parameters; the default limit is 6.
fn params(name: &str, count: usize) -> String {
    let list = (0..count)
        .map(|index| format!("p{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("function {name}({list}) {{\n  return p0;\n}}\n")
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
