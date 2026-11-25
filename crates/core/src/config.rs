use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use crate::{BaseBlob, Metric, Status, base_blob};

const DEFAULTS: &str = include_str!("../../../config.default.json");
const LIMIT: &str = "a non-negative integer or null";
const LIST: &str = "a list of strings";
const STATUSES: &str = "a non-empty list of new, worsened, unmatched, improved, or unchanged";
const REPO_CONFIG: &str = ".pickcheck.json";

/// A `None` limit is `null` in config: the metric is measured but never fails.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub complexity: Option<usize>,
    pub cognitive: Option<usize>,
    pub depth: Option<usize>,
    pub lines: Option<usize>,
    pub params: Option<usize>,
    pub bool_ops: Option<usize>,
    pub widget_depth: Option<usize>,
}

impl Limits {
    pub fn get(&self, metric: Metric) -> Option<usize> {
        match metric {
            Metric::Complexity => self.complexity,
            Metric::Cognitive => self.cognitive,
            Metric::Depth => self.depth,
            Metric::Lines => self.lines,
            Metric::Params => self.params,
            Metric::BoolOps => self.bool_ops,
            Metric::WidgetDepth => self.widget_depth,
        }
    }

    pub fn get_mut(&mut self, metric: Metric) -> &mut Option<usize> {
        match metric {
            Metric::Complexity => &mut self.complexity,
            Metric::Cognitive => &mut self.cognitive,
            Metric::Depth => &mut self.depth,
            Metric::Lines => &mut self.lines,
            Metric::Params => &mut self.params,
            Metric::BoolOps => &mut self.bool_ops,
            Metric::WidgetDepth => &mut self.widget_depth,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestsConfig {
    pub patterns: Vec<String>,
    pub exempt: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HookConfig {
    pub max_blocks: usize,
    /// The baseline statuses that block a hook in a session.
    pub fail_on: Vec<Status>,
}

/// The outer `Option` is whether the key is present; the inner one is the
/// limit itself, so `null` turns a metric off for one language instead of
/// inheriting the global limit.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LimitOverrides {
    #[serde(
        default,
        deserialize_with = "explicit",
        skip_serializing_if = "Option::is_none"
    )]
    pub complexity: Option<Option<usize>>,
    #[serde(
        default,
        deserialize_with = "explicit",
        skip_serializing_if = "Option::is_none"
    )]
    pub cognitive: Option<Option<usize>>,
    #[serde(
        default,
        deserialize_with = "explicit",
        skip_serializing_if = "Option::is_none"
    )]
    pub depth: Option<Option<usize>>,
    #[serde(
        default,
        deserialize_with = "explicit",
        skip_serializing_if = "Option::is_none"
    )]
    pub lines: Option<Option<usize>>,
    #[serde(
        default,
        deserialize_with = "explicit",
        skip_serializing_if = "Option::is_none"
    )]
    pub params: Option<Option<usize>>,
    #[serde(
        default,
        deserialize_with = "explicit",
        skip_serializing_if = "Option::is_none"
    )]
    pub bool_ops: Option<Option<usize>>,
    #[serde(
        default,
        deserialize_with = "explicit",
        skip_serializing_if = "Option::is_none"
    )]
    pub widget_depth: Option<Option<usize>>,
}

impl LimitOverrides {
    pub fn get(&self, metric: Metric) -> Option<Option<usize>> {
        match metric {
            Metric::Complexity => self.complexity,
            Metric::Cognitive => self.cognitive,
            Metric::Depth => self.depth,
            Metric::Lines => self.lines,
            Metric::Params => self.params,
            Metric::BoolOps => self.bool_ops,
            Metric::WidgetDepth => self.widget_depth,
        }
    }
}

fn explicit<'de, D>(deserializer: D) -> Result<Option<Option<usize>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LanguageConfig {
    #[serde(default)]
    pub limits: Option<LimitOverrides>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub limits: Limits,
    pub tests: TestsConfig,
    pub ignore: Vec<String>,
    pub languages: BTreeMap<String, LanguageConfig>,
    pub hook: HookConfig,
}

#[derive(Clone, Debug)]
pub struct ConfigResolution {
    pub config: Config,
    pub chain: Vec<PathBuf>,
}

