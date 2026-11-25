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

## Validation

- <What was actually run, and where its evidence lives. Nothing aspirational.>

## Known limits

- <What this release does not do, and what is not proven yet.>
