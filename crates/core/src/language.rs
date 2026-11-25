use std::{fmt, ops::Range, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tree_sitter::{Language as TsLanguage, Node, Parser, Tree};

use crate::cognitive;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    JavaScript,
    TypeScript,
    Tsx,
    Svelte,
    Dart,
    Rust,
    Python,
    Go,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FunctionMetrics {
    pub function: String,
    pub line: usize,
    pub end_line: usize,
    pub complexity: usize,
    pub cognitive: usize,
    pub depth: usize,
    pub lines: usize,
    pub params: usize,
    pub bool_ops: usize,
    pub widget_depth: usize,
    /// Byte range in the file, so containment gives a closure's enclosing
    /// function even when both start on the same line.
    #[serde(skip)]
    pub span: (usize, usize),
    /// A Svelte template unit, which `lines` and `params` do not apply to.
    #[serde(skip)]
    pub template: bool,
    /// A Dart `build` method, the only unit `widget_depth` applies to.
    #[serde(skip)]
    pub widget: bool,
}

/// Where a parsed fragment starts in its file.
#[derive(Clone, Copy, Default)]
struct Offset {
    line: usize,
    byte: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct GrammarInfo {
    pub language: &'static str,
    pub grammar: &'static str,
    pub version: &'static str,
}

impl Language {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "svelte" => Some(Self::Svelte),
            "dart" => Some(Self::Dart),
            "rs" => Some(Self::Rust),
            "py" | "pyi" => Some(Self::Python),
            "go" => Some(Self::Go),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::JavaScript => "javascript",
            Self::TypeScript | Self::Tsx => "typescript",
            Self::Svelte => "svelte",
            Self::Dart => "dart",
            Self::Rust => "rust",
            Self::Python => "python",
            Self::Go => "go",
        }
    }

    pub(crate) fn grammar(self) -> TsLanguage {
        match self {
            Self::JavaScript | Self::Svelte => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Dart => tree_sitter_dart::LANGUAGE.into(),
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }
}

pub fn grammar_inventory() -> Vec<GrammarInfo> {
    vec![
        GrammarInfo {
            language: "javascript",
            grammar: "tree-sitter-javascript",
            version: "0.25.0",
        },
        GrammarInfo {
            language: "typescript/tsx",
            grammar: "tree-sitter-typescript",
            version: "0.23.2",
        },
        GrammarInfo {
            language: "svelte",
            grammar: "tree-sitter-svelte-ng",
            version: "1.0.2",
        },
        GrammarInfo {
            language: "dart",
            grammar: "tree-sitter-dart",
            version: "0.2.0",
        },
        GrammarInfo {
            language: "rust",
            grammar: "tree-sitter-rust",
            version: "0.24.2",
        },
        GrammarInfo {
            language: "python",
            grammar: "tree-sitter-python",
            version: "0.25.0",
        },
        GrammarInfo {
            language: "go",
            grammar: "tree-sitter-go",
            version: "0.25.0",
        },
    ]
}

pub fn coverage_unknowns() -> Vec<(&'static str, Vec<String>)> {
    let languages = [
        ("javascript", Language::JavaScript),
        ("typescript", Language::TypeScript),
        ("tsx", Language::Tsx),
        ("dart", Language::Dart),
        ("rust", Language::Rust),
        ("python", Language::Python),
        ("go", Language::Go),
    ];
    let mut result: Vec<_> = languages
        .into_iter()
        .map(|(name, language)| (name, unknown_kinds(language.grammar())))
        .collect();
    result.push((
        "svelte",
        unknown_kinds(tree_sitter_svelte_ng::LANGUAGE.into()),
    ));
    result
}

fn unknown_kinds(grammar: TsLanguage) -> Vec<String> {
    const NEEDLES: &[&str] = &[
        "if",
        "for",
        "while",
        "loop",
        "match",
        "switch",
        "case",
        "catch",
        "except",
        "conditional",
        "ternary",
        "binary",
        "logical",
    ];
    let mut kinds = Vec::new();
    for id in 0..grammar.node_kind_count() {
        let Some(kind) = grammar.node_kind_for_id(id as u16) else {
            continue;
        };
        if NEEDLES.iter().any(|needle| kind.contains(needle)) && !coverage_classified(kind) {
            kinds.push(kind.to_owned());
        }
    }
    kinds.sort();
    kinds.dedup();
    kinds
}

fn coverage_classified(kind: &str) -> bool {
    matches!(
        kind,
        "if" | "else if"
            | "elif"
            | "for"
            | "while"
            | "loop"
            | "match"
            | "switch"
            | "case"
            | "catch"
            | "except"
            | "if_statement"
            | "if_expression"
            | "if_element"
            | "elif_clause"
            | "for_statement"
            | "for_in_statement"
            | "for_expression"
            | "for_element"
            | "for_in_clause"
            | "if_clause"
            | "while_statement"
            | "while_expression"
            | "loop_expression"
            | "switch_statement"
            | "switch_expression"
            | "expression_switch_statement"
            | "type_switch_statement"
            | "switch_statement_case"
            | "switch_statement_default"
            | "switch_case"
            | "switch_default"
            | "switch_expression_case"
            | "match_statement"
            | "match_expression"
            | "match_arm"
            | "case_clause"
            | "case_pattern"
            | "expression_case"
            | "type_case"
            | "communication_case"
            | "catch_clause"
            | "except_clause"
            | "conditional_expression"
            | "ternary_expression"
            | "binary_expression"
            | "binary_operator"
            | "boolean_operator"
            | "logical_and_expression"
            | "logical_or_expression"
            | "if_null_expression"
            | "if_start"
            | "else_if_start"
            | "each_start"
            | "await_start"
            | "catch_start"
    ) || coverage_ignored(kind)
}

fn coverage_ignored(kind: &str) -> bool {
    ignored_kind_name(kind)
        || matches!(
            kind,
            "accessibility_modifier"
                | "catch_block"
                | "conditional_type"
                | "default_case"
                | "else_if_block"
                | "except_clause_repeat1"
                | "for_clause"
                | "for_lifetimes"
                | "foreign_mod_item"
                | "for_in_clause_repeat1"
                | "format_expression"
                | "format_specifier"
                | "if_end"
                | "if_statement_repeat1"
                | "import_specification"
                | "lifetime"
                | "match_block"
                | "match_pattern"
                | "qualified"
                | "qualified_type"
                | "shift_expression"
                | "switch_block"
                | "switch_body"
                | "type_case_repeat1"
        )
}

fn ignored_kind_name(kind: &str) -> bool {
    kind.starts_with('_')
        || [
            "_repeat",
            "identifier",
            "parameter",
            "modifier",
            "specifier",
        ]
        .iter()
        .any(|part| kind.contains(part))
}

/// The highest unit `parse_source` measures, in syntax-tree levels below the
/// unit's own node. Measuring a unit recurses once per level, so this bound,
/// together with `on_measurement_stack`, keeps any input from overflowing the
/// stack. Ordinary code stays far below it: a 1,000-term `a + b + …` chain is
/// about 1,000 levels high.
pub const MAX_SYNTAX_DEPTH: usize = 3_000;

/// Stack for `on_measurement_stack`. On an unoptimized build, the highest unit
/// allowed needed at most 6.4 MiB (nested Svelte elements) and about 3 MiB for
/// the other languages, and a Svelte template expression adds its own parse at
/// most once on top, so 64 MiB leaves a wide margin. Only the pages actually
/// used are committed.
const MEASUREMENT_STACK: usize = 64 << 20;

/// Why `parse_source` left some units of a file out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unmeasured {
    /// A unit is higher than `MAX_SYNTAX_DEPTH`.
    TooDeep,
    /// A Svelte `<script>` start tag that the grammar could not close, so its
    /// content is not known.
    ScriptBlock,
}

impl fmt::Display for Unmeasured {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooDeep => "syntax nesting too deep to measure",
            Self::ScriptBlock => "script block could not be parsed",
        })
    }
}

/// A unit `parse_source` left out. Baseline matching still needs its name
/// and span: it keeps the unit's place among units of the same name, and it
/// still owns the closures inside it that were measured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeftOut {
    pub function: String,
    /// Byte range in the file, as `FunctionMetrics::span`.
    pub span: (usize, usize),
    /// A Svelte template unit.
    pub template: bool,
}

