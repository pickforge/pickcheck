//! Pairs each unit with the same unit at the merge base and decides the
//! status of a violation, as "Baseline (`--base`)" in `docs/spec.md` defines.

use std::{
    borrow::Cow,
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
};

use serde::{Deserialize, Serialize};

use crate::{FunctionMetrics, Metric, ParsedSource};

const ANONYMOUS: &str = "<anonymous>";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    New,
    Worsened,
    Unmatched,
    Improved,
    Unchanged,
}

impl Status {
    pub const ALL: [Self; 5] = [
        Self::New,
        Self::Worsened,
        Self::Unmatched,
        Self::Improved,
        Self::Unchanged,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Worsened => "worsened",
            Self::Unmatched => "unmatched",
            Self::Improved => "improved",
            Self::Unchanged => "unchanged",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.name() == name)
    }
}

/// How a current unit relates to the base file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pairing {
    Paired(usize),
    New,
    Unmatched,
}

/// Every metric's value in one unit, as JSON `base_metrics` reports it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MetricValues {
    pub complexity: usize,
    pub cognitive: usize,
    pub depth: usize,
    pub lines: usize,
    pub params: usize,
    pub bool_ops: usize,
    pub widget_depth: usize,
}

impl MetricValues {
    pub fn get(&self, name: &str) -> Option<usize> {
        Metric::parse(name).map(|metric| self.value(metric))
    }

    pub fn value(&self, metric: Metric) -> usize {
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

impl From<&FunctionMetrics> for MetricValues {
    fn from(unit: &FunctionMetrics) -> Self {
        Self {
            complexity: unit.value(Metric::Complexity),
            cognitive: unit.value(Metric::Cognitive),
            depth: unit.value(Metric::Depth),
            lines: unit.value(Metric::Lines),
            params: unit.value(Metric::Params),
            bool_ops: unit.value(Metric::BoolOps),
            widget_depth: unit.value(Metric::WidgetDepth),
        }
    }
}

/// Pairs every measured unit of a complete current file with a measured unit
/// of the complete base file. Units left out at either revision take part
/// with their name and span, so they keep their place among same-named units
/// and still own their closures; a unit paired with a left-out base unit is
/// new.
pub fn pair_units(current: &ParsedSource, base: &ParsedSource) -> Vec<Pairing> {
    pair_with_claims(current, base, &BTreeMap::new())
}

/// A file's text with its units.
#[derive(Clone, Copy)]
pub struct Text<'a> {
    pub source: &'a str,
    pub parsed: &'a ParsedSource,
}

/// Pairs like `pair_units`, but each named unit marked in `untouched` first
/// claims a base unit with its name and its text, one base unit per claim.
/// Texts that differ only in CRLF against LF line ends are the same text,
/// because a checkout can convert the line ends. The other units of that name then pair in source order with the base
/// units that no claim took. So a unit of the same name that is added before
/// an old unit is the new one, and the old unit keeps its own base.
pub fn pair_units_by_text(current: Text<'_>, base: Text<'_>, untouched: &[bool]) -> Vec<Pairing> {
    if !untouched.contains(&true) {
        return pair_units(current.parsed, base.parsed);
    }
    // Base units by name and text, the first in the file last, so `pop`
    // takes them in file order.
    let mut free = BTreeMap::<(&str, Cow<'_, str>), Vec<usize>>::new();
    for (index, unit) in base.parsed.functions.iter().enumerate().rev() {
        if let Some(text) = unit_text(base.source, unit) {
            free.entry((&unit.function, text)).or_default().push(index);
        }
    }
    let mut claims = BTreeMap::new();
    for (index, unit) in current.parsed.functions.iter().enumerate() {
        if untouched.get(index) != Some(&true) || unit.function == ANONYMOUS {
            continue;
        }
        let claimed = unit_text(current.source, unit)
            .and_then(|text| free.get_mut(&(unit.function.as_str(), text))?.pop());
        if let Some(base_index) = claimed {
            claims.insert(index, base_index);
        }
    }
    pair_with_claims(current.parsed, base.parsed, &claims)
}

/// The text of a unit with LF line ends.
fn unit_text<'a>(source: &'a str, unit: &FunctionMetrics) -> Option<Cow<'a, str>> {
    let text = source.get(unit.span.0..unit.span.1)?;
    Some(if text.contains("\r\n") {
        Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(text)
    })
}

