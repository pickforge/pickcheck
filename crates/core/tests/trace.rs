use std::{fs, path::Path};

use pickcheck_core::{Language, parse_source, trace_source};

/// Every unit of every golden fixture: the trace lists the same units as
/// `parse_source`, and its increments sum to the unit's `cognitive` and sit
/// inside the unit.
#[test]
fn golden_increments_sum_to_cognitive() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut checked = 0;
    for directory in fs::read_dir(root).unwrap() {
        let directory = directory.unwrap().path();
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let Some(language) = Language::from_path(&path) else {
                continue;
            };
            let source = fs::read_to_string(&path).unwrap();
            let units = parse_source(language, &source).unwrap();
            let traces = trace_source(language, &source).unwrap();
            assert_eq!(traces.len(), units.len(), "{}", path.display());
            for (trace, unit) in traces.iter().zip(&units) {
                let name = format!("{}:{} {}", path.display(), unit.line, unit.function);
                assert_eq!(&trace.metrics, unit, "{name}");
                let sum = trace.increments.iter().map(|item| item.add).sum::<usize>();
                assert_eq!(sum, unit.cognitive, "{name}");
                assert!(
                    trace
                        .increments
                        .iter()
                        .all(|item| (unit.line..=unit.end_line).contains(&item.line)),
                    "{name}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 0);
}