/// The units `parse_source` measured in one file, and why it left others out.
#[derive(Debug, Default)]
pub struct ParsedSource {
    /// Measured units, in source order.
    pub functions: Vec<FunctionMetrics>,
    /// Units left out, in source order.
    pub left_out: Vec<LeftOut>,
    /// Sorted and without repeats; empty when every unit was measured.
    pub unmeasured: Vec<Unmeasured>,
}

impl ParsedSource {
    /// The `UNVERIFIED` reason for a file with units left out.
    pub fn reason(&self) -> Option<String> {
        (!self.unmeasured.is_empty()).then(|| {
            self.unmeasured
                .iter()
                .map(Unmeasured::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    fn extend(&mut self, other: Self) {
        self.functions.extend(other.functions);
        self.left_out.extend(other.left_out);
        self.unmeasured.extend(other.unmeasured);
    }

    fn leave_out(&mut self, function: String, node: Node<'_>, offset: usize, template: bool) {
        self.left_out.push(LeftOut {
            function,
            span: (node.start_byte() + offset, node.end_byte() + offset),
            template,
        });
        self.unmeasured.push(Unmeasured::TooDeep);
    }

    fn finish(mut self) -> Self {
        self.functions
            .sort_by_key(|item| (item.line, item.end_line));
        self.left_out.sort_by_key(|item| item.span);
        self.unmeasured.sort();
        self.unmeasured.dedup();
        self
    }
}

/// Runs `task` on a thread with a stack sized for `MAX_SYNTAX_DEPTH`, so
/// measuring never depends on the caller's stack. Run a whole scan inside one
/// call, not one call per file.
///
/// # Errors
///
/// Fails when the thread cannot be started.
pub fn on_measurement_stack<T: Send>(task: impl FnOnce() -> T + Send) -> std::io::Result<T> {
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("pickcheck-measure".to_owned())
            .stack_size(MEASUREMENT_STACK)
            .spawn_scoped(scope, task)?;
        Ok(handle
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
    })
}

fn parse_tree(grammar: &TsLanguage, source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(grammar)
        .context("incompatible tree-sitter grammar")?;
    parser
        .parse(source, None)
        .context("tree-sitter parser returned no tree")
}

/// Whether the nodes inside `range` reach more than `MAX_SYNTAX_DEPTH` levels
/// below the highest of them, so the levels a wrapper adds around a template
/// text never count. Found with a cursor, so the check itself never recurses.
pub(crate) fn exceeds_depth_limit(tree: &Tree, range: Range<usize>) -> bool {
    let mut top: Option<(Node<'_>, usize)> = None;
    let mut too_deep = false;
    walk_tree(tree.root_node(), |step| {
        match step {
            Step::Enter(node, depth) => match top {
                Some((_, top_depth)) => too_deep |= depth - top_depth > MAX_SYNTAX_DEPTH,
                None if range.start <= node.start_byte() && node.end_byte() <= range.end => {
                    top = Some((node, depth));
                }
                None => {}
            },
            Step::Leave(node) => {
                if top.is_some_and(|(top, _)| top == node) {
                    top = None;
                }
            }
        }
        !too_deep
    });
    too_deep
}

/// One step of `walk_tree`.
enum Step<'tree> {
    /// A node and its depth below the walk's root. The visitor returns
    /// whether to walk its children.
    Enter(Node<'tree>, usize),
    /// A node entered earlier, after its children.
    Leave(Node<'tree>),
}

/// Visits `root` and every node below it in source order with one cursor, so
/// the walk never recurses however deep the tree is.
fn walk_tree<'tree>(root: Node<'tree>, mut visit: impl FnMut(Step<'tree>) -> bool) {
    let mut cursor = root.walk();
    let mut depth = 0;
    loop {
        if visit(Step::Enter(cursor.node(), depth)) && cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            visit(Step::Leave(cursor.node()));
            if depth == 0 {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
            cursor.goto_parent();
            depth -= 1;
        }
    }
}

/// A unit that `walk_tree` has entered and not yet left.
struct OpenUnit<'tree> {
    node: Node<'tree>,
    depth: usize,
    deepest: usize,
}

impl<'tree> OpenUnit<'tree> {
    fn new(node: Node<'tree>, depth: usize) -> Self {
        Self {
            node,
            depth,
            deepest: depth,
        }
    }

    fn height(&self) -> usize {
        self.deepest - self.depth
    }
}

/// Every function in source order, with the height of its whole subtree.
/// Nested functions count toward it, because measuring a function can walk
/// into them.
fn function_units(root: Node<'_>, language: Language) -> Vec<(Node<'_>, usize)> {
    let mut open: Vec<(usize, OpenUnit<'_>)> = Vec::new();
    let mut units = Vec::new();
    walk_tree(root, |step| {
        match step {
            Step::Enter(node, depth) => {
                if node.is_named() && is_function(language, node.kind()) {
                    open.push((units.len(), OpenUnit::new(node, depth)));
                    units.push((node, 0));
                }
                if let Some((_, unit)) = open.last_mut() {
                    unit.deepest = unit.deepest.max(depth);
                }
            }
            Step::Leave(node) => {
                if let Some((index, unit)) = open.pop_if(|(_, unit)| unit.node == node) {
                    if let Some((_, outer)) = open.last_mut() {
                        outer.deepest = outer.deepest.max(unit.deepest);
                    }
                    units[index].1 = unit.height();
                }
            }
        }
        true
    });
    units
}

/// Measures every unit of `source`. A unit higher than `MAX_SYNTAX_DEPTH` is
/// left out and listed in `unmeasured`; the other units are still measured.
///
/// The depth guarantee holds only inside `on_measurement_stack`: measuring a
/// unit recurses once per level, so a caller on a small stack must run this
/// there.
///
/// # Errors
///
/// Fails when a grammar cannot be loaded or the parser returns no tree.
pub fn parse_source(language: Language, source: &str) -> Result<ParsedSource> {
    if language == Language::Svelte {
        return parse_svelte(source);
    }
    parse_with_offset(language, source, Offset::default())
}

fn parse_with_offset(language: Language, source: &str, offset: Offset) -> Result<ParsedSource> {
    let tree = parse_tree(&language.grammar(), source)?;
    let lines = LineIndex::new(source, tree.root_node());
    let mut parsed = ParsedSource::default();
    for (node, height) in function_units(tree.root_node(), language) {
        if height > MAX_SYNTAX_DEPTH {
            let name = function_name(node, language, source);
            parsed.leave_out(name, node, offset.byte, false);
        } else {
            parsed
                .functions
                .push(measure_function(node, language, source, offset, &lines));
        }
    }
    Ok(parsed.finish())
}

fn measure_function(
    node: Node<'_>,
    language: Language,
    source: &str,
    offset: Offset,
    lines: &LineIndex,
) -> FunctionMetrics {
    let mut score = Score {
        complexity: 1,
        depth: 0,
        bool_ops: 0,
    };
    let body = node.child_by_field_name("body").unwrap_or(node);
    measure_node(body, node.id(), language, source, 0, &mut score);
    let start = node.start_position().row + 1;
    let end = node.end_position().row + 1;
    let widget_depth = measure_widget_depth(node, language, source);
    FunctionMetrics {
        function: function_name(node, language, source),
        line: start + offset.line,
        end_line: end + offset.line,
        complexity: score.complexity,
        cognitive: cognitive::measure_function(node, language, source),
        depth: score.depth,
        lines: lines.significant_lines(source, start, end, node),
        params: parameter_count(node, language, source),
        bool_ops: score.bool_ops,
        widget_depth: widget_depth.unwrap_or(0),
        span: (
            node.start_byte() + offset.byte,
            node.end_byte() + offset.byte,
        ),
        template: false,
        widget: widget_depth.is_some(),
    }
}

struct Score {
    complexity: usize,
    depth: usize,
    bool_ops: usize,
}

fn measure_node(
    node: Node<'_>,
    root_id: usize,
    language: Language,
    source: &str,
    depth: usize,
    score: &mut Score,
) {
    if node.id() != root_id && is_function(language, node.kind()) {
        return;
    }
    score.complexity += decision_count(node, language, source);
    if is_boolean_chain_root(node, language, source) {
        score.bool_ops = score
            .bool_ops
            .max(boolean_operators_in_chain(node, root_id, language, source));
    }
    let opens = opens_depth(node, language) && !is_else_if(node);
    let next_depth = depth + usize::from(opens);
    score.depth = score.depth.max(next_depth);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        measure_node(child, root_id, language, source, next_depth, score);
    }
}

fn is_boolean_chain_root(node: Node<'_>, language: Language, source: &str) -> bool {
    if !is_boolean_operator(node, language, source) {
        return false;
    }
    let mut parent = node.parent();
    while parent.is_some_and(|item| item.kind() == "parenthesized_expression") {
        parent = parent.and_then(|item| item.parent());
    }
    !parent.is_some_and(|item| is_boolean_operator(item, language, source))
}

fn boolean_operators_in_chain(
    node: Node<'_>,
    root_id: usize,
    language: Language,
    source: &str,
) -> usize {
    if node.id() != root_id
        && (is_function(language, node.kind()) || is_conditional_expression(node.kind()))
    {
        return 0;
    }
    let own = boolean_operator_count(node, language, source);
    let mut cursor = node.walk();
    own + node
        .named_children(&mut cursor)
        .map(|child| boolean_operators_in_chain(child, root_id, language, source))
        .sum::<usize>()
}

fn is_conditional_expression(kind: &str) -> bool {
    matches!(kind, "conditional_expression" | "ternary_expression")
}

fn is_boolean_operator(node: Node<'_>, language: Language, source: &str) -> bool {
    match language {
        Language::JavaScript | Language::TypeScript | Language::Tsx => {
            matches!(
                node.kind(),
                "binary_expression" | "augmented_assignment_expression"
            ) && has_operator(node, source, &["&&", "||", "??", "&&=", "||=", "??="])
        }
        Language::Dart => {
            matches!(
                node.kind(),
                "logical_and_expression" | "logical_or_expression" | "if_null_expression"
            ) || node.kind() == "assignment_expression"
                && has_operator(node, source, &["&&=", "||=", "??="])
        }
        Language::Rust => {
            node.kind() == "let_chain"
                || node.kind() == "binary_expression" && has_operator(node, source, &["&&", "||"])
        }
        Language::Go => {
            node.kind() == "binary_expression" && has_operator(node, source, &["&&", "||"])
        }
        Language::Python => {
            node.kind() == "boolean_operator" && has_operator(node, source, &["and", "or"])
        }
        Language::Svelte => false,
    }
}

/// `None` when the unit is not a Dart `build` method.
fn measure_widget_depth(node: Node<'_>, language: Language, source: &str) -> Option<usize> {
    if language != Language::Dart
        || node.kind() != "method_declaration"
        || direct_name(node, source) != Some("build")
    {
        return None;
    }
    let body = node.child_by_field_name("body").unwrap_or(node);
    Some(widget_depth_in_node(body, node.id(), source, 0, false))
}

fn widget_depth_in_node(
    node: Node<'_>,
    root_id: usize,
    source: &str,
    depth: usize,
    closure_slot: bool,
) -> usize {
    if node.id() != root_id && is_function(Language::Dart, node.kind()) {
        if node.kind() != "function_expression" || !closure_slot {
            return depth;
        }
        let body = node.child_by_field_name("body").unwrap_or(node);
        return widget_depth_in_node(body, root_id, source, depth, false);
    }
    if node.kind() == "named_argument" {
        return widget_slot_value(node, source).map_or(depth, |value| {
            widget_depth_in_node(value, root_id, source, depth, true)
        });
    }
    let constructor = is_constructor_like(node, source);
    let depth = depth + usize::from(constructor);
    let mut maximum = depth;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        maximum = maximum.max(widget_depth_in_node(
            child,
            root_id,
            source,
            depth,
            closure_slot || constructor,
        ));
    }
    maximum
}

/// A named argument carries depth only when its label names a widget slot;
/// `padding:` and `decoration:` values are configuration, not visual nesting.
/// The check lives here rather than at the constructor so that it still applies
/// when the grammar leaves an argument list dangling, as it does for arrow-bodied
/// builders.
fn widget_slot_value<'tree>(node: Node<'tree>, source: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    let mut children = node.named_children(&mut cursor);
    let label = children.next()?;
    let name = node_text(label, source).trim_end_matches(':');
    if !WIDGET_SLOTS.contains(&name) {
        return None;
    }
    children.next()
}

const WIDGET_SLOTS: &[&str] = &[
    "child",
    "children",
    "body",
    "appBar",
    "title",
    "subtitle",
    "leading",
    "trailing",
    "icon",
    "content",
    "actions",
    "bottomNavigationBar",
    "floatingActionButton",
    "drawer",
    "endDrawer",
    "flexibleSpace",
    "bottom",
    "header",
    "footer",
    "label",
    "prefix",
    "suffix",
    "prefixIcon",
    "suffixIcon",
    "separator",
    "placeholder",
    "builder",
    "itemBuilder",
    "separatorBuilder",
];

fn is_constructor_like(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "const_object_expression" | "new_expression" => true,
        "constructor_invocation" => node
            .child_by_field_name("type")
            .is_some_and(|callee| starts_ascii_uppercase(node_text(callee, source))),
        // A call on the result of another call is a method chain
        // (`Text('x').animate().fadeIn()`), not a new layer of widget. Only the
        // innermost constructor of the chain counts.
        "call_expression" if chained_call(node) => false,
        "call_expression" => node
            .child_by_field_name("function")
            .and_then(leftmost_callee_identifier)
            .is_some_and(|callee| starts_ascii_uppercase(node_text(callee, source))),
        _ => false,
    }
}

fn chained_call(node: Node<'_>) -> bool {
    node.child_by_field_name("function")
        .is_some_and(|callee| contains_call(callee))
}

fn contains_call(node: Node<'_>) -> bool {
    if node.kind() == "call_expression" {
        return true;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| contains_call(child))
}

fn leftmost_callee_identifier(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "identifier" | "type_identifier" => Some(node),
        "call_expression" => node
            .child_by_field_name("function")
            .and_then(leftmost_callee_identifier),
        "member_expression" | "null_aware_member_expression" => node
            .child_by_field_name("object")
            .and_then(leftmost_callee_identifier),
        _ => None,
    }
}

