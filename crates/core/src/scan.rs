use std::{
    collections::BTreeSet,
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use serde::Serialize;

use crate::{
    BaseBlob, ChangedFiles, CommitConfigs, Config, FunctionMetrics, Language, Limits, LineRange,
    Metric, ParsedSource,
    baseline::{self, MetricValues, Pairing, Status},
    diff::{physical, repository_root},
    load_config, load_config_at, parse_source,
};

/// The scan note of a diff that changes a `.pickcheck.json`, when the
/// configs come from the working tree.
pub const CONFIG_CHANGED_NOTE: &str = ".pickcheck.json changed in this diff";

const UNVERIFIED_SOURCE_EXTENSIONS: &[&str] = &[
    "kt", "java", "c", "cc", "cpp", "h", "hpp", "cs", "swift", "rb", "php", "scala", "lua", "zig",
    "m", "mm", "ex", "exs", "hs", "clj", "sh", "bash", "pl", "r",
];

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Violation {
    pub file: PathBuf,
    pub line: usize,
    pub function: String,
    pub metric: String,
    pub value: usize,
    pub limit: usize,
    /// Present only with `--base`.
    #[serde(flatten)]
    pub baseline: Option<Baseline>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Baseline {
    pub status: Status,
    /// `None` for `new` and `unmatched`.
    pub base_metrics: Option<MetricValues>,
    /// Effective limits for this function, `None` where a check is off or
    /// does not apply.
    pub limits: Limits,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Unverified {
    pub file: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct ScanResult {
    pub checked: usize,
    pub violations: Vec<Violation>,
    pub unverified: Vec<Unverified>,
    pub notes: Vec<String>,
    pub functions: Vec<(PathBuf, FunctionMetrics)>,
}

pub struct ScanOptions<'a> {
    pub cwd: &'a Path,
    pub paths: &'a [PathBuf],
    pub explicit_config: Option<&'a Path>,
    pub changed: Option<&'a ChangedFiles>,
    /// Read the repository config of each file from this commit instead of
    /// the working tree.
    pub commit_configs: Option<&'a CommitConfigs>,
}

struct ScanFile {
    path: PathBuf,
    explicit: bool,
}

/// One scanned source file and what decides its violations.
struct FileContext<'a> {
    display: &'a Path,
    matched: &'a Path,
    language: Language,
    config: &'a Config,
    test_file: bool,
}

impl FileContext<'_> {
    fn test_exempt(&self, metric: Metric) -> bool {
        self.test_file
            && self
                .config
                .tests
                .exempt
                .iter()
                .any(|item| item == metric.name())
    }
}

/// Scans the files that `options` selects.
///
/// The depth guarantee of `parse_source` holds only inside
/// `on_measurement_stack`, so a caller on a small stack must run this there.
///
/// # Errors
///
/// Fails on a bad config, an unreadable path, or a Git failure.
pub fn scan(options: &ScanOptions<'_>) -> Result<ScanResult> {
    let cwd = physical(options.cwd);
    let options = &ScanOptions {
        cwd: &cwd,
        paths: options.paths,
        explicit_config: options.explicit_config,
        changed: options.changed,
        commit_configs: options.commit_configs,
    };
    let roots = scan_roots(options);
    let match_base = match_base(options, &roots)?;
    let files = if let Some(changed) = options.changed.filter(|changed| !changed.fallback) {
        collect_changed_files(changed, &roots, !options.paths.is_empty())?
    } else {
        let base_config = load_config(options.cwd, options.explicit_config)?.config;
        collect_files(&roots, &match_base, &base_config)?
    };
    let mut result = ScanResult::default();
    if let Some(configs) = options.commit_configs {
        result.notes.push(format!(
            ".pickcheck.json changed in this diff; the config comes from {}",
            configs.commit()
        ));
    } else if options.changed.is_some_and(config_changed) {
        result.notes.push(CONFIG_CHANGED_NOTE.to_owned());
    }
    for file in files {
        scan_file(&file, &match_base, options, &mut result)?;
    }
    result
        .violations
        .sort_by(|a, b| (&a.file, a.line, &a.metric).cmp(&(&b.file, b.line, &b.metric)));
    result.unverified.sort_by(|a, b| a.file.cmp(&b.file));
    result
        .functions
        .sort_by(|a, b| (&a.0, a.1.line).cmp(&(&b.0, b.1.line)));
    Ok(result)
}

fn scan_roots(options: &ScanOptions<'_>) -> Vec<PathBuf> {
    if options.paths.is_empty() {
        vec![options.cwd.to_path_buf()]
    } else {
        options
            .paths
            .iter()
            .map(|path| resolve_path(options.cwd, path))
            .collect()
    }
}

fn match_base(options: &ScanOptions<'_>, roots: &[PathBuf]) -> Result<PathBuf> {
    if let Some(changed) = options.changed.filter(|changed| !changed.fallback) {
        return Ok(changed.repo_root.clone());
    }
    Ok(repository_root(options.cwd)?
        .or_else(|| nearest_project_root(options.cwd))
        .unwrap_or_else(|| common_scan_root(roots)))
}

fn common_scan_root(roots: &[PathBuf]) -> PathBuf {
    let mut components = scan_anchor(&roots[0]).components().collect::<Vec<_>>();
    for root in &roots[1..] {
        let other = scan_anchor(root).components().collect::<Vec<_>>();
        components.truncate(
            components
                .iter()
                .zip(other)
                .take_while(|(left, right)| left == &right)
                .count(),
        );
    }
    components.iter().collect()
}

fn scan_anchor(root: &Path) -> &Path {
    let parent = root.parent().unwrap_or(root);
    if root.is_file() {
        parent.parent().unwrap_or(parent)
    } else {
        parent
    }
}

fn nearest_project_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|directory| directory.join(".pickcheck.json").is_file())
        .map(Path::to_path_buf)
}

