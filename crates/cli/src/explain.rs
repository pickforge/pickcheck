//! `pickcheck explain`: one line per cognitive increment of the selected
//! units. It reports and never gates.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use pickcheck_core::{
    FileLimits, Increment, Language, LeftOutLines, Limits, MetricValues, SourceTrace, UnitTrace,
    Unmeasured, trace_source,
};
use serde::Serialize;

use crate::Format;

/// A unit with its effective limits.
struct Explained {
    trace: UnitTrace,
    limits: Limits,
}

pub(crate) fn run(
    target: &str,
    function: Option<&str>,
    format: Format,
    config: Option<&Path>,
) -> Result<u8> {
    let cwd = env::current_dir().context("cannot determine current directory")?;
    let (path, line) = parse_target(target, &cwd)?;
    if line.is_some() && function.is_some() {
        anyhow::bail!("use <file>:<line> or --function, not both");
    }
    let file = cwd.join(&path);
    let language =
        Language::from_path(&file).with_context(|| format!("no grammar for {}", path.display()))?;
    let source =
        fs::read_to_string(&file).with_context(|| format!("cannot read {}", path.display()))?;
    let limits = FileLimits::resolve(&cwd, &path, language, config)?;
    let mut traced = trace_source(language, &source)
        .with_context(|| format!("cannot explain {}", path.display()))?;
    if let Some(reason) = &traced.reason
        && traced.units.is_empty()
    {
        anyhow::bail!("cannot explain {}: {reason}", path.display());
    }
    let mut units = std::mem::take(&mut traced.units)
        .into_iter()
        .map(|trace| Explained {
            limits: limits.for_unit(&trace.metrics),
            trace,
        })
        .collect::<Vec<_>>();
    // Source order, also for units that start on one line.
    units.sort_by_key(|unit| unit.trace.metrics.span.0);
    let selected = select(&units, &traced, line, function, &path)?;
    if let Some(note) = left_out_note(&traced, &path) {
        eprintln!("{note}");
    }
    if selected.is_empty() && format == Format::Text {
        eprintln!("no unit in {} exceeds the cognitive limit", path.display());
        return Ok(0);
    }
    match format {
        Format::Text => print!("{}", text(&path, &selected)),
        Format::Json => println!("{}", json(&path, &selected)?),
    }
    Ok(0)
}

/// The one `note:` line for a file that `check` reports `UNVERIFIED`.
fn left_out_note(traced: &SourceTrace, path: &Path) -> Option<String> {
    let reason = traced.reason.as_ref()?;
    let units = traced
        .left_out
        .iter()
        .map(|unit| format!("{} at line {}", unit.function, unit.line))
        .collect::<Vec<_>>();
    let units = if units.is_empty() {
        String::new()
    } else {
        format!(", left out {}", units.join(", "))
    };
    Some(format!(
        "note: {} is UNVERIFIED: {reason}{units}",
        path.display()
    ))
}

/// Splits `<file>[:<line>[:<column>]]`; the column is ignored. An argument
/// that names an existing file is the file, even when it ends in `:<digits>`.
fn parse_target(target: &str, cwd: &Path) -> Result<(PathBuf, Option<usize>)> {
    let whole = || Ok((PathBuf::from(target), None));
    if cwd.join(target).is_file() {
        return whole();
    }
    let Some((mut file, mut line)) = target.rsplit_once(':') else {
        return whole();
    };
    let is_digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let digits = is_digits(line);
    if let Some((head, row)) = file.rsplit_once(':')
        && digits
        && is_digits(row)
        && !cwd.join(file).is_file()
    {
        (file, line) = (head, row);
    }
    if !digits && !cwd.join(file).is_file() {
        return whole();
    }
    match line.parse::<usize>() {
        Ok(line) if digits && line > 0 => Ok((PathBuf::from(file), Some(line))),
        _ => anyhow::bail!("malformed target '{target}'; use <file>:<line> with a line from 1"),
    }
}