fn starts_ascii_uppercase(text: &str) -> bool {
    text.trim_start_matches('_')
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_uppercase)
}

pub(crate) fn is_function(language: Language, kind: &str) -> bool {
    match language {
        Language::JavaScript | Language::TypeScript | Language::Tsx => matches!(
            kind,
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "method_definition"
                | "generator_function"
                | "generator_function_declaration"
        ),
        Language::Dart => matches!(
            kind,
            "function_declaration"
                | "local_function_declaration"
                | "function_expression"
                | "method_declaration"
                | "getter_declaration"
                | "setter_declaration"
        ),
        Language::Rust => matches!(kind, "function_item" | "closure_expression"),
        Language::Python => matches!(kind, "function_definition" | "lambda"),
        Language::Go => matches!(
            kind,
            "function_declaration" | "method_declaration" | "func_literal"
        ),
        Language::Svelte => false,
    }
}

/// A Rust `let_chain` holds its `&&` tokens as anonymous children rather than
/// as `binary_expression` nodes, so it carries one operator per token.
fn let_chain_operators(node: Node<'_>) -> usize {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| child.kind() == "&&")
        .count()
}

fn boolean_operator_count(node: Node<'_>, language: Language, source: &str) -> usize {
    if node.kind() == "let_chain" {
        return let_chain_operators(node);
    }
    usize::from(is_boolean_operator(node, language, source))
}

fn decision_count(node: Node<'_>, language: Language, source: &str) -> usize {
    if language == Language::Rust && node.kind() == "let_chain" {
        return let_chain_operators(node);
    }
    usize::from(is_decision(node, language, source))
}

fn is_decision(node: Node<'_>, language: Language, source: &str) -> bool {
    match language {
        Language::JavaScript | Language::TypeScript | Language::Tsx => js_decision(node, source),
        Language::Dart => dart_decision(node, source),
        Language::Rust => rust_decision(node, source),
        Language::Python => python_decision(node, source),
        Language::Go => go_decision(node, source),
        Language::Svelte => false,
    }
}

fn js_decision(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "if_statement" | "for_statement" | "for_in_statement" | "while_statement"
        | "do_statement" | "switch_case" | "catch_clause" | "ternary_expression" => true,
        "binary_expression" | "augmented_assignment_expression" => {
            has_operator(node, source, &["&&", "||", "??", "&&=", "||=", "??="])
        }
        _ => false,
    }
}