/// Whether the diff adds, edits, deletes, or renames a `.pickcheck.json`.
pub fn config_changed(changed: &ChangedFiles) -> bool {
    !changed.fallback
        && changed
            .touched
            .iter()
            .chain(&changed.untracked)
            .any(|path| {
                path.file_name()
                    .is_some_and(|name| name == ".pickcheck.json")
            })
}

fn collect_changed_files(
    changed: &ChangedFiles,
    roots: &[PathBuf],
    intersect_paths: bool,
) -> Result<Vec<ScanFile>> {
    if intersect_paths {
        for root in roots {
            if !root.exists() {
                anyhow::bail!("unreadable path {}", root.display());
            }
        }
    }
    let selected = |path: &Path| {
        !intersect_paths
            || roots
                .iter()
                .any(|root| root.is_dir() && path.starts_with(root) || path == root)
    };
    let mut files = changed
        .spans
        .keys()
        .chain(&changed.untracked)
        .map(|path| changed.repo_root.join(path))
        .filter(|path| selected(path))
        .map(|path| ScanFile {
            path,
            explicit: false,
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files.dedup_by(|left, right| left.path == right.path);
    Ok(files)
}

fn collect_files(roots: &[PathBuf], base: &Path, config: &Config) -> Result<Vec<ScanFile>> {
    let ignores = Config::matcher(&config.ignore)?;
    let mut files = Vec::new();
    for root in roots {
        if root.is_file() {
            files.push(ScanFile {
                path: root.clone(),
                explicit: true,
            });
            continue;
        }
        if !root.exists() {
            anyhow::bail!("unreadable path {}", root.display());
        }
        let walker = WalkBuilder::new(root)
            .standard_filters(true)
            .hidden(false)
            .build();
        for entry in walker {
            let entry = entry.with_context(|| format!("cannot walk {}", root.display()))?;
            let path = entry.path();
            if entry.file_type().is_some_and(|kind| kind.is_file())
                && !ignores.is_match(relative(base, path))
            {
                files.push(ScanFile {
                    path: entry.into_path(),
                    explicit: false,
                });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    Ok(files)
}

fn scan_file(
    file: &ScanFile,
    match_base: &Path,
    options: &ScanOptions<'_>,
    result: &mut ScanResult,
) -> Result<()> {
    let display = relative(options.cwd, &file.path);
    let matched_path = relative(match_base, &file.path);
    let config = file_config(&file.path, options)?;
    if Config::matcher(&config.ignore)?.is_match(&matched_path) {
        return Ok(());
    }
    let Some(language) = Language::from_path(&file.path) else {
        if file.explicit || unverified_source_path(&file.path) {
            result.unverified.push(Unverified {
                file: display,
                reason: extension_reason(&file.path),
            });
        }
        return Ok(());
    };
    let source = match fs::read_to_string(&file.path) {
        Ok(source) => source,
        Err(error) => {
            result.unverified.push(Unverified {
                file: display,
                reason: read_reason(&error),
            });
            return Ok(());
        }
    };
    let parsed = parse_source(language, &source)?;
    if let Some(reason) = parsed.reason() {
        result.unverified.push(Unverified {
            file: display.clone(),
            reason,
        });
    }
    let context = FileContext {
        display: &display,
        matched: &matched_path,
        language,
        config: &config,
        test_file: Config::matcher(&config.tests.patterns)?.is_match(&matched_path),
    };
    let spans = changed_spans(&matched_path, options.changed);
    let compared = options.changed.filter(|changed| changed.base.is_some());
    let measure_untouched =
        spans.is_some() && compared.is_some_and(|changed| changed.measure_untouched);
    let first = result.violations.len();
    let measured = measure_units(&context, &parsed, spans, measure_untouched, result);
    if let Some(changed) = compared
        && !measured.units_of.is_empty()
    {
        let current = Current {
            source: &source,
            parsed: &parsed,
            measured: &measured,
        };
        compare_with_base(changed, &context, &current, result)?;
    }
    if measure_untouched {
        keep_untouched_changes(result, first, &measured, &context, &parsed);
    }
    Ok(())
}

/// The units that a scan measured in one file.
struct Measured {
    /// The unit of each of the file's violations, by index.
    units_of: Vec<usize>,
    /// Whether each unit was measured although no hunk touches it.
    untouched: Vec<bool>,
}

/// Adds the violations of the units in `spans`, every unit without spans,
/// and with `measure_untouched` every other unit too. Only touched units
/// count as checked here.
fn measure_units(
    file: &FileContext<'_>,
    parsed: &ParsedSource,
    spans: Option<&[LineRange]>,
    measure_untouched: bool,
    result: &mut ScanResult,
) -> Measured {
    let first = result.violations.len();
    let mut measured = Measured {
        units_of: Vec::new(),
        untouched: vec![false; parsed.functions.len()],
    };
    for (index, function) in parsed.functions.iter().enumerate() {
        let touched = spans.is_none_or(|ranges| touches(function, ranges));
        if !touched && !measure_untouched {
            continue;
        }
        add_violations(file, function, result);
        measured
            .units_of
            .resize(result.violations.len() - first, index);
        if touched {
            result.checked += 1;
            result
                .functions
                .push((file.display.to_path_buf(), function.clone()));
        } else {
            measured.untouched[index] = true;
        }
    }
    measured
}

/// Drops each violation of a unit that no hunk touches unless its status
/// is `new` or `worsened`, and counts the untouched units that keep one.
/// The file's violations start at `first` in `result`.
fn keep_untouched_changes(
    result: &mut ScanResult,
    first: usize,
    measured: &Measured,
    file: &FileContext<'_>,
    parsed: &ParsedSource,
) {
    let mut kept = BTreeSet::new();
    let violations = result.violations.split_off(first);
    for (violation, &index) in violations.into_iter().zip(&measured.units_of) {
        if measured.untouched[index] {
            let status = violation.baseline.as_ref().map(|baseline| baseline.status);
            if !matches!(status, Some(Status::New | Status::Worsened)) {
                continue;
            }
            kept.insert(index);
        }
        result.violations.push(violation);
    }
    for index in kept {
        result.checked += 1;
        result
            .functions
            .push((file.display.to_path_buf(), parsed.functions[index].clone()));
    }
}

fn file_config(path: &Path, options: &ScanOptions<'_>) -> Result<Config> {
    let resolution = match options.commit_configs {
        Some(configs) => load_config_at(path, configs)?,
        None => load_config(path, options.explicit_config)?,
    };
    Ok(resolution.config)
}

/// The working-tree file whose violations are compared with the base.
struct Current<'a> {
    source: &'a str,
    parsed: &'a ParsedSource,
    measured: &'a Measured,
}

/// The file's text and units at the merge base.
struct BaseUnits {
    source: String,
    parsed: ParsedSource,
}

/// Adds baseline fields to this file's violations, the last
/// `measured.units_of.len()` in `result`, where `units_of[i]` indexes the
/// violating unit in `parsed.functions`.
fn compare_with_base(
    changed: &ChangedFiles,
    file: &FileContext<'_>,
    current: &Current<'_>,
    result: &mut ScanResult,
) -> Result<()> {
    let functions = &current.parsed.functions;
    let units_of = &current.measured.units_of;
    let base = base_units(changed, file, result)?;
    let mut pairing = base.as_ref().map_or_else(
        || vec![Pairing::New; functions.len()],
        |base| baseline::pair_units(current.parsed, &base.parsed),
    );
    if let Some(base) = &base {
        pair_untouched_by_text(&mut pairing, current, base);
    }
    let first = result.violations.len() - units_of.len();
    for (violation, &index) in result.violations[first..].iter_mut().zip(units_of) {
        let base_unit = match pairing[index] {
            Pairing::Paired(base_index) => base
                .as_ref()
                .map(|units| &units.parsed.functions[base_index]),
            Pairing::New | Pairing::Unmatched => None,
        };
        violation.baseline = Some(Baseline {
            status: baseline::status(
                base_unit,
                pairing[index],
                Metric::parse(&violation.metric).expect("violations carry metric names"),
                violation.value,
            ),
            base_metrics: base_unit.map(MetricValues::from),
            limits: effective_limits(file, &functions[index]),
        });
    }
    Ok(())
}

/// A unit that no hunk touches is old when the base has a unit with its name
/// and its text. It pairs with that unit, whatever its position among the
/// units of that name, so a unit of the same name that the session adds
/// before it does not make it look new.
fn pair_untouched_by_text(pairing: &mut [Pairing], current: &Current<'_>, base: &BaseUnits) {
    for (index, unit) in current.parsed.functions.iter().enumerate() {
        if !current.measured.untouched[index] {
            continue;
        }
        let Some(text) = current.source.get(unit.span.0..unit.span.1) else {
            continue;
        };
        let same = base.parsed.functions.iter().position(|candidate| {
            candidate.function == unit.function
                && base.source.get(candidate.span.0..candidate.span.1) == Some(text)
        });
        if let Some(base_index) = same {
            pairing[index] = Pairing::Paired(base_index);
        }
    }
}

/// The file's text and units at the merge base, or `None` when it has no
/// base.
fn base_units(
    changed: &ChangedFiles,
    file: &FileContext<'_>,
    result: &mut ScanResult,
) -> Result<Option<BaseUnits>> {
    let Some(commit) = changed.base.as_deref() else {
        return Ok(None);
    };
    if changed.untracked.iter().any(|path| path == file.matched) {
        return Ok(None);
    }
    let path = changed
        .renames
        .get(file.matched)
        .map_or(file.matched, PathBuf::as_path);
    let content = match changed.base_blob(commit, path) {
        BaseBlob::Missing => return Ok(None),
        BaseBlob::Unreadable => None,
        BaseBlob::Content(bytes) => String::from_utf8(bytes).ok(),
    };
    let units = content.and_then(|source| {
        let language = Language::from_path(path)?;
        let parsed = parse_source(language, &source).ok()?;
        Some(BaseUnits { source, parsed })
    });
    if units.is_none() {
        result.notes.push(format!(
            "{} has no readable base; its functions count as new",
            file.display.display()
        ));
    }
    Ok(units)
}

fn effective_limits(file: &FileContext<'_>, function: &FunctionMetrics) -> Limits {
    let mut limits = file.config.limits_for(file.language.name());
    for metric in Metric::ALL {
        if !applies(file, function, metric) {
            *limits.get_mut(metric) = None;
        }
    }
    limits
}

/// Whether a metric's limit can fail this function.
fn applies(file: &FileContext<'_>, function: &FunctionMetrics, metric: Metric) -> bool {
    !file.test_exempt(metric) && metric.applies_to(function)
}

/// The effective limits of one file's units, resolved as `scan` resolves
/// them: the file's config chain (or `explicit_config`), its language
/// overrides, and test-file exemptions.
pub struct FileLimits {
    language: Language,
    config: Config,
    test_file: bool,
}

impl FileLimits {
    pub fn resolve(
        cwd: &Path,
        path: &Path,
        language: Language,
        explicit_config: Option<&Path>,
    ) -> Result<Self> {
        let file = resolve_path(cwd, path);
        let roots = std::slice::from_ref(&file);
        let options = ScanOptions {
            cwd,
            paths: roots,
            explicit_config,
            changed: None,
            commit_configs: None,
        };
        let matched = relative(&match_base(&options, roots)?, &file);
        let config = load_config(&file, explicit_config)?.config;
        let test_file = Config::matcher(&config.tests.patterns)?.is_match(&matched);
        Ok(Self {
            language,
            config,
            test_file,
        })
    }

    /// `None` where a check is off or does not apply to `unit`.
    pub fn for_unit(&self, unit: &FunctionMetrics) -> Limits {
        let file = FileContext {
            display: Path::new(""),
            matched: Path::new(""),
            language: self.language,
            config: &self.config,
            test_file: self.test_file,
        };
        effective_limits(&file, unit)
    }
}

fn changed_spans<'a>(path: &Path, changed: Option<&'a ChangedFiles>) -> Option<&'a [LineRange]> {
    let changed = changed?;
    if changed.fallback || changed.untracked.iter().any(|item| item == path) {
        return None;
    }
    changed.spans.get(path).map(Vec::as_slice)
}

fn touches(function: &FunctionMetrics, ranges: &[LineRange]) -> bool {
    ranges
        .iter()
        .any(|range| range.intersects(function.line, function.end_line))
}

fn add_violations(file: &FileContext<'_>, function: &FunctionMetrics, result: &mut ScanResult) {
    let limits = effective_limits(file, function);
    for metric in Metric::ALL {
        let value = function.value(metric);
        let Some(limit) = limits.get(metric).filter(|limit| value > *limit) else {
            continue;
        };
        result.violations.push(Violation {
            file: file.display.to_path_buf(),
            line: function.line,
            function: function.function.clone(),
            metric: metric.name().to_owned(),
            value,
            limit,
            baseline: None,
        });
    }
}

/// An explicit path as the file system resolves it: links in its directory
/// part are followed before `..` applies. A path whose directory does not
/// exist is normalized lexically first, then resolved the same way.
pub fn resolve_path(cwd: &Path, path: &Path) -> PathBuf {
    let path = cwd.join(path);
    resolved(&path).unwrap_or_else(|| {
        let lexical = normalize(&path);
        resolved(&lexical).unwrap_or(lexical)
    })
}

/// The physical spelling of `path`, or `None` when its directory does not
/// exist. A link to a file keeps its own name, which is the name Git lists
/// and the one that selects the grammar. Every other existing entry gets the
/// name the file system stores, so a Windows short name or another letter
/// case still matches the path Git prints.
fn resolved(path: &Path) -> Option<PathBuf> {
    let link = path
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink());
    if path.is_dir() || (!link && path.exists()) {
        return Some(physical(path));
    }
    let parent = path.parent().filter(|parent| parent.is_dir())?;
    Some(physical(parent).join(path.file_name()?))
}

fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                Some(Component::ParentDir) | None if !normalized.has_root() => {
                    normalized.push(component);
                }
                _ => {}
            },
            _ => normalized.push(component),
        }
    }
    normalized
}