pub fn load_config(start: &Path, explicit: Option<&Path>) -> Result<ConfigResolution> {
    let (mut value, mut chain) = user_layers()?;
    let repo = explicit
        .map(Path::to_path_buf)
        .or_else(|| nearest_repo_config(start));
    if let Some(path) = repo {
        merge_file(&mut value, &path)?;
        chain.push(path);
    }
    resolve(value, chain)
}

/// Like `load_config`, but the repository layer comes from `commit`: the
/// nearest `.pickcheck.json` in that commit's tree from `start` up to the
/// repository root, then the working tree above the root. A `start` outside
/// the repository resolves as `load_config` does.
pub fn load_config_at(start: &Path, repo_root: &Path, commit: &str) -> Result<ConfigResolution> {
    if !start.starts_with(repo_root) {
        return load_config(start, None);
    }
    let (mut value, mut chain) = user_layers()?;
    if let Some((label, text)) = committed_repo_config(start, repo_root, commit)? {
        merge_text(&mut value, &text, &label)?;
        chain.push(label);
    } else if let Some(path) = repo_root.parent().and_then(nearest_repo_config) {
        merge_file(&mut value, &path)?;
        chain.push(path);
    }
    resolve(value, chain)
}

/// The built-in defaults merged with the user config.
fn user_layers() -> Result<(Value, Vec<PathBuf>)> {
    let mut value: Value = serde_json::from_str(DEFAULTS).context("invalid embedded defaults")?;
    let mut chain = vec![PathBuf::from("<built-in>")];
    if let Some(user) = user_config_path().filter(|path| path.is_file()) {
        merge_file(&mut value, &user)?;
        chain.push(user);
    }
    Ok((value, chain))
}

fn resolve(value: Value, chain: Vec<PathBuf>) -> Result<ConfigResolution> {
    let mut config: Config = serde_json::from_value(value).context("invalid configuration")?;
    config.hook.max_blocks = config.hook.max_blocks.max(1);
    Ok(ConfigResolution { config, chain })
}

/// The nearest repository config at `commit`, labelled `<commit>:<path>`.
fn committed_repo_config(
    start: &Path,
    repo_root: &Path,
    commit: &str,
) -> Result<Option<(PathBuf, String)>> {
    let start = if start.is_file() {
        start.parent().unwrap_or(start)
    } else {
        start
    };
    for directory in start.ancestors() {
        let Ok(relative) = directory.strip_prefix(repo_root) else {
            break;
        };
        let path = relative.join(REPO_CONFIG);
        let label = PathBuf::from(format!("{commit}:{}", path.display()));
        match base_blob(repo_root, commit, &path) {
            BaseBlob::Missing => {}
            BaseBlob::Unreadable => bail!("cannot read config {}", label.display()),
            BaseBlob::Content(bytes) => {
                let text = String::from_utf8(bytes)
                    .with_context(|| format!("invalid JSON in {}", label.display()))?;
                return Ok(Some((label, text)));
            }
        }
    }
    Ok(None)
}

fn user_config_path() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(root).join("pickcheck/config.json"));
    }
    dirs::config_dir().map(|root| root.join("pickcheck/config.json"))
}

fn nearest_repo_config(start: &Path) -> Option<PathBuf> {
    let start = if start.is_file() {
        start.parent()?
    } else {
        start
    };
    start
        .ancestors()
        .map(|dir| dir.join(REPO_CONFIG))
        .find(|path| path.is_file())
}

fn merge_file(base: &mut Value, path: &Path) -> Result<()> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("cannot read config {}", path.display()))?;
    merge_text(base, &text, path)
}

fn merge_text(base: &mut Value, text: &str, path: &Path) -> Result<()> {
    let patch: Value = serde_json::from_str(text)
        .with_context(|| format!("invalid JSON in {}", path.display()))?;
    validate_keys(&patch, path)?;
    shallow_merge(base, patch);
    Ok(())
}

fn shallow_merge(base: &mut Value, patch: Value) {
    let (Some(base), Value::Object(patch)) = (base.as_object_mut(), patch) else {
        return;
    };
    for (key, value) in patch {
        match (base.get_mut(&key), value) {
            (Some(Value::Object(current)), Value::Object(next)) => current.extend(next),
            (_, next) => {
                base.insert(key, next);
            }
        }
    }
}

