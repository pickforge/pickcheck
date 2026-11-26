# Hook mapping

Verified against the live documentation on **2026-09-03**:

- [Claude Code hooks](https://docs.anthropic.com/en/docs/claude-code/hooks)
- [Codex hooks](https://developers.openai.com/codex/hooks)
- [Cursor hooks](https://cursor.com/docs/hooks)
- [Grok hooks](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/10-hooks.md)

| Harness | Edited-file event | Completion event | Violation result |
|---|---|---|---|
| Claude Code | `PostToolUse` for `Edit`, `Write`, `MultiEdit` | `Stop` | `decision: "block"` JSON |
| Codex | `PostToolUse` for `apply_patch`, `Edit`, `Write` | `Stop` | `decision: "block"` JSON |
| Cursor | `afterFileEdit` with top-level `file_path` | `stop` with `status: "completed"` | edit annotation on stderr; stop `followup_message` JSON |
| Grok | `post_tool_use` for edit tools | `stop` | edit annotation on stderr; stop exit 2 with reason on stderr |

## Limitations

Codex exposes an `apply_patch` command string, not a stable edited-file field.
For that event the gate checks all functions touched in the Git diff. This is
less precise if unrelated working-tree edits already exist. Neither post-tool
hook can undo an edit that already happened.

Cursor and Grok post-edit hooks are passive, so immediate findings appear in
their hook output rather than becoming agent instructions. Their completion
hooks still force a correction pass.

Every automatic finding uses the compact report: totals, at most 20 failing or
unverified paths, an omitted count, and a scoped command for detailed output.
Passing functions are never printed.

A run with no violations never blocks and prints nothing to stdout. When it has
unverified files, such as a changed file too deep to measure, it writes one
`note: UNVERIFIED <file>  <reason>` line per file to stderr, at most 20, and a
`note: ... <n> more files` line for the rest. This holds for every harness, on
both the edit and the completion event.

The stop loop guard is shared across harnesses. Counters are keyed by the
sanitized, length-capped session or conversation ID under `~/.pickforge/pickcheck/`, or
`PICKCHECK_HOME` when set. The hook blocks the first `hook.max_blocks`
consecutive failing Stop events. Later failing Stop events report `UNRESOLVED`
on stderr and do not reset the counter; only a clean Stop resets it. A missing
ID uses the `unkeyed` counter. Working-directory fallbacks use each harness's
workspace field or environment variable before the process directory.

## Sessions

Hooks scope the gate to the agent session, as "Hook sessions" in
`docs/spec.md` defines. The first event of a session in a repository records
`HEAD` as the anchor. Every event records the edited file and the files that
are changed or untracked at that moment. During an unfinished merge,
cherry-pick, revert, or rebase, an event records only the edited file and the
unmerged files. `Stop` checks the diff from the merge
base of the anchor and `HEAD` to the working tree, restricted to the recorded
files. A post-edit event checks the functions that the session changed in the
edited file. An edit of a file outside the repository checks the whole file,
as without a session. An edit of a file that is in no diff checks the whole
file with no base, so its violations are new, unless the index tracks it as a
plain file. This covers ignored files, files of nested repositories and
submodules, skip-worktree entries, and links. Files outside the diff that the
session does not edit are never checked. Commits made during the session stay in scope. Commits that a
merge or rebase brings in count only where they touch a recorded file.

Violations are compared with that base. `hook.fail_on` lists the statuses that
block, by default `["new", "worsened", "unmatched"]`. A block reason lists the
other violations as `WARN`; they never block alone. To block on every
violation in scope, set:

```json
{"hook": {"fail_on": ["new", "worsened", "unmatched", "improved", "unchanged"]}}
```

When a `.pickcheck.json` that a hook would read differs from the base, or the
diff changes any `.pickcheck.json`, hooks read the repository config from the
base commit, and the block reason starts with a note that says so. The
comparison reads content through links, so a config that Git ignores, a
skip-worktree entry, or a changed link target counts too.

The state lives in `sessions/` under the state directory: one
`<session>.<key hash>.anchor` file and one append-only
`<session>.<key hash>.files` log per session and repository. The key hash
covers the raw session ID and the repository root. The anchor is written to a
temporary file and published with a hard link, so a reader never sees half of
it. A hook that starts a session deletes the state files of other sessions
that are older than 30 days, and no other file.

Session code never fails a hook. When the state directory cannot be written,
the anchor or log is unusable, or another step of the session gate fails,
such as a Git command or a base config that does not load, the hook prints
one `note:` line to stderr and works as without a session. A session run that does not block writes the
same `note: UNVERIFIED` lines as a run outside a session. A failed write to
stderr is ignored. A session started from a linked working directory and
continued from the physical one keeps one anchor.

Outside Git, without `HEAD`, or without a session ID, hooks work as before
sessions. When the anchor no longer resolves or has no merge base with `HEAD`,
they work as before and the block reason starts with a note.

A file that a shell command changes and commits before the next hook event is
not attributed to the session. An installer can close this gap by sending
shell tool events to the hook, because every event records the changed files.

Claude Code's `stop_hook_active` input field is intentionally ignored. The
local counter is the loop guard for every harness, regardless of whether a
Stop event was triggered by another Stop hook continuation.
