//! Where a unit's `cognitive` score comes from. The increments come from the
//! same walk that produces the score, which pushes them into a sink that
//! `trace_source` passes down.

use anyhow::{Context, Result};

use crate::{
    FunctionMetrics, Language,
    cognitive::Increment,
    language::{TracedUnit, parse_units},
};

/// One unit of a file with its metrics and its cognitive increments in
/// source order. The increments sum to `metrics.cognitive`.
#[derive(Clone, Debug)]
pub struct UnitTrace {
    pub metrics: FunctionMetrics,
    pub increments: Vec<Increment>,
}

/// Every unit of `source`, as `parse_source` reports it, with its increments.
pub fn trace_source(language: Language, source: &str) -> Result<Vec<UnitTrace>> {
    let mut traced = Vec::new();
    let units = parse_units(language, source, Some(&mut traced))?;
    units
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
            increments.sort_by_key(|increment| increment.line);
            Ok(UnitTrace {
                metrics,
                increments,
            })
        })
        .collect()
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
        let units = trace_source(Language::JavaScript, source).unwrap();
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
        let units = trace_source(Language::Svelte, source).unwrap();
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
    fn svelte_expression_lines_follow_the_raw_text() {
        let source = "\
<script>
  let ready = true;
</script>
{#if ready}
  <p>{a
    ? b
    : c && d}</p>
  <p>{e || f} {g ? h : i}</p>
  <p>{
    j ?? k}</p>
{/if}
";
        let units = trace_source(Language::Svelte, source).unwrap();
        let unit = units
            .iter()
            .find(|unit| unit.metrics.function == "{#if}")
            .unwrap();
        let increments = unit
            .increments
            .iter()
            .map(|item| (item.line, item.construct))
            .collect::<Vec<_>>();
        assert_eq!(
            increments,
            [
                (4, "{#if}"),
                (5, "ternary"),
                (7, "sequence &&"),
                (8, "sequence ||"),
                (8, "ternary"),
                (10, "sequence ??"),
            ]
        );
        let sum = unit.increments.iter().map(|item| item.add).sum::<usize>();
        assert_eq!(sum, unit.metrics.cognitive);
    }

    #[test]
    fn the_expression_wrapper_prefix_is_one_line() {
        assert!(!crate::cognitive::wrap_expression("").contains('\n'));
    }
}
