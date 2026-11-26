use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const VISIT: &str = "\
function visit(items) {
  for (const x of items) {
    if (x.a && x.b) {
      continue;
    } else if (x.c) {
      try { save(x); } catch (e) {
        log(e ? e.message : x);
      }
    }
  }
}
";

const VISIT_TEXT: &str = "\
visit.js:1 visit  cognitive 12 <= 15
  2  +1  for          nesting 0
  3  +2  if           nesting 1
  3  +1  sequence &&
  5  +1  else if
  6  +3  catch        nesting 2
  7  +4  ternary      nesting 3
";

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pickcheck"))
        .current_dir(cwd)
        .arg("explain")
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn over_limit() -> String {
    let decisions = (0..16)
        .map(|index| format!("  if (x === {index}) x++;"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("function bad(x) {{\n{decisions}\n  return x;\n}}\n")
}

#[test]
fn text_lists_increments_with_the_limit_verdict() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("visit.js"), VISIT).unwrap();
    let output = run(dir.path(), &["visit.js:1"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), VISIT_TEXT);

    fs::write(
        dir.path().join("low.json"),
        r#"{"limits":{"cognitive":10}}"#,
    )
    .unwrap();
    let output = run(dir.path(), &["visit.js:1", "--config", "low.json"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).starts_with("visit.js:1 visit  cognitive 12 > 10\n"));

    fs::write(
        dir.path().join("off.json"),
        r#"{"limits":{"cognitive":null}}"#,
    )
    .unwrap();
    let output = run(dir.path(), &["visit.js:1", "--config", "off.json"]);
    assert!(stdout(&output).starts_with("visit.js:1 visit  cognitive 12\n"));
}

#[test]
fn json_carries_metrics_limits_and_increments() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("visit.js"), VISIT).unwrap();
    let output = run(
        dir.path(),
        &["visit.js", "--function", "visit", "--format", "json"],
    );
    assert_eq!(output.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["file"], "visit.js");
    let unit = &report["units"][0];
    assert_eq!(unit["function"], "visit");
    assert_eq!(unit["line"], 1);
    assert_eq!(unit["cognitive"], 12);
    assert_eq!(unit["limit"], 15);
    assert_eq!(
        unit["metrics"],
        serde_json::json!({"complexity": 7, "cognitive": 12, "depth": 3, "lines": 11,
                           "params": 1, "bool_ops": 1, "widget_depth": 0})
    );
    assert_eq!(
        unit["limits"],
        serde_json::json!({"complexity": null, "cognitive": 15, "depth": 4, "lines": 100,
                           "params": 6, "bool_ops": 3, "widget_depth": null})
    );
    assert_eq!(
        unit["increments"][2],
        serde_json::json!({"line": 3, "column": 9, "add": 1, "construct": "sequence &&",
                           "nesting": null})
    );
    assert_eq!(
        unit["increments"][5],
        serde_json::json!({"line": 7, "column": 13, "add": 4, "construct": "ternary",
                           "nesting": 3})
    );
    assert_eq!(unit["increments"].as_array().unwrap().len(), 6);
}