fn dart_decision(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "if_statement"
        | "if_element"
        | "for_statement"
        | "for_element"
        | "while_statement"
        | "do_statement"
        | "switch_statement_case"
        | "conditional_expression" => true,
        "block" => is_dart_handler(node),
        "switch_expression_case" => !is_default_arm(node, source),
        "logical_and_expression" | "logical_or_expression" | "if_null_expression" => true,
        "assignment_expression" => has_operator(node, source, &["??="]),
        _ => false,
    }
}

/// Every `block` child of a Dart `try_statement` other than its `body` is a
/// handler: `on T {}` has no `catch_clause`, so counting that node misses it.
pub(crate) fn is_dart_handler(node: Node<'_>) -> bool {
    node.kind() == "block"
        && node.parent().is_some_and(|parent| {
            parent.kind() == "try_statement"
                && parent
                    .child_by_field_name("body")
                    .is_none_or(|body| body.id() != node.id())
        })
}

fn rust_decision(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "if_expression" | "for_expression" | "while_expression" | "loop_expression" => true,
        "match_arm" => !is_default_arm(node, source),
        "let_declaration" => node.child_by_field_name("alternative").is_some(),
        "binary_expression" => has_operator(node, source, &["&&", "||"]),
        _ => false,
    }
}

fn python_decision(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "if_statement"
        | "elif_clause"
        | "for_statement"
        | "while_statement"
        | "except_clause"
        | "conditional_expression"
        | "for_in_clause"
        | "if_clause" => true,
        "case_clause" => !is_default_arm(node, source),
        "boolean_operator" => has_operator(node, source, &["and", "or"]),
        _ => false,
    }
}

fn go_decision(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "if_statement" | "for_statement" | "expression_case" | "type_case"
        | "communication_case" => true,
        "binary_expression" => has_operator(node, source, &["&&", "||"]),
        _ => false,
    }
}

pub(crate) fn has_operator(node: Node<'_>, source: &str, wanted: &[&str]) -> bool {
    if let Some(operator) = node.child_by_field_name("operator") {
        return wanted.contains(&node_text(operator, source));
    }
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| !child.is_named())
        .any(|child| wanted.contains(&node_text(child, source)))
}

fn is_default_arm(node: Node<'_>, source: &str) -> bool {
    let pattern = node.child_by_field_name("pattern").or_else(|| {
        let mut cursor = node.walk();
        node.named_children(&mut cursor).next()
    });
    pattern.is_some_and(|pattern| {
        let wildcard = node_text(pattern, source).trim() == "_"
            || pattern
                .child_by_field_name("pattern")
                .is_some_and(|inner| node_text(inner, source).trim() == "_");
        let mut cursor = node.walk();
        wildcard
            && !node
                .children(&mut cursor)
                .any(|child| node_text(child, source) == "when")
    })
}

fn opens_depth(node: Node<'_>, language: Language) -> bool {
    match language {
        Language::JavaScript | Language::TypeScript | Language::Tsx => matches!(
            node.kind(),
            "if_statement"
                | "for_statement"
                | "for_in_statement"
                | "while_statement"
                | "do_statement"
                | "switch_statement"
                | "try_statement"
        ),
        Language::Dart => matches!(
            node.kind(),
            "if_statement"
                | "for_statement"
                | "while_statement"
                | "do_statement"
                | "switch_statement"
                | "switch_expression"
                | "try_statement"
        ),
        Language::Rust => matches!(
            node.kind(),
            "if_expression"
                | "for_expression"
                | "while_expression"
                | "loop_expression"
                | "match_expression"
        ),
        Language::Python => matches!(
            node.kind(),
            "if_statement"
                | "for_statement"
                | "while_statement"
                | "match_statement"
                | "try_statement"
                | "with_statement"
        ),
        Language::Go => matches!(
            node.kind(),
            "if_statement"
                | "for_statement"
                | "expression_switch_statement"
                | "type_switch_statement"
                | "select_statement"
        ),
        Language::Svelte => false,
    }
}

fn is_else_if(node: Node<'_>) -> bool {
    if !matches!(node.kind(), "if_statement" | "if_expression") {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    if matches!(parent.kind(), "if_statement" | "if_expression") {
        return parent
            .child_by_field_name("alternative")
            .is_some_and(|item| item.id() == node.id());
    }
    parent.kind() == "else_clause"
        && parent
            .parent()
            .is_some_and(|item| matches!(item.kind(), "if_statement" | "if_expression"))
}

fn function_name(node: Node<'_>, language: Language, source: &str) -> String {
    if let Some(name) = direct_name(node, source) {
        return qualify_method(node, language, name, source);
    }
    let mut child = node;
    while let Some(item) = child.parent() {
        if is_function(language, item.kind()) {
            break;
        }
        if let Some(name) = binding_name(item, child, source) {
            return name;
        }
        if !binding_wrapper(item.kind()) {
            break;
        }
        child = item;
    }
    "<anonymous>".to_owned()
}

fn direct_name<'source>(node: Node<'_>, source: &'source str) -> Option<&'source str> {
    if let Some(named) = node.child_by_field_name("name") {
        return named.utf8_text(source.as_bytes()).ok();
    }
    matches!(
        node.kind(),
        "function_declaration"
            | "local_function_declaration"
            | "method_declaration"
            | "getter_declaration"
            | "setter_declaration"
    )
    .then(|| descendant_field(node, "name"))
    .flatten()
    .and_then(|named| named.utf8_text(source.as_bytes()).ok())
}

/// Dart methods carry their annotations (`@override`) before the signature, and
/// an annotation has its own `name` field. Skip them so the method's own name
/// wins.
fn annotation_kind(kind: &str) -> bool {
    kind == "metadata" || kind.contains("annotation")
}

/// The first `field` child in source order outside bodies and annotations,
/// found without recursion because a unit left out as too deep is named too.
fn descendant_field<'a>(node: Node<'a>, field: &str) -> Option<Node<'a>> {
    let mut found = None;
    walk_tree(node, |step| {
        let Step::Enter(item, depth) = step else {
            return false;
        };
        let skipped = depth > 0
            && (!item.is_named() || item.kind().contains("body") || annotation_kind(item.kind()));
        if found.is_some() || skipped {
            return false;
        }
        found = item.child_by_field_name(field);
        found.is_none()
    });
    found
}

fn qualify_method(node: Node<'_>, language: Language, name: &str, source: &str) -> String {
    if !is_method(node, language) {
        return name.to_owned();
    }
    if language == Language::Go {
        return node
            .child_by_field_name("receiver")
            .and_then(|receiver| receiver_type(receiver, source))
            .map_or_else(|| name.to_owned(), |owner| format!("{owner}.{name}"));
    }
    let type_kinds = match language {
        Language::JavaScript | Language::TypeScript | Language::Tsx => {
            &["class_declaration", "class"] as &[&str]
        }
        Language::Dart => &[
            "class_declaration",
            "mixin_declaration",
            "extension_declaration",
            "extension_type_declaration",
        ],
        Language::Python => &["class_definition"],
        Language::Rust => &["impl_item"],
        _ => return name.to_owned(),
    };
    let mut parent = node.parent();
    while let Some(item) = parent {
        if type_kinds.contains(&item.kind())
            && let Some(owner) = type_owner(item, language, source)
        {
            return format!("{owner}.{name}");
        }
        parent = item.parent();
    }
    name.to_owned()
}

fn is_method(node: Node<'_>, language: Language) -> bool {
    if matches!(
        node.kind(),
        "method_definition" | "method_declaration" | "getter_declaration" | "setter_declaration"
    ) {
        return true;
    }
    if language == Language::Python && node.kind() == "function_definition" {
        return has_ancestor(node, "class_definition");
    }
    language == Language::Rust && node.kind() == "function_item" && has_ancestor(node, "impl_item")
}

fn type_owner<'a>(node: Node<'_>, language: Language, source: &'a str) -> Option<&'a str> {
    if language == Language::Rust {
        return node
            .child_by_field_name("type")?
            .utf8_text(source.as_bytes())
            .ok();
    }
    direct_name(node, source)
}

fn has_ancestor(node: Node<'_>, kind: &str) -> bool {
    let mut parent = node.parent();
    while let Some(item) = parent {
        if item.kind() == kind {
            return true;
        }
        parent = item.parent();
    }
    false
}

fn receiver_type(node: Node<'_>, source: &str) -> Option<String> {
    node_text(node, source)
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .rfind(|word| !word.is_empty())
        .map(str::to_owned)
}

