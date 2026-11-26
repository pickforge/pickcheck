//! Where a unit's `cognitive` score comes from. The increments come from the
//! same walk that produces the score, which pushes them into a sink that
//! `trace_source` passes down.

use anyhow::{Context, Result};

use crate::{
    FunctionMetrics, Language,
    cognitive::Increment,
    language::{TracedUnit, parse_tree, parse_units},
};

/// One unit of a file with its metrics and its cognitive increments in
/// source order. The increments sum to `metrics.cognitive`.
#[derive(Clone, Debug)]
pub struct UnitTrace {
    pub metrics: FunctionMetrics,
    pub increments: Vec<Increment>,
}

/// A unit that `parse_source` left out, with its first and last file line
/// and its byte span in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeftOutLines {
    pub function: String,
    pub line: usize,
    pub end_line: usize,
    pub span: (usize, usize),
}

/// The measured units of a file with their increments, and what `check`
/// reports as `UNVERIFIED` about the rest.
#[derive(Clone, Debug, Default)]
pub struct SourceTrace {
    pub units: Vec<UnitTrace>,
    /// Units left out, in source order. A unit left out has no increments.
    pub left_out: Vec<LeftOutLines>,
    /// The `UNVERIFIED` reason, when units were left out or a Svelte script
    /// block could not be parsed.
    pub reason: Option<String>,
    /// The first and last line of each top-level Svelte `<script>` and
    /// `<style>` element, which no template unit measures.
    pub foreign: Vec<(usize, usize)>,
}

/// Every unit of `source` that `parse_source` measures, with its increments.
///
/// # Errors
///
/// Fails as `parse_source` does.
pub fn trace_source(language: Language, source: &str) -> Result<SourceTrace> {
    let mut traced = Vec::new();
    let parsed = parse_units(language, source, Some(&mut traced))?;
    let reason = parsed.reason();
    let line_of = |byte: usize| source[..byte].matches('\n').count() + 1;
    let left_out = parsed
        .left_out
        .iter()
        .map(|unit| LeftOutLines {
            function: unit.function.clone(),
            line: line_of(unit.span.0),
            end_line: line_of(unit.span.1),
            span: unit.span,
        })
        .collect();
    let units = parsed
        .functions
        .into_iter()
        .map(|metrics| {
            let index = traced
                .iter()
                .position(|unit| unit.span == metrics.span && unit.function == metrics.function)
                .with_context(|| {
                    format!(
                        "no cognitive trace for {} at line {}",
                        metrics.function, metrics.line
                    )
                })?;
            let TracedUnit { mut increments, .. } = traced.swap_remove(index);
            increments.sort_by_key(|increment| (increment.line, increment.column));
            Ok(UnitTrace {
                metrics,
                increments,
            })
        })
        .collect::<Result<_>>()?;
    Ok(SourceTrace {
        units,
        left_out,
        reason,
        foreign: foreign_lines(language, source)?,
    })
}

fn foreign_lines(language: Language, source: &str) -> Result<Vec<(usize, usize)>> {
    if language != Language::Svelte {
        return Ok(Vec::new());
    }
    let tree = parse_tree(&tree_sitter_svelte_ng::LANGUAGE.into(), source)?;
    let root = tree.root_node();
    let mut cursor = root.walk();
    Ok(root
        .named_children(&mut cursor)
        .filter(|node| matches!(node.kind(), "script_element" | "style_element"))
        .map(|node| (node.start_position().row + 1, node.end_position().row + 1))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worked_example_matches_the_spec() {
        let source = "\
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
        let units = trace_source(Language::JavaScript, source).unwrap().units;
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].metrics.function, "visit");
        assert_eq!(units[0].metrics.cognitive, 12);
        let increments = units[0]
            .increments
            .iter()
            .map(|item| (item.line, item.add, item.construct, item.nesting))
            .collect::<Vec<_>>();
        assert_eq!(
            increments,
            [
                (2, 1, "for", Some(0)),
                (3, 2, "if", Some(1)),
                (3, 1, "sequence &&", None),
                (5, 1, "else if", None),
                (6, 3, "catch", Some(2)),
                (7, 4, "ternary", Some(3)),
            ]
        );
    }

    #[test]
    fn svelte_lines_are_file_lines() {
        let source = "\
<p>intro</p>
<script>
  function pick(a, b) {
    return a && b;
  }
</script>
{#if ready}
  <p>{a ? b : c}</p>
{:else}
  <p>{x ||
    y}</p>
{/if}
";
        let units = trace_source(Language::Svelte, source).unwrap().units;
        let find = |name: &str| {
            units
                .iter()
                .find(|unit| unit.metrics.function == name)
                .unwrap()
        };
        let lines = |unit: &UnitTrace| {
            unit.increments
                .iter()
                .map(|item| (item.line, item.construct))
                .collect::<Vec<_>>()
        };
        assert_eq!(lines(find("pick")), [(4, "sequence &&")]);
        assert_eq!(
            lines(find("{#if}")),
            [
                (7, "{#if}"),
                (8, "ternary"),
                (9, "{:else}"),
                (10, "sequence ||")
            ]
        );
        for unit in &units {
            let sum = unit.increments.iter().map(|item| item.add).sum::<usize>();
            assert_eq!(sum, unit.metrics.cognitive, "{}", unit.metrics.function);
        }
    }

    #[test]
    fn svelte_positions_are_file_lines_and_columns() {
        let source = "\
<script lang=\"ts\">function f(a: boolean, b: boolean) { return a && b; }</script>
{#if ready}
  <p>{a
    ? b
    : c && d}</p>
  <p>{e || f} {g ? h : i}</p>
  <p>{
    j ?? k}</p>
{/if}
";
        let units = trace_source(Language::Svelte, source).unwrap().units;
        let positions = |name: &str| {
            let unit = units
                .iter()
                .find(|unit| unit.metrics.function == name)
                .unwrap();
            let sum = unit.increments.iter().map(|item| item.add).sum::<usize>();
            assert_eq!(sum, unit.metrics.cognitive, "{name}");
            unit.increments
                .iter()
                .map(|item| (item.line, item.column, item.construct))
                .collect::<Vec<_>>()
        };
        assert_eq!(positions("f"), [(1, 65, "sequence &&")]);
        assert_eq!(
            positions("{#if}"),
            [
                (2, 1, "{#if}"),
                (3, 7, "ternary"),
                (5, 9, "sequence &&"),
                (6, 9, "sequence ||"),
                (6, 16, "ternary"),
                (8, 7, "sequence ??"),
            ]
        );
    }

    #[test]
    fn the_expression_wrapper_prefix_is_one_line() {
        let (wrapped, range) = crate::language::Wrapper::Expression.wrap("");
        assert!(!wrapped[..range.start].contains('\n'));
    }
}