fn select<'a>(
    units: &'a [Explained],
    traced: &SourceTrace,
    line: Option<usize>,
    function: Option<&str>,
    path: &Path,
) -> Result<Vec<&'a Explained>> {
    if let Some(name) = function {
        let named = units
            .iter()
            .filter(|unit| unit.trace.metrics.function == name)
            .collect::<Vec<_>>();
        if named.is_empty() {
            if traced.left_out.iter().any(|unit| unit.function == name) {
                anyhow::bail!(
                    "cannot explain {name} in {}: {}",
                    path.display(),
                    Unmeasured::TooDeep
                );
            }
            anyhow::bail!("no unit named {name} in {}", path.display());
        }
        return Ok(named);
    }
    let Some(line) = line else {
        return Ok(units.iter().filter(|unit| over_limit(unit)).collect());
    };
    let starting = units
        .iter()
        .filter(|unit| unit.trace.metrics.line == line)
        .collect::<Vec<_>>();
    if !starting.is_empty() {
        return Ok(starting);
    }
    let inner = units
        .iter()
        .filter(|unit| (unit.trace.metrics.line..=unit.trace.metrics.end_line).contains(&line))
        .min_by_key(|unit| unit.trace.metrics.span.1 - unit.trace.metrics.span.0);
    // A unit left out wins when it sits inside the innermost measured unit.
    let within = |unit: &&LeftOutLines| {
        (unit.line..=unit.end_line).contains(&line)
            && inner.is_none_or(|inner| {
                let outer = inner.trace.metrics.span;
                outer.0 <= unit.span.0 && unit.span.1 <= outer.1
            })
    };
    if let Some(unit) = traced.left_out.iter().find(within) {
        anyhow::bail!(
            "cannot explain {}:{line}: {} at line {} is left out: {}",
            path.display(),
            unit.function,
            unit.line,
            Unmeasured::TooDeep
        );
    }
    // A Svelte `<script>` or `<style>` line belongs to no template unit.
    let foreign = |unit: &&Explained| {
        traced
            .foreign
            .iter()
            .any(|&(start, end)| unit.trace.metrics.template && (start..=end).contains(&line))
    };
    inner
        .filter(|unit| !foreign(unit))
        .map(|unit| vec![unit])
        .with_context(|| format!("no unit in {} contains line {line}", path.display()))
}

fn over_limit(unit: &Explained) -> bool {
    unit.limits
        .cognitive
        .is_some_and(|limit| unit.trace.metrics.cognitive > limit)
}

fn text(path: &Path, units: &[&Explained]) -> String {
    let blocks = units.iter().map(|unit| {
        let metrics = &unit.trace.metrics;
        let verdict = match unit.limits.cognitive {
            Some(limit) if metrics.cognitive > limit => format!(" > {limit}"),
            Some(limit) => format!(" <= {limit}"),
            None => String::new(),
        };
        let mut lines = vec![format!(
            "{}:{} {}  cognitive {}{verdict}",
            path.display(),
            metrics.line,
            metrics.function,
            metrics.cognitive
        )];
        lines.extend(increment_rows(&unit.trace.increments));
        lines.join("\n") + "\n"
    });
    blocks.collect::<Vec<_>>().join("\n")
}

/// Aligned rows: line, `+add`, construct, and nesting when it applies.
fn increment_rows(increments: &[Increment]) -> Vec<String> {
    let width = |values: Vec<usize>| values.into_iter().max().unwrap_or(0);
    let line_width = width(
        increments
            .iter()
            .map(|item| item.line.to_string().len())
            .collect(),
    );
    let add_width = width(
        increments
            .iter()
            .map(|item| item.add.to_string().len() + 1)
            .collect(),
    );
    let construct_width = width(increments.iter().map(|item| item.construct.len()).collect());
    increments
        .iter()
        .map(|item| {
            let nesting = item
                .nesting
                .map_or_else(String::new, |level| format!("nesting {level}"));
            let row = format!(
                "  {:>line_width$}  {:<add_width$}  {:<construct_width$}  {nesting}",
                item.line,
                format!("+{}", item.add),
                item.construct
            );
            row.trim_end().to_owned()
        })
        .collect()
}

#[derive(Serialize)]
struct JsonExplain<'a> {
    version: &'static str,
    file: String,
    units: Vec<JsonUnit<'a>>,
}

#[derive(Serialize)]
struct JsonUnit<'a> {
    function: &'a str,
    line: usize,
    cognitive: usize,
    limit: Option<usize>,
    metrics: MetricValues,
    limits: &'a Limits,
    increments: &'a [Increment],
}

fn json(path: &Path, units: &[&Explained]) -> Result<String> {
    let report = JsonExplain {
        version: env!("CARGO_PKG_VERSION"),
        file: path.display().to_string(),
        units: units
            .iter()
            .map(|unit| JsonUnit {
                function: &unit.trace.metrics.function,
                line: unit.trace.metrics.line,
                cognitive: unit.trace.metrics.cognitive,
                limit: unit.limits.cognitive,
                metrics: MetricValues::from(&unit.trace.metrics),
                limits: &unit.limits,
                increments: &unit.trace.increments,
            })
            .collect(),
    };
    Ok(serde_json::to_string_pretty(&report)?)
}