/// `claims` maps a measured current unit to the measured base unit it pairs
/// with before the pairing by order.
fn pair_with_claims(
    current: &ParsedSource,
    base: &ParsedSource,
    claims: &BTreeMap<usize, usize>,
) -> Vec<Pairing> {
    let (current_keys, base_keys) = (keys(current), keys(base));
    let mut pairing = pair_named(&current_keys, &base_keys, claims);
    pair_anonymous(&current_keys, &base_keys, &mut pairing);
    let measured = base.functions.len();
    pairing
        .into_iter()
        .take(current.functions.len())
        .map(|item| match item {
            Pairing::Paired(index) if index >= measured => Pairing::New,
            other => other,
        })
        .collect()
}

fn pair_named(
    current: &[Key<'_>],
    base: &[Key<'_>],
    claims: &BTreeMap<usize, usize>,
) -> Vec<Pairing> {
    let mut pairing = vec![Pairing::New; current.len()];
    let taken = claims.values().collect::<BTreeSet<_>>();
    let base_names = named_occurrences(base);
    for (name, indices) in named_occurrences(current) {
        let base_indices = base_names.get(name).map_or(&[][..], Vec::as_slice);
        let unclaimed = indices.iter().filter(|index| !claims.contains_key(index));
        let free = base_indices.iter().filter(|index| !taken.contains(index));
        for (&index, &base_index) in unclaimed.zip(free) {
            pairing[index] = Pairing::Paired(base_index);
        }
    }
    for (&index, &base_index) in claims {
        pairing[index] = Pairing::Paired(base_index);
    }
    pairing
}

fn pair_anonymous(current: &[Key<'_>], base: &[Key<'_>], pairing: &mut [Pairing]) {
    let base_groups = anonymous_groups(base);
    for (owner, members) in anonymous_groups(current) {
        let base_owner = match owner {
            None => None,
            Some(index) => match pairing[index] {
                Pairing::Paired(base_index) => Some(base_index),
                Pairing::New | Pairing::Unmatched => continue,
            },
        };
        let base_members = base_groups.get(&base_owner).map_or(&[][..], Vec::as_slice);
        for (position, &index) in members.iter().enumerate() {
            pairing[index] = if base_members.len() == members.len() {
                Pairing::Paired(base_members[position])
            } else {
                Pairing::Unmatched
            };
        }
    }
}

/// What matching reads of a unit, measured or left out.
struct Key<'a> {
    name: &'a str,
    span: (usize, usize),
    template: bool,
}

/// Measured units first, so a measured unit's key has the unit's index.
fn keys(parsed: &ParsedSource) -> Vec<Key<'_>> {
    let measured = parsed.functions.iter().map(|unit| Key {
        name: &unit.function,
        span: unit.span,
        template: unit.template,
    });
    let left_out = parsed.left_out.iter().map(|unit| Key {
        name: &unit.function,
        span: unit.span,
        template: unit.template,
    });
    measured.chain(left_out).collect()
}

/// The status of one violation of a unit, given the unit's base match.
pub fn status(
    base: Option<&FunctionMetrics>,
    pairing: Pairing,
    metric: Metric,
    value: usize,
) -> Status {
    let base_value = match (pairing, base) {
        (Pairing::Paired(_), Some(unit)) => Some(unit.value(metric)),
        (Pairing::Unmatched, _) => return Status::Unmatched,
        _ => None,
    };
    match base_value.map(|base_value| value.cmp(&base_value)) {
        None => Status::New,
        Some(std::cmp::Ordering::Greater) => Status::Worsened,
        Some(std::cmp::Ordering::Less) => Status::Improved,
        Some(std::cmp::Ordering::Equal) => Status::Unchanged,
    }
}

/// Unit indices in source order, outer units before the units they contain.
fn source_order(units: &[Key<'_>]) -> Vec<usize> {
    let mut order = (0..units.len()).collect::<Vec<_>>();
    order.sort_by_key(|&index| (units[index].span.0, Reverse(units[index].span.1)));
    order
}

fn named_occurrences<'a>(units: &[Key<'a>]) -> BTreeMap<&'a str, Vec<usize>> {
    let mut names = BTreeMap::<&str, Vec<usize>>::new();
    for index in source_order(units) {
        if units[index].name != ANONYMOUS {
            names.entry(units[index].name).or_default().push(index);
        }
    }
    names
}