fn binding_name(node: Node<'_>, value: Node<'_>, source: &str) -> Option<String> {
    let (name_field, value_field) = match node.kind() {
        "variable_declarator" | "initialized_variable_definition" => ("name", "value"),
        "let_declaration" => ("pattern", "value"),
        "pair" => ("key", "value"),
        "assignment_expression" | "assignment_statement" => ("left", "right"),
        "short_var_declaration" => ("left", "right"),
        _ => return None,
    };
    let assigned = node.child_by_field_name(value_field)?;
    if assigned.id() != value.id() {
        return None;
    }
    let name = node.child_by_field_name(name_field)?;
    let text = node_text(name, source).trim();
    (!text.is_empty() && !text.contains([' ', '\n'])).then(|| text.to_owned())
}

fn binding_wrapper(kind: &str) -> bool {
    matches!(kind, "parenthesized_expression" | "expression_list")
}

fn parameter_count(node: Node<'_>, language: Language, source: &str) -> usize {
    let params = node
        .child_by_field_name("parameters")
        .or_else(|| descendant_field(node, "parameters"));
    let Some(params) = params else {
        return usize::from(node.child_by_field_name("parameter").is_some());
    };
    let mut cursor = params.walk();
    let children: Vec<_> = params.named_children(&mut cursor).collect();
    match language {
        Language::Go => children
            .iter()
            .map(|child| go_parameter_count(*child))
            .sum(),
        _ => children
            .iter()
            .filter(|child| !receiver_parameter(**child, source))
            .count(),
    }
}

fn go_parameter_count(node: Node<'_>) -> usize {
    if !matches!(
        node.kind(),
        "parameter_declaration" | "variadic_parameter_declaration"
    ) {
        return 1;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .count()
        .saturating_sub(1)
        .max(1)
}

fn receiver_parameter(node: Node<'_>, source: &str) -> bool {
    let text = node_text(node, source).trim();
    matches!(node.kind(), "self_parameter")
        || matches!(text, "self" | "&self" | "&mut self" | "this")
        || text.starts_with("self:")
        || text.starts_with("this:")
}

/// Line starts and comment ranges of one parsed source, built once so that
/// `lines` costs each function only its own lines instead of a rescan of the
/// whole source.
struct LineIndex {
    /// Byte offset of each line's first byte.
    starts: Vec<usize>,
    /// Every comment node, in source order and never overlapping.
    comments: Vec<(usize, usize)>,
}

impl LineIndex {
    fn new(source: &str, root: Node<'_>) -> Self {
        let starts = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(index, _)| index + 1))
            .filter(|start| *start < source.len())
            .collect();
        let mut comments = Vec::new();
        walk_tree(root, |step| match step {
            Step::Enter(node, _) if node.kind().ends_with("comment") => {
                comments.push((node.start_byte(), node.end_byte()));
                false
            }
            _ => true,
        });
        Self { starts, comments }
    }

    /// Lines `start..=end` (1-based) of `node` that hold a non-whitespace byte
    /// outside the comments inside `node`.
    fn significant_lines(&self, source: &str, start: usize, end: usize, node: Node<'_>) -> usize {
        let first = self
            .comments
            .partition_point(|(comment, _)| *comment < node.start_byte());
        let last = self
            .comments
            .partition_point(|(_, comment)| *comment <= node.end_byte());
        let comments = &self.comments[first..last.max(first)];
        (start..=end.min(self.starts.len()))
            .filter(|line| {
                let line_start = self.starts[line - 1];
                let line_end = self.starts.get(*line).copied().unwrap_or(source.len());
                line_has_code(line_start, &source[line_start..line_end], comments)
            })
            .count()
    }
}

/// `comments` is sorted, so only the ones that reach this line are checked.
fn line_has_code(line_start: usize, line: &str, comments: &[(usize, usize)]) -> bool {
    let first = comments.partition_point(|(_, end)| *end <= line_start);
    let line_end = line_start + line.len();
    let nearby = &comments[first..];
    let nearby = &nearby[..nearby.partition_point(|(start, _)| *start < line_end)];
    line.bytes().enumerate().any(|(index, byte)| {
        !byte.is_ascii_whitespace()
            && !nearby
                .iter()
                .any(|(start, end)| *start <= line_start + index && line_start + index < *end)
    })
}

pub(crate) fn node_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

fn parse_svelte(source: &str) -> Result<ParsedSource> {
    let tree = parse_tree(&tree_sitter_svelte_ng::LANGUAGE.into(), source)?;
    let root = tree.root_node();
    let units = svelte_units(root, source);
    let mut parsed = ParsedSource::default();
    if units.damaged_script {
        parsed.unmeasured.push(Unmeasured::ScriptBlock);
    }
    let mut script = Language::JavaScript;
    for element in &units.scripts {
        let Some((language, start, end)) = script_content(*element, source) else {
            continue;
        };
        if language == Language::TypeScript {
            script = language;
        }
        parsed.extend(parse_with_offset(
            language,
            &source[start.byte..end],
            start,
        )?);
    }
    for (unit, height) in units.template {
        let measured = (height <= MAX_SYNTAX_DEPTH)
            .then(|| measure_template_unit(unit, source, script, unit.id() == root.id()))
            .flatten();
        match measured {
            Some(unit) => parsed.functions.push(unit),
            None => parsed.leave_out(template_unit_name(unit, source), unit, 0, true),
        }
    }
    Ok(parsed.finish())
}

/// The parts of a Svelte tree that are measured, found with one cursor walk.
#[derive(Default)]
struct SvelteUnits<'tree> {
    scripts: Vec<Node<'tree>>,
    /// A `<script>` start tag outside any `script_element`, as the grammar
    /// leaves one whose content holds a string such as `"</scripts>"`.
    damaged_script: bool,
    /// Template units, each with the height of the part of the tree its walks
    /// visit: its own subtree without scripts, styles, and the units inside it.
    template: Vec<(Node<'tree>, usize)>,
}

/// A `<script>` inside an HTML comment is part of a `comment` node, not an
/// element, so it is never collected.
fn svelte_units<'tree>(root: Node<'tree>, source: &str) -> SvelteUnits<'tree> {
    let mut units = SvelteUnits::default();
    let mut open: Vec<OpenUnit<'_>> = Vec::new();
    walk_tree(root, |step| match step {
        Step::Enter(node, depth) => {
            if let Some(unit) = open.last_mut() {
                unit.deepest = unit.deepest.max(depth);
            }
            match node.kind() {
                "script_element" => {
                    units.scripts.push(node);
                    return false;
                }
                "style_element" => return false,
                "start_tag" => units.damaged_script |= is_script_tag(node, source),
                kind if open.is_empty()
                    || kind == "snippet_statement"
                    || open.len() == 1 && TEMPLATE_BLOCKS.contains(&kind) =>
                {
                    open.push(OpenUnit::new(node, depth));
                }
                _ => {}
            }
            true
        }
        Step::Leave(node) => {
            if let Some(unit) = open.pop_if(|unit| unit.node == node) {
                units.template.push((unit.node, unit.height()));
            }
            true
        }
    });
    units
}

/// The Svelte grammar recognises only a lowercase `script` tag as a script.
fn is_script_tag(tag: Node<'_>, source: &str) -> bool {
    child_of_kind(tag, "tag_name").is_some_and(|name| node_text(name, source) == "script")
}

/// The grammar of a `script_element` and its content: every byte between its
/// start tag and its end tag. A string such as `"</scripts>"` can split the
/// content into `raw_text` and an `ERROR` node; this range still covers both.
fn script_content(element: Node<'_>, source: &str) -> Option<(Language, Offset, usize)> {
    let tag = child_of_kind(element, "start_tag")?;
    let end = child_of_kind(element, "end_tag").map_or(element.end_byte(), |end| end.start_byte());
    let start = Offset {
        line: tag.end_position().row,
        byte: tag.end_byte(),
    };
    Some((script_language(tag, source), start, end.max(start.byte)))
}

fn child_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
}

/// TypeScript when the start tag has `lang="ts"` or `lang="typescript"`,
/// quoted or not.
fn script_language(tag: Node<'_>, source: &str) -> Language {
    let mut cursor = tag.walk();
    let typescript = tag
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "attribute")
        .any(|attribute| {
            let name = child_of_kind(attribute, "attribute_name")
                .is_some_and(|name| node_text(name, source).eq_ignore_ascii_case("lang"));
            let value = child_of_kind(attribute, "attribute_value").or_else(|| {
                child_of_kind(attribute, "quoted_attribute_value")
                    .and_then(|quoted| child_of_kind(quoted, "attribute_value"))
            });
            name && value
                .is_some_and(|value| matches!(node_text(value, source), "ts" | "typescript"))
        });
    if typescript {
        Language::TypeScript
    } else {
        Language::JavaScript
    }
}

