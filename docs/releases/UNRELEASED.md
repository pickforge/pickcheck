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
- Svelte `<script>` blocks and their `lang` now come from the Svelte syntax
  tree, so a `<script>` inside an HTML comment is no longer measured. Template
  `complexity` and `bool_ops` now count operator nodes in the parsed
  expression, with the component's script grammar, so a `?` or `||` inside a
  regular expression, string, or comment no longer adds a decision. (#49)
- A file whose syntax tree is deeper than 3,000 levels, such as a generated
  20,000-term expression, is now reported `UNVERIFIED <file>  syntax nesting
  too deep to measure` and the scan continues, instead of aborting the process
  with a stack overflow in `check` and in every hook. (#49)
- `lines` now indexes each file's lines and comments once instead of
  rescanning the file for every function, so a 100,000-line file with 20,000
  functions is measured in about a second instead of tens of seconds. (#49)
- New `pickcheck explain <file>[:<line>]` prints where a unit's `cognitive`
  score comes from, one line per increment with its line, amount, construct,
  and nesting level, as text or JSON. Detailed `check` output ends with an
  `EXPLAIN` hint when it lists a `cognitive` violation. (#51)

## Validation

- <What was actually run, and where its evidence lives. Nothing aspirational.>

## Known limits

- <What this release does not do, and what is not proven yet.>