fn relative(base: &Path, path: &Path) -> PathBuf {
    if let Ok(relative) = path.strip_prefix(base) {
        return relative.to_path_buf();
    }
    let base = base.components().collect::<Vec<_>>();
    let path = path.components().collect::<Vec<_>>();
    let shared = base
        .iter()
        .zip(&path)
        .take_while(|(left, right)| left == right)
        .count();
    std::iter::repeat_n(std::path::Component::ParentDir, base.len() - shared)
        .chain(path[shared..].iter().copied())
        .collect()
}

fn unverified_source_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            UNVERIFIED_SOURCE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
}

fn extension_reason(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .map_or_else(
            || "no grammar for extensionless file".to_owned(),
            |extension| format!("no grammar for .{extension}"),
        )
}

fn read_reason(error: &std::io::Error) -> String {
    if error.kind() == ErrorKind::InvalidData {
        "not valid UTF-8".to_owned()
    } else {
        format!("cannot read: {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_under_a_missing_directory_is_normalized_lexically() {
        let cwd = Path::new("/no-such-root/work");
        for (given, expected) in [
            ("gone/../a.js", "/no-such-root/work/a.js"),
            ("./gone/./b.js", "/no-such-root/work/gone/b.js"),
        ] {
            assert_eq!(resolve_path(cwd, Path::new(given)), Path::new(expected));
        }
        // `..` stops at the root, and stays when there is nothing to leave.
        for (given, expected) in [("/a/../../c.js", "/c.js"), ("../../d.js", "../../d.js")] {
            assert_eq!(normalize(Path::new(given)), Path::new(expected));
        }
    }

    #[test]
    fn violations_skip_metrics_that_do_not_apply_to_the_unit() {
        let config: Config =
            serde_json::from_str(include_str!("../../../config.default.json")).unwrap();
        let file = FileContext {
            display: Path::new("page.svelte"),
            matched: Path::new("page.svelte"),
            language: Language::Svelte,
            config: &config,
            test_file: false,
        };
        let unit = FunctionMetrics {
            function: "<template>".to_owned(),
            line: 1,
            end_line: 1,
            complexity: 0,
            cognitive: 0,
            depth: 0,
            lines: 500,
            params: 50,
            bool_ops: 0,
            widget_depth: 50,
            span: (0, 1),
            template: true,
            widget: false,
        };
        let mut result = ScanResult::default();
        add_violations(&file, &unit, &mut result);
        assert_eq!(result.violations, []);

        let unit = FunctionMetrics {
            template: false,
            ..unit
        };
        add_violations(&file, &unit, &mut result);
        let metrics = result
            .violations
            .iter()
            .map(|violation| violation.metric.as_str())
            .collect::<Vec<_>>();
        assert_eq!(metrics, ["lines", "params"]);

        let test_file = FileContext {
            test_file: true,
            ..file
        };
        let mut result = ScanResult::default();
        add_violations(&test_file, &unit, &mut result);
        assert_eq!(result.violations.len(), 1);
        assert_eq!(result.violations[0].metric, "params");
    }
}