fn validate_keys(value: &Value, path: &Path) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("config {} must be an object", path.display()))?;
    allowed(
        object,
        &["limits", "tests", "ignore", "languages", "hook"],
        "",
        path,
    )?;
    let limits = nested_keys(object, "", "limits", &Metric::ALL.map(Metric::name), path)?;
    section_values::<Option<usize>>(limits, "limits", LIMIT, path)?;
    let tests = nested_keys(object, "", "tests", &["patterns", "exempt"], path)?;
    section_values::<Vec<String>>(tests, "tests", LIST, path)?;
    validate_test_exempt(object, path)?;
    let hook = nested_keys(object, "", "hook", &["max_blocks", "fail_on"], path)?;
    validate_hook(hook, path)?;
    if let Some(ignore) = object.get("ignore") {
        value_type::<Vec<String>>(ignore, "ignore", LIST, path)?;
    }
    validate_language_keys(object, path)
}

fn nested_keys<'a>(
    root: &'a Map<String, Value>,
    parent: &str,
    key: &str,
    keys: &[&str],
    path: &Path,
) -> Result<Option<&'a Map<String, Value>>> {
    let Some(value) = root.get(key) else {
        return Ok(None);
    };
    let full = if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    };
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{full} in {} must be an object", path.display()))?;
    allowed(object, keys, &full, path)?;
    Ok(Some(object))
}

/// Checks each value against the type its `Config` field deserializes into,
/// so a wrong type is reported with its key and file instead of after merging.
fn section_values<T: DeserializeOwned>(
    section: Option<&Map<String, Value>>,
    prefix: &str,
    expected: &str,
    path: &Path,
) -> Result<()> {
    for (key, value) in section.into_iter().flatten() {
        value_type::<T>(value, &format!("{prefix}.{key}"), expected, path)?;
    }
    Ok(())
}

fn value_type<T: DeserializeOwned>(
    value: &Value,
    key: &str,
    expected: &str,
    path: &Path,
) -> Result<()> {
    if T::deserialize(value).is_err() {
        bail!("`{key}` in {} must be {expected}", path.display());
    }
    Ok(())
}

fn validate_hook(hook: Option<&Map<String, Value>>, path: &Path) -> Result<()> {
    let Some(hook) = hook else {
        return Ok(());
    };
    if let Some(value) = hook.get("max_blocks") {
        value_type::<usize>(value, "hook.max_blocks", "a non-negative integer", path)?;
    }
    if let Some(value) = hook.get("fail_on")
        && !Vec::<Status>::deserialize(value).is_ok_and(|statuses| !statuses.is_empty())
    {
        bail!("`hook.fail_on` in {} must be {STATUSES}", path.display());
    }
    Ok(())
}

