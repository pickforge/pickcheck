# PickCheck <version>

<One paragraph on what this release is for.>

## Changes

- An unknown or non-object per-language limit key in `.pickcheck.json` is now
  reported with its `languages.<name>` path, for example
  `languages.go.limits.widgetdepth`, instead of a bare `limits.widgetdepth`.
  (#41)
- An unknown language name or a non-object `languages.<name>` entry is now
  reported with the config file it came from, so it is clear whether the user
  or the repo config holds the typo. An unknown language is reported before
  any error inside its entry. (#43)
- A config value of the wrong type, such as a string limit or a negative
  `hook.max_blocks`, is now reported with its full key and the config file it
  came from, instead of a generic type error after the files are merged. (#46)
- Hooks find the state directory through the platform home directory, so a
  `Stop` hook no longer fails with `cannot determine home directory` on
  Windows, where `HOME` is usually unset. `PICKCHECK_HOME` still takes
  precedence. (#50)
- A repository reached through a symlink, such as a macOS temporary directory
  under `/var`, or a Windows short path, now gives the same results as its
  canonical path. Before, `check --changed <path>` could find no changes,
  `ignore` globs could miss, and hook reports could show long `../` paths.
  An explicit path now follows directory links before `..` applies. (#50)
- On Windows, `--changed` and `--base` reports now use `\` in every path
  instead of mixing it with the `/` that Git prints. (#50)
- Svelte `<script>` blocks and their `lang` now come from the Svelte syntax
  tree, so a `<script>` inside an HTML comment is no longer measured. A script
  is measured from its start tag to its end tag, and a script that the grammar
  cannot close, as with a string such as `"</scripture>"`, is reported
  `UNVERIFIED <file>  script block could not be parsed` instead of being
  dropped. Template `complexity` and `bool_ops` now count operator nodes in
  every parsed template expression, bindings included, with the component's
  script grammar, so a `?` or `||` inside a regular expression, string, or
  comment no longer adds a decision. Each text is parsed in the position its
  role in the Svelte tree gives it, so operators in snippet parameter
  defaults, `{@const}` declarations, and destructuring defaults of bindings
  still count. (#49)
- A function or Svelte template unit whose syntax tree is more than 3,000
  levels high, such as a generated 20,000-term expression, is now left out,
  and its file is reported `UNVERIFIED <file>  syntax nesting too deep to
  measure`. The other units of the file are still measured, and the scan
  continues, instead of aborting the process with a stack overflow in `check`
  and in every hook. A hook run that does not block now writes a
  `note: UNVERIFIED` line to stderr for each unverified file, so such a file
  never passes in silence. (#49)
- `lines` now indexes each file's lines and comments once instead of
  rescanning the file for every function, so a 100,000-line file with 20,000
  functions is measured in about a second instead of tens of seconds. (#49)
- Hooks now anchor the gate to the agent session (#53). The first hook event
  of a session records `HEAD`, and `Stop` checks the diff from the merge base
  of that commit and `HEAD`, restricted to the files the session touched. A
  violation the agent commits before it stops still blocks, and a commit that
  a merge or rebase brings in counts only where it touches a session file.
  Session state lives in `sessions/` under the state directory, and state
  older than 30 days is deleted. A state failure, such as an unwritable state
  directory or a corrupt anchor, is one `note:` line on stderr, and the hook
  then works as without a session; it never changes the exit code or stdout.
  An edit of a file outside the repository still checks the whole file.
- Default change: hooks now compare every violation with the session base and
  block only the statuses in the new `hook.fail_on` key, by default
  `["new", "worsened", "unmatched"]`. A legacy violation that an edit leaves
  `unchanged` or `improved` is listed as `WARN` when something else blocks and
  never blocks alone. `PostToolUse` checks only the functions the session
  changed in the edited file, not the whole file. To restore the old
  strictness, set
  `"hook": {"fail_on": ["new", "worsened", "unmatched", "improved", "unchanged"]}`.
- When a session changes any `.pickcheck.json`, hooks read the repository
  config from the session base, so loosening a limit no longer empties the
  gate, and the block reason carries the note. (#53)

## Validation

- <What was actually run, and where its evidence lives. Nothing aspirational.>

## Known limits

- <What this release does not do, and what is not proven yet.>
