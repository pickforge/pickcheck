#![deny(clippy::cognitive_complexity, clippy::too_many_lines)]

pub mod baseline;
mod cognitive;
pub mod config;
pub mod diff;
pub mod language;
mod metric;
pub mod scan;

pub use baseline::{MetricValues, Pairing, Status, pair_units};
pub use config::{
    CommitConfigs, Config, ConfigResolution, HookConfig, Limits, load_config, load_config_at,
};
pub use diff::{
    BaseBlob, ChangedFiles, DiffStatus, LineRange, Pathspec, Repository, base_blob, changed_files,
    changed_files_since, diff_spans, diff_status, is_plainly_tracked, merge_base_with_head,
    parse_diff_hunks, physical, repository, unmerged_files, untracked_files,
};
pub use language::{
    FunctionMetrics, Language, LeftOut, MAX_SYNTAX_DEPTH, ParsedSource, Unmeasured,
    coverage_unknowns, grammar_inventory, on_measurement_stack, parse_source,
};
pub use metric::Metric;
pub use scan::{
    Baseline, ScanOptions, ScanResult, Unverified, Violation, config_changed, resolve_path, scan,
};