fn validate_test_exempt(root: &Map<String, Value>, path: &Path) -> Result<()> {
    let Some(exempt) = root
        .get("tests")
        .and_then(Value::as_object)
        .and_then(|tests| tests.get("exempt"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    for value in exempt {
        if value.as_str() != Some("lines") {
            bail!(
                "unsupported tests.exempt key `{}` in {}",
                value.as_str().unwrap_or("<non-string>"),
                path.display()
            );
        }
    }
    Ok(())
}

fn validate_language_keys(root: &Map<String, Value>, path: &Path) -> Result<()> {
    const LANGUAGES: &[&str] = &[
        "javascript",
        "typescript",
        "svelte",
        "dart",
        "rust",
        "python",
        "go",
    ];
    let Some(value) = root.get("languages") else {
        return Ok(());
    };
    let languages = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("languages in {} must be an object", path.display()))?;
    for (name, value) in languages {
        if !LANGUAGES.contains(&name.as_str()) {
            bail!(
                "unknown config key `languages.{name}` in {}",
                path.display()
            );
        }
        let object = value.as_object().ok_or_else(|| {
            anyhow::anyhow!("languages.{name} in {} must be an object", path.display())
        })?;
        let prefix = format!("languages.{name}");
        allowed(object, &["limits"], &prefix, path)?;
        let limits = nested_keys(
            object,
            &prefix,
            "limits",
            &Metric::ALL.map(Metric::name),
            path,
        )?;
        section_values::<Option<usize>>(limits, &format!("{prefix}.limits"), LIMIT, path)?;
    }
    Ok(())
}

fn allowed(object: &Map<String, Value>, keys: &[&str], prefix: &str, path: &Path) -> Result<()> {
    for key in object.keys() {
        if !keys.contains(&key.as_str()) {
            let full = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            bail!("unknown config key `{full}` in {}", path.display());
        }
    }
    Ok(())
}

impl Config {
    pub fn limits_for(&self, language: &str) -> Limits {
        let Some(overrides) = self
            .languages
            .get(language)
            .and_then(|entry| entry.limits.as_ref())
        else {
            return self.limits.clone();
        };
        let mut limits = self.limits.clone();
        for metric in Metric::ALL {
            if let Some(limit) = overrides.get(metric) {
                *limits.get_mut(metric) = limit;
            }
        }
        limits
    }

    pub fn matcher(patterns: &[String]) -> Result<GlobSet> {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            builder.add(Glob::new(pattern)?);
        }
        Ok(builder.build()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_paths_built_with_native_separators() {
        let ignore = Config::matcher(&["gen/**".to_owned()]).unwrap();
        let tests =
            Config::matcher(&["**/tests/**".to_owned(), "**/*_test.go".to_owned()]).unwrap();
        let generated = Path::new("gen").join("sub").join("a.rs");
        assert!(ignore.is_match(&generated));
        assert!(!ignore.is_match(Path::new("src").join("gen.rs")));
        assert!(tests.is_match(Path::new("pkg").join("tests").join("a.js")));
        assert!(tests.is_match(Path::new("pkg").join("a_test.go")));
        assert!(!tests.is_match(Path::new("pkg").join("a.go")));
    }

    #[test]
    fn partial_top_level_objects_merge() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"limits":{"complexity":7}}"#).unwrap();
        let resolved = load_config(dir.path(), Some(&path)).unwrap();
        assert_eq!(resolved.config.limits.complexity, Some(7));
        assert_eq!(resolved.config.limits.depth, Some(4));
    }

    #[test]
    fn partial_language_limits_inherit_global_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"languages":{"rust":{"limits":{"complexity":4}}}}"#,
        )
        .unwrap();
        let config = load_config(dir.path(), Some(&path)).unwrap().config;
        let limits = config.limits_for("rust");
        assert_eq!(limits.complexity, Some(4));
        assert_eq!(limits.cognitive, Some(15));
        assert_eq!(limits.depth, Some(4));
        assert_eq!(limits.bool_ops, Some(3));
        assert_eq!(limits.widget_depth, Some(7));
    }

    #[test]
    fn readability_limits_accept_global_and_language_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"limits":{"bool_ops":2},"languages":{"dart":{"limits":{"widget_depth":5,"cognitive":9}}}}"#,
        )
        .unwrap();
        let config = load_config(dir.path(), Some(&path)).unwrap().config;
        assert_eq!(config.limits_for("javascript").bool_ops, Some(2));
        assert_eq!(config.limits_for("dart").bool_ops, Some(2));
        assert_eq!(config.limits_for("dart").widget_depth, Some(5));
        assert_eq!(config.limits_for("dart").cognitive, Some(9));
        assert_eq!(config.limits_for("javascript").cognitive, Some(15));
    }

    #[test]
    fn complexity_is_off_by_default_and_null_turns_limits_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "{}").unwrap();
        let defaults = load_config(dir.path(), Some(&path)).unwrap().config;
        assert_eq!(defaults.limits.complexity, None);
        assert_eq!(defaults.limits.cognitive, Some(15));
        fs::write(
            &path,
            r#"{"limits":{"complexity":15,"depth":null},"languages":{"rust":{"limits":{"complexity":null}}}}"#,
        )
        .unwrap();
        let config = load_config(dir.path(), Some(&path)).unwrap().config;
        assert_eq!(config.limits_for("go").complexity, Some(15));
        assert_eq!(config.limits_for("go").depth, None);
        assert_eq!(config.limits_for("rust").complexity, None);
        assert_eq!(config.limits_for("rust").cognitive, Some(15));
    }

    #[test]
    fn test_exempt_rejects_metrics_other_than_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"tests":{"exempt":["depth"]}}"#).unwrap();
        let error = load_config(dir.path(), Some(&path))
            .unwrap_err()
            .to_string();
        assert!(error.contains("tests.exempt") && error.contains("depth"));
    }

    #[test]
    fn hook_max_blocks_is_clamped_to_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"hook":{"max_blocks":0}}"#).unwrap();
        let config = load_config(dir.path(), Some(&path)).unwrap().config;
        assert_eq!(config.hook.max_blocks, 1);
    }

    #[test]
    fn unknown_nested_key_names_full_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"hook":{"blocks":2}}"#).unwrap();
        let error = load_config(dir.path(), Some(&path))
            .unwrap_err()
            .to_string();
        assert!(error.contains("hook.blocks"));
    }

    #[test]
    fn unknown_language_limit_key_names_full_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"languages":{"go":{"limits":{"widgetdepth":1}}}}"#,
        )
        .unwrap();
        let error = load_config(dir.path(), Some(&path))
            .unwrap_err()
            .to_string();
        assert!(error.contains("`languages.go.limits.widgetdepth`"));
    }

    #[test]
    fn unknown_language_limit_key_names_the_language_with_the_typo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"languages":{"go":{"limits":{"depth":3}},"rust":{"limits":{"widgetdepth":1}}}}"#,
        )
        .unwrap();
        let error = load_config(dir.path(), Some(&path))
            .unwrap_err()
            .to_string();
        assert!(error.contains("`languages.rust.limits.widgetdepth`"));
        assert!(!error.contains("languages.go"));
    }

    #[test]
    fn non_object_language_limits_names_full_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"languages":{"go":{"limits":1}}}"#).unwrap();
        let error = load_config(dir.path(), Some(&path))
            .unwrap_err()
            .to_string();
        assert!(error.contains("languages.go.limits in ") && error.contains("must be an object"));
    }

    #[test]
    fn non_object_language_entry_names_config_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"languages":{"go":1}}"#).unwrap();
        let error = load_config(dir.path(), Some(&path))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!("languages.go in {} must be an object", path.display())
        );
    }

    fn config_error(json: &str) -> (String, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, json).unwrap();
        let error = load_config(dir.path(), Some(&path))
            .unwrap_err()
            .to_string();
        (error, path)
    }

    #[test]
    fn string_global_limit_names_key_and_config_path() {
        let (error, path) = config_error(r#"{"limits":{"depth":"4"}}"#);
        assert_eq!(
            error,
            format!(
                "`limits.depth` in {} must be a non-negative integer or null",
                path.display()
            )
        );
    }

    #[test]
    fn string_language_limit_names_full_path() {
        let (error, path) = config_error(r#"{"languages":{"go":{"limits":{"depth":"4"}}}}"#);
        assert_eq!(
            error,
            format!(
                "`languages.go.limits.depth` in {} must be a non-negative integer or null",
                path.display()
            )
        );
    }

    #[test]
    fn float_limit_is_rejected() {
        let (error, path) = config_error(r#"{"limits":{"lines":1.5}}"#);
        assert_eq!(
            error,
            format!(
                "`limits.lines` in {} must be a non-negative integer or null",
                path.display()
            )
        );
    }

    #[test]
    fn negative_max_blocks_names_key_and_config_path() {
        let (error, path) = config_error(r#"{"hook":{"max_blocks":-1}}"#);
        assert_eq!(
            error,
            format!(
                "`hook.max_blocks` in {} must be a non-negative integer",
                path.display()
            )
        );
    }

    #[test]
    fn hook_fail_on_defaults_to_new_worsened_and_unmatched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"hook":{"fail_on":["unchanged","new"]}}"#).unwrap();
        let config = load_config(dir.path(), Some(&path)).unwrap().config;
        assert_eq!(config.hook.fail_on, [Status::Unchanged, Status::New]);
        assert_eq!(config.hook.max_blocks, 3);
        fs::write(&path, "{}").unwrap();
        let defaults = load_config(dir.path(), Some(&path)).unwrap().config;
        assert_eq!(
            defaults.hook.fail_on,
            [Status::New, Status::Worsened, Status::Unmatched]
        );
    }

    #[test]
    fn bad_hook_fail_on_names_key_and_config_path() {
        for json in [
            r#"{"hook":{"fail_on":["new","fixed"]}}"#,
            r#"{"hook":{"fail_on":"new"}}"#,
            r#"{"hook":{"fail_on":[]}}"#,
        ] {
            let (error, path) = config_error(json);
            assert_eq!(
                error,
                format!(
                    "`hook.fail_on` in {} must be a non-empty list of new, worsened, unmatched, improved, or unchanged",
                    path.display()
                ),
                "{json}"
            );
        }
    }

    #[test]
    fn string_ignore_is_rejected_as_non_list() {
        let (error, path) = config_error(r#"{"ignore":"dist/**"}"#);
        assert_eq!(
            error,
            format!("`ignore` in {} must be a list of strings", path.display())
        );
    }

    #[test]
    fn non_string_test_pattern_is_rejected() {
        let (error, path) = config_error(r#"{"tests":{"patterns":["a",1]}}"#);
        assert_eq!(
            error,
            format!(
                "`tests.patterns` in {} must be a list of strings",
                path.display()
            )
        );
    }

    #[test]
    fn string_test_exempt_is_rejected_as_non_list() {
        let (error, path) = config_error(r#"{"tests":{"exempt":"lines"}}"#);
        assert_eq!(
            error,
            format!(
                "`tests.exempt` in {} must be a list of strings",
                path.display()
            )
        );
    }

    #[test]
    fn non_string_test_exempt_entry_gets_list_message() {
        let (error, path) = config_error(r#"{"tests":{"exempt":["lines",1]}}"#);
        assert_eq!(
            error,
            format!(
                "`tests.exempt` in {} must be a list of strings",
                path.display()
            )
        );
    }

    #[test]
    fn null_limits_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"limits":{"depth":null},"languages":{"go":{"limits":{"lines":null}}}}"#,
        )
        .unwrap();
        let config = load_config(dir.path(), Some(&path)).unwrap().config;
        assert_eq!(config.limits.depth, None);
        assert_eq!(config.limits_for("go").lines, None);
    }

    #[test]
    fn unknown_language_names_config_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"languages":{"cobol":1}}"#).unwrap();
        let error = load_config(dir.path(), Some(&path))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!("unknown config key `languages.cobol` in {}", path.display())
        );
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn repo_config_at_a_commit_ignores_working_tree_edits() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::physical(dir.path());
        let package = root.join("pkg");
        fs::create_dir(&package).unwrap();
        fs::write(root.join(REPO_CONFIG), r#"{"limits":{"depth":2}}"#).unwrap();
        fs::write(package.join(REPO_CONFIG), r#"{"limits":{"params":1}}"#).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "user.name", "Test"]);
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "initial"]);
        let commit = git(&root, &["rev-parse", "HEAD"]);
        fs::write(root.join(REPO_CONFIG), r#"{"limits":{"depth":null}}"#).unwrap();
        fs::remove_file(package.join(REPO_CONFIG)).unwrap();

        let top = load_config_at(&root, &root, &commit).unwrap();
        assert_eq!(top.config.limits.depth, Some(2));
        assert_eq!(
            top.chain.last().unwrap(),
            &PathBuf::from(format!("{commit}:{REPO_CONFIG}"))
        );
        let nested = load_config_at(&package.join("a.js"), &root, &commit).unwrap();
        assert_eq!(nested.config.limits.params, Some(1));
        assert_eq!(nested.config.limits.depth, Some(4));
        assert_eq!(load_config(&root, None).unwrap().config.limits.depth, None);

        fs::write(root.join(REPO_CONFIG), "{").unwrap();
        git(&root, &["commit", "-qam", "broken"]);
        let broken = git(&root, &["rev-parse", "HEAD"]);
        let error = load_config_at(&root, &root, &broken).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("invalid JSON in {broken}:{REPO_CONFIG}")
        );
    }

    #[test]
    fn unknown_top_level_limit_key_has_no_language_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"limits":{"widgetdepth":1}}"#).unwrap();
        let error = load_config(dir.path(), Some(&path))
            .unwrap_err()
            .to_string();
        assert!(error.contains("key `limits.widgetdepth`"));
    }
}
