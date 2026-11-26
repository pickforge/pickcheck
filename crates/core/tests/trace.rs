use std::{fs, path::Path};

use pickcheck_core::{Language, UnitTrace, parse_source, trace_source};
use serde::Deserialize;

#[derive(Deserialize)]
struct Golden {
    functions: Vec<Expected>,
}

/// The part of an `expected.json` entry that names a unit and its score.
#[derive(Deserialize)]
struct Expected {
    function: String,
    line: usize,
    cognitive: usize,
}

/// Every unit of every golden fixture: the trace lists the units and metrics
/// that `parse_source` and `expected.json` give, its increments sum to the
/// unit's `cognitive`, and each increment points at a token inside the unit.
#[test]
fn golden_increments_sum_to_cognitive() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut checked = 0;
    for directory in fs::read_dir(root).unwrap() {
        let directory = directory.unwrap().path();
        let expected = fs::read_to_string(directory.join("expected.json")).unwrap();
        let expected = serde_json::from_str::<Golden>(&expected).unwrap().functions;
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let Some(language) = Language::from_path(&path) else {
                continue;
            };
            let source = fs::read_to_string(&path).unwrap();
            let parsed = parse_source(language, &source).unwrap();
            assert!(parsed.left_out.is_empty(), "{}", path.display());
            let traces = trace_source(language, &source).unwrap().units;
            assert_eq!(traces.len(), parsed.functions.len(), "{}", path.display());
            assert_eq!(traces.len(), expected.len(), "{}", path.display());
            let units = parsed.functions.iter().zip(&expected);
            for (trace, (unit, golden)) in traces.iter().zip(units) {
                let name = format!("{}:{} {}", path.display(), unit.line, unit.function);
                assert_eq!(&trace.metrics, unit, "{name}");
                let key = (golden.function.as_str(), golden.line, golden.cognitive);
                let metrics = &trace.metrics;
                let traced = (metrics.function.as_str(), metrics.line, metrics.cognitive);
                assert_eq!(traced, key, "{name}");
                let sum = trace.increments.iter().map(|item| item.add).sum::<usize>();
                assert_eq!(sum, unit.cognitive, "{name}");
                assert_points_into_unit(&source, trace, &name);
                checked += 1;
            }
        }
    }
    assert!(checked > 0);
}

/// Each increment's line and column name a byte inside the unit's span that
/// starts a token, so no increment points into a wrapper or between tokens.
fn assert_points_into_unit(source: &str, trace: &UnitTrace, name: &str) {
    let line_starts = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(index, _)| index + 1))
        .collect::<Vec<_>>();
    for item in &trace.increments {
        let at = line_starts[item.line - 1] + item.column - 1;
        let (start, end) = trace.metrics.span;
        assert!((start..end).contains(&at), "{name}: {item:?}");
        let rest = &source[at..];
        assert!(
            rest.starts_with(|c: char| !c.is_whitespace()),
            "{name}: {item:?}"
        );
    }
}

/// The Svelte role cases at the end of the golden fixture: the trace finds
/// the same units, and each increment sits at the opening tag of its block.
#[test]
fn svelte_role_wrapper_units_trace_at_their_tags() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/svelte/fixture.svelte");
    let source = fs::read_to_string(path).unwrap();
    let traces = trace_source(Language::Svelte, &source).unwrap().units;
    let rows = traces
        .iter()
        .filter(|trace| (272..=277).contains(&trace.metrics.line))
        .map(|trace| {
            let increments = trace
                .increments
                .iter()
                .map(|item| (item.line, item.column, item.add, item.construct))
                .collect::<Vec<_>>();
            (trace.metrics.function.as_str(), increments)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            ("{#snippet pick}", vec![]),
            ("{#if}", vec![(273, 1, 1, "{#if}")]),
            ("{#each}", vec![(274, 1, 1, "{#each}")]),
            ("{#await}", vec![(275, 1, 1, "{#await}")]),
            ("{#snippet hint}", vec![]),
        ]
    );
}