/// Anonymous units by owner: the innermost named unit containing them, or
/// `None` for the file.
fn anonymous_groups(units: &[Key<'_>]) -> BTreeMap<Option<usize>, Vec<usize>> {
    let mut groups = BTreeMap::<Option<usize>, Vec<usize>>::new();
    for index in source_order(units) {
        if units[index].name == ANONYMOUS {
            groups.entry(owner(units, index)).or_default().push(index);
        }
    }
    groups
}

fn owner(units: &[Key<'_>], index: usize) -> Option<usize> {
    let (start, end) = units[index].span;
    units
        .iter()
        .enumerate()
        .filter(|(candidate, unit)| *candidate != index && can_own(unit))
        .filter(|(_, unit)| unit.span.0 <= start && end <= unit.span.1)
        .min_by_key(|(_, unit)| unit.span.1 - unit.span.0)
        .map(|(candidate, _)| candidate)
}

/// Svelte template units never own script closures: the synthetic
/// `<template>` spans the whole file, which would make a `.js` file renamed to
/// `.svelte` give its closures a new owner.
fn can_own(unit: &Key<'_>) -> bool {
    !unit.template && unit.name != ANONYMOUS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LeftOut;

    fn unit(function: &str, span: (usize, usize), cognitive: usize) -> FunctionMetrics {
        FunctionMetrics {
            function: function.to_owned(),
            line: span.0,
            end_line: span.1,
            complexity: 1,
            cognitive,
            depth: 0,
            lines: 1,
            params: 0,
            bool_ops: 0,
            widget_depth: 0,
            span,
            template: false,
            widget: false,
        }
    }

    fn parsed(units: &[FunctionMetrics]) -> ParsedSource {
        ParsedSource {
            functions: units.to_vec(),
            ..ParsedSource::default()
        }
    }

    fn left_out(function: &str, span: (usize, usize)) -> LeftOut {
        LeftOut {
            function: function.to_owned(),
            span,
            template: false,
        }
    }

    #[test]
    fn named_units_pair_by_name_and_duplicates_in_source_order() {
        let base = [
            unit("a", (0, 10), 0),
            unit("dup", (20, 30), 0),
            unit("dup", (40, 50), 0),
        ];
        let current = [
            unit("dup", (0, 5), 0),
            unit("renamed", (10, 20), 0),
            unit("dup", (30, 40), 0),
            unit("dup", (50, 60), 0),
        ];
        assert_eq!(
            pair_units(&parsed(&current), &parsed(&base)),
            [
                Pairing::Paired(1),
                Pairing::New,
                Pairing::Paired(2),
                Pairing::New
            ]
        );
    }

    #[test]
    fn anonymous_units_pair_under_their_named_owner_when_counts_match() {
        let base = [
            unit("owner", (0, 100), 0),
            unit(ANONYMOUS, (10, 20), 0),
            unit(ANONYMOUS, (12, 18), 0),
            unit("other", (200, 300), 0),
            unit(ANONYMOUS, (210, 220), 0),
            unit(ANONYMOUS, (400, 410), 0),
        ];
        let current = [
            unit("owner", (0, 100), 0),
            unit(ANONYMOUS, (10, 20), 0),
            unit(ANONYMOUS, (12, 18), 0),
            unit("other", (200, 300), 0),
            unit(ANONYMOUS, (210, 220), 0),
            unit(ANONYMOUS, (230, 240), 0),
            unit("added", (300, 350), 0),
            unit(ANONYMOUS, (310, 320), 0),
            unit(ANONYMOUS, (400, 410), 0),
        ];
        assert_eq!(
            pair_units(&parsed(&current), &parsed(&base)),
            [
                Pairing::Paired(0),
                Pairing::Paired(1),
                Pairing::Paired(2),
                Pairing::Paired(3),
                Pairing::Unmatched,
                Pairing::Unmatched,
                Pairing::New,
                Pairing::New,
                Pairing::Paired(5),
            ]
        );
    }

    /// A file whose units are `name`s with their text, one per line.
    fn file(units: &[(&str, &str)]) -> (String, ParsedSource) {
        let mut source = String::new();
        let mut functions = Vec::new();
        for (name, text) in units {
            let start = source.len();
            source.push_str(text);
            functions.push(unit(name, (start, source.len()), text.len()));
            source.push('\n');
        }
        let parsed = ParsedSource {
            functions,
            ..ParsedSource::default()
        };
        (source, parsed)
    }

    fn by_text(
        current: &[(&str, &str)],
        base: &[(&str, &str)],
        untouched: &[bool],
    ) -> Vec<Pairing> {
        let (source, parsed) = file(current);
        let (base_source, base_parsed) = file(base);
        let current = Text {
            source: &source,
            parsed: &parsed,
        };
        let base = Text {
            source: &base_source,
            parsed: &base_parsed,
        };
        pair_units_by_text(current, base, untouched)
    }

    #[test]
    fn an_untouched_unit_claims_the_base_unit_with_its_text() {
        use Pairing::{New, Paired};
        // A unit of the same name added before the old one is the new one.
        let old = ("f", "old body");
        assert_eq!(
            by_text(&[("f", "added"), old], &[old], &[false, true]),
            [New, Paired(0)]
        );
        // Also when the added unit is a copy of the old one.
        assert_eq!(
            by_text(&[old, old], &[old], &[false, true]),
            [New, Paired(0)]
        );
        // Without untouched units the pairing is the one by order.
        assert_eq!(
            by_text(&[("f", "added"), old], &[old], &[false, false]),
            [Paired(0), New]
        );
    }

    #[test]
    fn one_base_unit_takes_one_claim() {
        use Pairing::Paired;
        // Two untouched units with the text of one base unit: the second
        // keeps its place by order, so its values are compared with its own
        // base unit.
        let base = [("f", "long body"), ("f", "short")];
        assert_eq!(
            by_text(&[base[0], base[0]], &base, &[true, true]),
            [Paired(0), Paired(1)]
        );
        // A claim leaves the other units of the name the other base units.
        assert_eq!(
            by_text(&[("f", "edited"), base[0]], &base, &[false, true]),
            [Paired(1), Paired(0)]
        );
        // Another name with the same text is not claimed.
        assert_eq!(
            by_text(&[("g", "long body")], &[("f", "long body")], &[true]),
            [Pairing::New]
        );
    }

    #[test]
    fn line_ends_do_not_change_the_text_of_a_claim() {
        use Pairing::{New, Paired};
        let base = [("f", "a {\n}"), ("f", "b {\n}")];
        let current = [("f", "new"), ("f", "a {\r\n}"), ("f", "b {\r\n}")];
        assert_eq!(
            by_text(&current, &base, &[false, true, true]),
            [New, Paired(0), Paired(1)]
        );
        // A unit with no mark claims nothing.
        assert_eq!(
            by_text(&current, &base, &[false, true]),
            [Paired(1), Paired(0), New]
        );
    }

    #[test]
    fn left_out_units_keep_their_place_and_own_their_closures() {
        // The head left `f` out but measured the closure inside it; the base
        // measured `f` and has a closure at file level. A second `g` keeps
        // its place behind a left-out first `g`.
        let base = ParsedSource {
            functions: vec![
                unit("f", (0, 10), 0),
                unit(ANONYMOUS, (20, 30), 0),
                unit("g", (60, 70), 0),
            ],
            left_out: vec![left_out("g", (40, 50))],
            ..ParsedSource::default()
        };
        let current = ParsedSource {
            functions: vec![unit(ANONYMOUS, (5, 15), 0), unit("g", (60, 70), 0)],
            left_out: vec![left_out("f", (0, 30)), left_out("g", (40, 50))],
            ..ParsedSource::default()
        };
        assert_eq!(
            pair_units(&current, &base),
            [Pairing::Unmatched, Pairing::Paired(2)]
        );
        // The same closure under a left-out `f` at both revisions pairs; a
        // unit paired with a left-out base unit is new.
        let base = ParsedSource {
            functions: vec![unit(ANONYMOUS, (5, 15), 0)],
            left_out: vec![left_out("f", (0, 30)), left_out("g", (40, 50))],
            ..ParsedSource::default()
        };
        let current = ParsedSource {
            functions: vec![unit(ANONYMOUS, (5, 15), 0), unit("g", (40, 45), 0)],
            left_out: vec![left_out("f", (0, 30))],
            ..ParsedSource::default()
        };
        assert_eq!(
            pair_units(&current, &base),
            [Pairing::Paired(0), Pairing::New]
        );
    }

    #[test]
    fn template_units_do_not_own_script_closures() {
        let base = [unit(ANONYMOUS, (10, 20), 0)];
        let mut template = unit("<template>", (0, 100), 0);
        template.template = true;
        let current = [template, unit(ANONYMOUS, (30, 40), 0)];
        assert_eq!(
            pair_units(&parsed(&current), &parsed(&base)),
            [Pairing::New, Pairing::Paired(0)]
        );
    }

    #[test]
    fn status_follows_the_pairing_then_the_metric_value() {
        let base = unit("f", (0, 10), 16);
        let paired = Pairing::Paired(0);
        assert_eq!(
            status(Some(&base), paired, Metric::Cognitive, 18),
            Status::Worsened
        );
        assert_eq!(
            status(Some(&base), paired, Metric::Cognitive, 16),
            Status::Unchanged
        );
        assert_eq!(
            status(Some(&base), paired, Metric::Cognitive, 15),
            Status::Improved
        );
        assert_eq!(
            status(None, Pairing::New, Metric::Cognitive, 18),
            Status::New
        );
        assert_eq!(
            status(None, Pairing::Unmatched, Metric::Lines, 3),
            Status::Unmatched
        );
        assert_eq!(Status::parse("worsened"), Some(Status::Worsened));
        assert_eq!(Status::parse("missing"), None);
    }
}
