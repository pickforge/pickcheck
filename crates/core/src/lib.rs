#![deny(clippy::cognitive_complexity, clippy::too_many_lines)]

pub mod baseline;
mod cognitive;
pub mod config;
pub mod diff;
pub mod language;
mod metric;
pub mod scan;

pub use baseline::{MetricValues, Pairing, Status, pair_units};
pub use config::{Config, ConfigResolution, HookConfig, Limits, load_config, load_config_at};
pub use diff::{
    BaseBlob, ChangedFiles, LineRange, UnusableBase, base_blob, changed_files, changed_files_since,
    head_commit, parse_diff_hunks, physical,
};
pub use language::{
    FunctionMetrics, Language, MAX_SYNTAX_DEPTH, TooDeep, coverage_unknowns, grammar_inventory,
    on_measurement_stack, parse_source,
};
pub use metric::Metric;
pub use scan::{
    Baseline, ScanOptions, ScanResult, Unverified, Violation, config_changed, resolve_path, scan,
};