#[test]
fn selection_by_line_name_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    let source = format!(
        "{}function twice(a) {{\n  return a.map((b) => {{\n    return b && a;\n  }});\n}}\nfunction twice(c) {{ return c || 1; }}\n",
        over_limit()
    );
    fs::write(dir.path().join("pick.js"), &source).unwrap();
    let header = |output: &Output| {
        stdout(output)
            .lines()
            .filter(|line| !line.starts_with(' ') && !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    // bad: lines 1-19; twice: 20-24 with a closure at 21; twice again at 25.
    assert_eq!(
        header(&run(dir.path(), &["pick.js:20"])),
        ["pick.js:20 twice  cognitive 0 <= 15"]
    );
    assert_eq!(
        header(&run(dir.path(), &["pick.js:22"])),
        ["pick.js:21 <anonymous>  cognitive 1 <= 15"]
    );
    assert_eq!(
        header(&run(dir.path(), &["pick.js", "--function", "twice"])),
        [
            "pick.js:20 twice  cognitive 0 <= 15",
            "pick.js:25 twice  cognitive 1 <= 15"
        ]
    );
    let output = run(dir.path(), &["pick.js"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(header(&output), ["pick.js:1 bad  cognitive 16 > 15"]);
    assert_eq!(stdout(&output).lines().count(), 17);

    fs::write(dir.path().join("clean.js"), VISIT).unwrap();
    let output = run(dir.path(), &["clean.js"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert_eq!(
        stderr(&output),
        "no unit in clean.js exceeds the cognitive limit\n"
    );
}

#[test]
fn errors_exit_2_with_one_line() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("visit.js"), VISIT).unwrap();
    fs::write(dir.path().join("Main.kt"), "fun main() {}\n").unwrap();
    fs::write(dir.path().join("bin.png"), [0x89, 0x50, 0xff, 0xfe]).unwrap();
    fs::write(
        dir.path().join("E.svelte"),
        "<script>\n  function g(a) { return a ? 1 : 2; }\n  let y = a && b;\n</script>\n{#if y}<p>a</p>{/if}\n",
    )
    .unwrap();
    let cases: [(&[&str], &str); 9] = [
        (&["missing.js"], "cannot read missing.js"),
        (&["Main.kt"], "no grammar for Main.kt"),
        (&["bin.png"], "no grammar for bin.png"),
        (&["E.svelte:3"], "no unit in E.svelte contains line 3"),
        (&["visit.js:40"], "no unit in visit.js contains line 40"),
        (
            &["visit.js", "--function", "nope"],
            "no unit named nope in visit.js",
        ),
        (&["visit.js:"], "malformed target 'visit.js:'"),
        (&["visit.js:0"], "malformed target 'visit.js:0'"),
        (&["visit.js:3", "--function", "visit"], "not both"),
    ];
    for (args, message) in cases {
        let output = run(dir.path(), args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let error = stderr(&output);
        assert!(
            error.starts_with("error: ") && error.contains(message),
            "{args:?}: {error}"
        );
        assert_eq!(error.lines().count(), 1, "{args:?}");
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn a_file_name_ending_in_colon_digits_is_still_a_file() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("visit.js"), VISIT).unwrap();
    fs::write(dir.path().join("visit.js:1"), VISIT).unwrap();
    // `visit.js:1` names an existing file, so it is not `visit.js` line 1;
    // its extension `js:1` has no grammar.
    let output = run(dir.path(), &["visit.js:1"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stderr(&output), "error: no grammar for visit.js:1\n");
    fs::remove_file(dir.path().join("visit.js:1")).unwrap();
    assert_eq!(stdout(&run(dir.path(), &["visit.js:1"])), VISIT_TEXT);
}

#[test]
fn svelte_template_units_use_file_lines() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Page.svelte"),
        "<script>\n  let ready = true;\n</script>\n\n{#if ready && open}\n  <p>{a ? b : c}</p>\n{:else}\n  {#each items as item}\n    <i>{item}</i>\n  {/each}\n{/if}\n",
    )
    .unwrap();
    let output = run(dir.path(), &["Page.svelte:6"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        "\
Page.svelte:5 {#if}  cognitive 7 <= 15
  5  +1  {#if}        nesting 0
  5  +1  sequence &&
  6  +2  ternary      nesting 1
  7  +1  {:else}
  8  +2  {#each}      nesting 1
"
    );
}

#[test]
fn detailed_check_points_to_explain_for_cognitive_only() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("bad.js"), over_limit()).unwrap();
    fs::write(
        dir.path().join("wide.js"),
        "function wide(a, b, c, d, e, f, g) { return a; }\n",
    )
    .unwrap();
    let check = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_pickcheck"))
            .current_dir(dir.path())
            .args(args)
            .output()
            .unwrap()
    };
    let output = check(&["check", "bad.js"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stdout(&output),
        "FAIL bad.js:1 bad  cognitive 16 > 15\nEXPLAIN pickcheck explain <file>:<line>\n"
    );
    let output = check(&["check", "wide.js"]);
    assert!(!stdout(&output).contains("EXPLAIN"));
    let output = check(&["check", "--summary", "bad.js"]);
    assert!(!stdout(&output).contains("EXPLAIN"));
    let output = check(&["check", "--format", "json", "bad.js"]);
    assert!(!stdout(&output).contains("EXPLAIN"));
}

#[test]
fn an_unverified_file_explains_the_units_it_measured() {
    let dir = tempfile::tempdir().unwrap();
    let deep = format!("function deep(a) {{ return {}a; }}\n", "a+".repeat(20_000));
    fs::write(
        dir.path().join("deep.js"),
        format!("{}{deep}", over_limit()),
    )
    .unwrap();
    fs::write(dir.path().join("only.js"), &deep).unwrap();
    fs::write(
        dir.path().join("case.svelte"),
        "<script>const text = \"</scripture>\"; function bad(a) { return a; }</script>\n{#if a}<p>b</p>{/if}\n",
    )
    .unwrap();
    let note = "note: deep.js is UNVERIFIED: syntax nesting too deep to measure, \
                left out deep at line 20\n";
    for args in [
        &["deep.js"][..],
        &["deep.js:1"],
        &["deep.js", "--function", "bad"],
    ] {
        let output = run(dir.path(), args);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert!(stdout(&output).starts_with("deep.js:1 bad  cognitive 16 > 15\n"));
        assert_eq!(stderr(&output), note, "{args:?}");
    }
    let output = run(dir.path(), &["case.svelte:2"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        "case.svelte:2 {#if}  cognitive 1 <= 15\n  2  +1  {#if}  nesting 0\n"
    );
    assert_eq!(
        stderr(&output),
        "note: case.svelte is UNVERIFIED: script block could not be parsed\n"
    );
    let cases: [(&[&str], &str); 3] = [
        (
            &["deep.js:20"],
            "cannot explain deep.js:20: deep at line 20 is left out: \
             syntax nesting too deep to measure",
        ),
        (
            &["deep.js", "--function", "deep"],
            "cannot explain deep in deep.js: syntax nesting too deep to measure",
        ),
        (
            &["only.js"],
            "cannot explain only.js: syntax nesting too deep to measure",
        ),
    ];
    for (args, message) in cases {
        let output = run(dir.path(), args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(stderr(&output), format!("error: {message}\n"), "{args:?}");
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn json_without_a_unit_over_the_limit_prints_an_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("visit.js"), VISIT).unwrap();
    let output = run(dir.path(), &["visit.js", "--format", "json"]);
    assert_eq!(output.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["file"], "visit.js");
    assert_eq!(report["units"], serde_json::json!([]));
    assert!(output.stderr.is_empty());
}

#[test]
fn an_editor_position_ignores_the_column() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("visit.js"), VISIT).unwrap();
    let output = run(dir.path(), &["visit.js:3:9"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), VISIT_TEXT);
    fs::write(dir.path().join("odd:3"), VISIT).unwrap();
    let output = run(dir.path(), &["odd:3:1"]);
    assert_eq!(stderr(&output), "error: no grammar for odd:3\n");
}