const TEMPLATE_BLOCKS: [&str; 3] = ["if_statement", "each_statement", "await_statement"];

/// The template walks of one unit. `script` is the grammar of the component's
/// `<script>`, which also parses template expressions. `root` marks the
/// document, whose walk leaves out the blocks that form their own units.
struct TemplateWalk<'s> {
    source: &'s str,
    script: Language,
    unit_id: usize,
    root: bool,
}

/// `None` when an expression in the unit is too deep to measure.
fn measure_template_unit(
    node: Node<'_>,
    source: &str,
    script: Language,
    root: bool,
) -> Option<FunctionMetrics> {
    let walk = TemplateWalk {
        source,
        script,
        unit_id: node.id(),
        root,
    };
    let mut score = Score {
        complexity: 1,
        depth: 0,
        bool_ops: 0,
    };
    walk.score(node, 0, &mut score)?;
    let mut unit = template_metrics(template_unit_name(node, source), node, &score);
    if root {
        unit.line = 1;
        unit.end_line = source.lines().count().max(1);
    }
    unit.cognitive = cognitive::measure_template_unit(node, source, script, root);
    Some(unit)
}

fn template_unit_name(node: Node<'_>, source: &str) -> String {
    match node.kind() {
        "if_statement" => "{#if}".to_owned(),
        "each_statement" => "{#each}".to_owned(),
        "await_statement" => "{#await}".to_owned(),
        "snippet_statement" => {
            let name = node
                .named_child(0)
                .and_then(|start| child_of_kind(start, "snippet_name"))
                .map_or("", |name| node_text(name, source));
            format!("{{#snippet {name}}}")
        }
        _ => "<template>".to_owned(),
    }
}

fn template_metrics(function: String, node: Node<'_>, score: &Score) -> FunctionMetrics {
    FunctionMetrics {
        function,
        line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        complexity: score.complexity,
        cognitive: 0,
        depth: score.depth,
        lines: 0,
        params: 0,
        bool_ops: score.bool_ops,
        widget_depth: 0,
        span: (node.start_byte(), node.end_byte()),
        template: true,
        widget: false,
    }
}

impl TemplateWalk<'_> {
    /// Leaves out scripts, styles, and the units inside this one, the same
    /// parts `svelte_units` leaves out of the unit's height.
    fn score(&self, node: Node<'_>, depth: usize, score: &mut Score) -> Option<()> {
        match node.kind() {
            "script_element" | "style_element" => return Some(()),
            "snippet_statement" if node.id() != self.unit_id => return Some(()),
            kind if self.root && TEMPLATE_BLOCKS.contains(&kind) => return Some(()),
            "if_start" | "else_if_start" | "each_start" | "await_start" | "catch_start" => {
                score.complexity += 1;
            }
            "svelte_raw_text" => {
                let expression = score_expression(node, self.source, self.script)?;
                score.complexity += expression.complexity;
                score.bool_ops = score.bool_ops.max(expression.bool_ops);
            }
            _ => {}
        }
        let next_depth = depth + usize::from(TEMPLATE_BLOCKS.contains(&node.kind()));
        score.depth = score.depth.max(next_depth);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.score(child, next_depth, score)?;
        }
        Some(())
    }
}

/// How a template text is parsed: inside a function, so the script grammar
/// accepts it, in the position its role in the Svelte tree gives it.
#[derive(Clone, Copy)]
pub(crate) enum Wrapper {
    /// `return (…)`: an expression.
    Expression,
    /// `return […]`: the text of a spread attribute (`{...props}`), which
    /// parses only as an array element.
    Spread,
    /// Formal parameters: snippet parameters, and the patterns of `{#each}`,
    /// `{:then}`, and `{:catch}` bindings.
    Parameters,
    /// `const …;`: the declaration of a `{@const}` tag.
    Declaration,
}

impl Wrapper {
    /// The wrapped source and the byte range of `text` in it.
    pub(crate) fn wrap(self, text: &str) -> (String, Range<usize>) {
        let (head, tail) = match self {
            Self::Expression => ("function expression() { return (", "); }"),
            Self::Spread => ("function expression() { return [", "]; }"),
            Self::Parameters => ("function expression(", ") {}"),
            Self::Declaration => ("function expression() { const ", "; }"),
        };
        (
            format!("{head}{text}{tail}"),
            head.len()..head.len() + text.len(),
        )
    }
}

/// The wrappers to try for a template text, in order, and the one whose
/// error-recovered tree is scored when none parses cleanly. An `{#each}`
/// binding can end with a key, `item (item.id)`, which parses only as a call.
fn template_wrappers(node: Node<'_>) -> (&'static [Wrapper], Wrapper) {
    use Wrapper::{Declaration, Expression, Parameters, Spread};
    let Some(parent) = node.parent() else {
        return (&[Expression, Parameters, Declaration], Expression);
    };
    let roles: &'static [Wrapper] = match parent.kind() {
        "attribute_name" => return (&[Spread], Spread),
        "snippet_start" | "then_start" | "catch_start" => &[Parameters],
        "each_start"
            if parent
                .child_by_field_name("parameter")
                .is_some_and(|parameter| parameter.id() == node.id()) =>
        {
            &[Parameters, Expression]
        }
        "const_tag" => &[Declaration],
        "if_start" | "else_if_start" | "each_start" | "await_start" | "key_start"
        | "expression" | "html_tag" | "render_tag" | "debug_tag" => &[Expression],
        _ => &[Expression, Parameters, Declaration],
    };
    (roles, Expression)
}

/// Decisions (`complexity` without its base 1) and the widest boolean chain in
/// one template text, parsed with the component's script grammar in the first
/// of its wrappers that parses without `ERROR` or `MISSING` nodes, or else in
/// its fallback wrapper. Functions in the text count inline; nodes inside an
/// `ERROR` subtree add nothing. `None` when the text's own subtree is higher
/// than `MAX_SYNTAX_DEPTH`; the wrapper's levels do not count.
fn score_expression(node: Node<'_>, source: &str, script: Language) -> Option<Score> {
    let text = node_text(node, source);
    let grammar = script.grammar();
    let (wrappers, fallback) = template_wrappers(node);
    let parse = |wrapper: Wrapper| {
        let (wrapped, range) = wrapper.wrap(text);
        parse_tree(&grammar, &wrapped)
            .ok()
            .map(|tree| (tree, wrapped, range))
    };
    let (tree, wrapped, range) = wrappers
        .iter()
        .filter_map(|&wrapper| parse(wrapper))
        .find(|(tree, _, _)| !tree.root_node().has_error())
        .or_else(|| parse(fallback))?;
    if exceeds_depth_limit(&tree, range) {
        return None;
    }
    let mut score = Score {
        complexity: 0,
        depth: 0,
        bool_ops: 0,
    };
    score_expression_node(tree.root_node(), &wrapped, script, &mut score);
    Some(score)
}

fn score_expression_node(node: Node<'_>, source: &str, script: Language, score: &mut Score) {
    if node.is_error() {
        return;
    }
    let operator = is_boolean_operator(node, script, source);
    score.complexity += usize::from(operator || is_conditional_expression(node.kind()));
    if is_boolean_chain_root(node, script, source) {
        score.bool_ops =
            score
                .bool_ops
                .max(template_chain_operators(node, node.id(), script, source));
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        score_expression_node(child, source, script, score);
    }
}

/// `boolean_operators_in_chain` for a template expression, which also skips
/// `ERROR` subtrees.
fn template_chain_operators(
    node: Node<'_>,
    root_id: usize,
    script: Language,
    source: &str,
) -> usize {
    if node.is_error()
        || node.id() != root_id
            && (is_function(script, node.kind()) || is_conditional_expression(node.kind()))
    {
        return 0;
    }
    let mut cursor = node.walk();
    usize::from(is_boolean_operator(node, script, source))
        + node
            .named_children(&mut cursor)
            .map(|child| template_chain_operators(child, root_id, script, source))
            .sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_depth(language: Language, source: &str) -> usize {
        let grammar = if language == Language::Svelte {
            tree_sitter_svelte_ng::LANGUAGE.into()
        } else {
            language.grammar()
        };
        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut cursor = tree.walk();
        let (mut depth, mut deepest) = (0, 0);
        loop {
            if cursor.goto_first_child() {
                depth += 1;
                deepest = deepest.max(depth);
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    return deepest;
                }
                depth -= 1;
            }
        }
    }

    /// Sources whose unit sits exactly at the height limit and one level over
    /// it, for a shape that adds one level per repetition. The unit's node is
    /// `unit_depth` levels below the root, and the deepest node is in the unit.
    fn at_and_over_limit(
        language: Language,
        unit_depth: usize,
        shape: impl Fn(usize) -> String,
    ) -> [String; 2] {
        let height = |source: &str| tree_depth(language, source) - unit_depth;
        let offset = height(&shape(10)) - 10;
        let at = shape(MAX_SYNTAX_DEPTH - offset);
        let over = shape(MAX_SYNTAX_DEPTH - offset + 1);
        assert_eq!(height(&at), MAX_SYNTAX_DEPTH);
        assert_eq!(height(&over), MAX_SYNTAX_DEPTH + 1);
        [at, over]
    }

    #[test]
    fn depth_limit_holds_per_unit_on_a_one_mebibyte_caller_stack() {
        let chain = |terms: usize| format!("function f(a) {{ return {}a; }}", "a + ".repeat(terms));
        let ifs =
            |levels: usize| format!("function f(a) {{ {}{} }}", "if (a) ".repeat(levels), "a;");
        // Nested elements need the most stack per level of any shape measured.
        let elements =
            |levels: usize| format!("{}x{}", "<b>".repeat(levels), "</b>".repeat(levels));
        let [chain_at, chain_over] = at_and_over_limit(Language::JavaScript, 1, chain);
        let [ifs_at, ifs_over] = at_and_over_limit(Language::JavaScript, 1, ifs);
        let [elements_at, elements_over] = at_and_over_limit(Language::Svelte, 0, elements);
        // Next to a unit over the limit: a closure inside it, a function after
        // it, a script function, and a block after a deep template expression,
        // which leaves out the root template unit.
        let script = format!(
            "{}\nfunction g(a) {{ return a; }}",
            chain_over.replace("{ return", "{ const ok = (b) => b; return")
        );
        // A template expression's own subtree counts, not its wrapper: a
        // chain of `n + 1` terms is `n` levels high.
        let expression =
            |levels: usize| format!("<p>{{{}a}}</p>{{#if a}}{{/if}}", "a + ".repeat(levels));
        let expression_at = expression(MAX_SYNTAX_DEPTH);
        let expression_over = expression(MAX_SYNTAX_DEPTH + 1);
        let component = format!("<script>{script}</script>{expression_over}");
        let cases = [
            (Language::JavaScript, chain_at, vec!["f"], false),
            (Language::JavaScript, chain_over, vec![], true),
            (Language::JavaScript, ifs_at, vec!["f"], false),
            (Language::JavaScript, ifs_over, vec![], true),
            (Language::Svelte, elements_at, vec!["<template>"], false),
            (Language::Svelte, elements_over, vec![], true),
            (
                Language::Svelte,
                expression_at,
                vec!["{#if}", "<template>"],
                false,
            ),
            (Language::Svelte, expression_over, vec!["{#if}"], true),
            (Language::JavaScript, script, vec!["ok", "g"], true),
            (Language::Svelte, component, vec!["ok", "g", "{#if}"], true),
        ];
        let outcome = std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(move || {
                on_measurement_stack(move || {
                    cases
                        .into_iter()
                        .map(|(language, source, names, deep)| {
                            let parsed = parse_source(language, &source).unwrap();
                            let found = parsed
                                .functions
                                .iter()
                                .map(|unit| unit.function.clone())
                                .collect::<Vec<_>>();
                            (found, parsed.reason(), names, deep)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap()
            })
            .unwrap()
            .join()
            .unwrap();
        for (index, (found, reason, names, deep)) in outcome.into_iter().enumerate() {
            assert_eq!(found, names, "case {index}");
            let expected = deep.then_some("syntax nesting too deep to measure");
            assert_eq!(reason.as_deref(), expected, "case {index}");
        }
    }

    #[test]
    fn current_grammars_have_no_unclassified_control_flow_candidates() {
        for (language, kinds) in coverage_unknowns() {
            assert!(kinds.is_empty(), "{language}: {}", kinds.join(", "));
        }
    }

    #[test]
    fn operators_use_grammar_tokens_only() {
        let functions = parse_source(
            Language::JavaScript,
            "function nested(a,b,c){ return (a && b) + c; }\nfunction text(a){ return a + '||'; }",
        )
        .unwrap()
        .functions;
        assert_eq!(functions[0].complexity, 2);
        assert_eq!(functions[1].complexity, 1);
        assert_eq!(functions[0].bool_ops, 1);
        assert_eq!(functions[1].bool_ops, 0);
    }

    #[test]
    fn boolean_chains_cross_parentheses_but_not_ternaries_or_closures() {
        let cases = [
            (
                Language::JavaScript,
                "function f(a,b,c,d,e){ return a && (b || c) && (() => d || e)(); }",
            ),
            (
                Language::Dart,
                "bool f(a,b,c,d,e) => a && (b || c) && (() => d || e)();",
            ),
            (
                Language::Rust,
                "fn f(a:bool,b:bool,c:bool,d:bool,e:bool)->bool { a && (b || c) && (|| d || e)() }",
            ),
            (
                Language::Python,
                "def f(a,b,c,d,e):\n    return a and (b or c) and (lambda: d or e)()\n",
            ),
            (
                Language::Go,
                "func f(a,b,c,d,e bool) bool { return a && (b || c) && func() bool { return d || e }() }",
            ),
        ];
        for (language, source) in cases {
            let functions = parse_source(language, source).unwrap().functions;
            assert_eq!(functions[0].bool_ops, 3, "{}", language.name());
            assert_eq!(functions[1].bool_ops, 1, "{} closure", language.name());
        }

        let ternary = parse_source(
            Language::TypeScript,
            "function f(a:boolean,b:boolean,c:boolean,d:boolean,e:boolean){ return (a && b) ? (c || d) : e; }",
        )
        .unwrap()
        .functions;
        assert_eq!(ternary[0].bool_ops, 1);
    }

    #[test]
    fn dart_widget_depth_follows_only_widget_slots_in_build_methods() {
        let functions = parse_source(
            Language::Dart,
            r#"
class Screen {
  Widget build(BuildContext context) => Column(children: [
    Padding(padding: EdgeInsets.all(8), child: Builder(
      builder: (context) => Theme.of(context).enabled
          ? Center(child: Text('ok'))
          : const SizedBox(),
    )),
    DecoratedBox(decoration: BoxDecoration(borderRadius: BorderRadius.circular(4))),
  ]);
  Widget helper() => Column(child: Text('no'));
}
"#,
        )
        .unwrap()
        .functions;
        assert_eq!(functions[0].widget_depth, 5);
        assert_eq!(functions[1].widget_depth, 0);
        assert_eq!(functions[2].widget_depth, 0);
    }

    #[test]
    fn else_if_chains_and_try_handlers_share_depth() {
        let javascript = parse_source(
            Language::JavaScript,
            "function chain(x){ if(x===1){} else if(x===2){} else if(x===3){} else{} try{}catch(e){} }",
        )
        .unwrap()
        .functions;
        let rust = parse_source(
            Language::Rust,
            "fn chain(x:u8){ if x==1 {} else if x==2 {} else if x==3 {} else {} }",
        )
        .unwrap()
        .functions;
        let python = parse_source(
            Language::Python,
            "def guarded():\n    try:\n        pass\n    except Exception:\n        pass\n    finally:\n        pass\n",
        )
        .unwrap()
        .functions;
        assert_eq!((javascript[0].complexity, javascript[0].depth), (5, 1));
        assert_eq!((rust[0].complexity, rust[0].depth), (4, 1));
        assert_eq!(python[0].depth, 1);
    }

    #[test]
    fn unbraced_else_control_flow_opens_its_own_level() {
        let javascript = parse_source(
            Language::JavaScript,
            "function branches(x) { if (x) {} else for (;;) { while (x) { if (x) {} } } if (x) {} else try { while (x) { if (x) {} } } catch {} if (x) {} else switch (x) { case 1: while (x) { if (x) {} } } }",
        )
        .unwrap()
        .functions;
        let dart = parse_source(
            Language::Dart,
            "void branches(int x) { if (x > 0) {} else for (;;) { while (x > 0) { if (x > 0) {} } } if (x > 0) {} else try { while (x > 0) { if (x > 0) {} } } catch (_) {} if (x > 0) {} else switch (x) { case 1: while (x > 0) { if (x > 0) {} } } }",
        )
        .unwrap()
        .functions;
        assert_eq!(javascript[0].depth, 4);
        assert_eq!(dart[0].depth, 4);
    }

    #[test]
    fn callbacks_are_anonymous_and_single_arrow_parameter_counts() {
        let functions = parse_source(
            Language::JavaScript,
            "function outer(list){ const mapped = list.map(x => x + 1); return mapped; }",
        )
        .unwrap()
        .functions;
        assert_eq!(functions[0].function, "outer");
        assert_eq!(functions[1].function, "<anonymous>");
        assert_eq!(functions[1].params, 1);
    }

    #[test]
    fn svelte_expressions_are_scored_from_the_syntax_tree() {
        let template = |source: &str| {
            let functions = parse_source(Language::Svelte, source).unwrap().functions;
            let unit = functions.last().unwrap();
            assert_eq!(unit.function, "<template>");
            (unit.complexity, unit.bool_ops)
        };
        assert_eq!(template("<p>{a && b /* || */}</p>"), (2, 1));
        assert_eq!(template("<p>{a || b // && ??\n}</p>"), (2, 1));
        assert_eq!(template("<p>{/a?b?c||d/.test(x)}</p>"), (1, 0));
        assert_eq!(template("<p>{x?.y ?? (a ? b : c)}</p>"), (3, 1));
        assert_eq!(template("<p>{a && b && c} {d || e}</p>"), (4, 2));
        assert_eq!(
            template("<button on:click={() => ok && go()}>x</button>"),
            (2, 1)
        );
        let block = |source: &str| {
            let functions = parse_source(Language::Svelte, source).unwrap().functions;
            assert_eq!(functions[0].function, "{#each}");
            (functions[0].complexity, functions[0].bool_ops)
        };
        assert_eq!(
            block("{#each foo as item (a && b && c && d && e)}{item}{/each}"),
            (6, 4)
        );
        // The grammar splits `has` at `as`; the leading `&&` is an `ERROR`.
        assert_eq!(
            block("{#each has && a && b && c && d && list as item}{item}{/each}"),
            (6, 4)
        );
        assert_eq!(
            block("{#each items as { x = \"a && b && c && d\" }}{x}{/each}"),
            (2, 0)
        );
        // TypeScript reads this `?` as a conditional type, JavaScript as a
        // ternary.
        let conditional_type = "<p>{a as (T extends U ? X : Y)}</p>";
        assert_eq!(template(conditional_type), (2, 0));
        assert_eq!(
            template(&format!("<script lang=\"ts\"></script>{conditional_type}")),
            (1, 0)
        );
    }

    #[test]
    fn svelte_template_texts_are_parsed_in_their_role() {
        let unit = |script: &str, template: &str| {
            let source = format!("<script{script}></script>\n{template}");
            let functions = parse_source(Language::Svelte, &source).unwrap().functions;
            let unit = functions.iter().find(|unit| unit.line == 2).unwrap();
            (unit.function.clone(), unit.complexity, unit.bool_ops)
        };
        let chain = "a && b && c && d && e";
        for (script, typed) in [(" lang=\"ts\"", ": boolean"), ("", "")] {
            let cases = [
                (
                    format!("{{#snippet f(x{typed} = {chain})}}{{x}}{{/snippet}}"),
                    "{#snippet f}",
                    5,
                ),
                (
                    format!("{{#if ok}}{{@const x{typed} = {chain}}}{{x}}{{/if}}"),
                    "{#if}",
                    6,
                ),
                (
                    format!("{{#each a as {{ x = {chain} }}}}{{x}}{{/each}}"),
                    "{#each}",
                    6,
                ),
                (
                    format!("{{#each a as {{ x = {chain} }}, i}}{{x}}{{/each}}"),
                    "{#each}",
                    6,
                ),
                (
                    format!("{{#await p}}{{:then {{ x = {chain} }}}}{{x}}{{/await}}"),
                    "{#await}",
                    6,
                ),
                (
                    format!("{{#await p}}{{:catch {{ x = {chain} }}}}{{x}}{{/await}}"),
                    "{#await}",
                    7,
                ),
            ];
            for (template, name, decisions) in cases {
                let (found, complexity, bool_ops) = unit(script, &template);
                assert_eq!(found, name, "{template}");
                assert_eq!(bool_ops, 4, "{script} {template}");
                assert_eq!(complexity, decisions, "{script} {template}");
            }
        }
        // A key after a pattern parses only as a call, so a binding with a key
        // falls back to the expression wrapper: a plain pattern still counts,
        // a destructuring default inside an `ERROR` adds nothing.
        assert_eq!(
            unit("", &format!("{{#each a as x, i ({chain})}}{{x}}{{/each}}")),
            ("{#each}".to_owned(), 6, 4)
        );
        assert_eq!(
            unit("", "{#each a as { x = b && c }, i (k)}{x}{/each}"),
            ("{#each}".to_owned(), 2, 0)
        );
    }

    #[test]
    fn svelte_scripts_come_from_script_elements() {
        let source = "<!-- <script>function hidden(a) { return a; }</script> -->\n\
            <script context=\"module\" lang='typescript'>\nfunction first(a: number) { return a; }\n</script>\n\
            <script module>\nfunction second(a) { return a; }\n</script>\n\
            <script lang=ts>\nfunction third(a: number) { return a; }\n</script>\n\
            <script>\nfunction fourth(a, b) { return a; }\n</script>\n";
        let functions = parse_source(Language::Svelte, source).unwrap().functions;
        let names = functions
            .iter()
            .map(|item| (item.function.as_str(), item.line, item.params))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                ("<template>", 1, 0),
                ("first", 3, 1),
                ("second", 6, 1),
                ("third", 9, 1),
                ("fourth", 12, 2),
            ]
        );
        let first = &functions[1];
        assert_eq!(
            &source[first.span.0..first.span.1],
            "function first(a: number) { return a; }"
        );

        // A string such as "</scripts>" splits the content into `raw_text` and
        // an `ERROR` node. Inside a `script_element`, the content up to the
        // end tag is still measured; outside one, the file is incomplete.
        let recovered =
            "<script>const t = \"</scripts>\"; function after(a) {}</script>\n<style>.a{}</style>";
        let recovered = parse_source(Language::Svelte, recovered).unwrap();
        assert_eq!(recovered.functions[0].function, "after");
        assert!(recovered.unmeasured.is_empty());
        let damaged = "<script>const t = \"</scripture>\"; function lost(a) { return a; }</script>\n\
            <script module>function kept(a) { return a; }</script>";
        let damaged = parse_source(Language::Svelte, damaged).unwrap();
        let names = damaged
            .functions
            .iter()
            .map(|item| item.function.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["<template>", "kept"]);
        assert_eq!(damaged.unmeasured, [Unmeasured::ScriptBlock]);
    }

    #[test]
    fn wildcard_arms_and_svelte_strings_are_exact() {
        let dart = parse_source(
            Language::Dart,
            "int choose(int x) => switch (x) { 1 => 1, _ => 0 };",
        )
        .unwrap()
        .functions;
        let rust = parse_source(
            Language::Rust,
            "fn choose(default_value:u8)->u8 { match default_value { 0 => 1, default_value => 2 } }",
        )
        .unwrap()
        .functions;
        let guarded_dart = parse_source(
            Language::Dart,
            "int guarded(int v) => switch (v) { _ when v > 0 => 1, _ => 2 };",
        )
        .unwrap()
        .functions;
        let named_dart = parse_source(
            Language::Dart,
            "int named(int defaultValue) => switch (defaultValue) { defaultValue => 1 };",
        )
        .unwrap()
        .functions;
        let svelte = parse_source(
            Language::Svelte,
            "<p>{\"question? && ||\"} {`s: ${ready && v}`}</p>",
        )
        .unwrap()
        .functions;
        assert_eq!(dart[0].complexity, 2);
        assert_eq!(guarded_dart[0].complexity, 2);
        assert_eq!(named_dart[0].complexity, 2);
        assert_eq!(rust[0].complexity, 3);
        assert_eq!(svelte[0].complexity, 2);
    }
}
