//! Comment directives that silence a rule.
//!
//! ```surql
//! -- surql-ignore-file: permission-unknown, dynamic-target
//!
//! -- surql-ignore: unknown-field
//! CREATE person SET nickname = "b";
//!
//! CREATE person SET nickname = "b";  -- surql-ignore: unknown-field
//! ```
//!
//! All three comment openers the grammar accepts work — `--`, `//` and `#` —
//! and so does the `/* … */` block form, because the grammar folds every one
//! of them into the `Comment` and `BlockComment` node kinds.
//!
//! Directives are read from the **parse tree**, never by scanning lines. The
//! grammar makes comments `extras`, so they are real nodes, and a line scan
//! would read the string in `SELECT '-- surql-ignore: parse'` as a directive.

use ls_types::Range;
use tree_sitter::Node;

use crate::semantic::node_kind as k;
use crate::semantic::text::LineIndex;

/// The prefix that opens a directive, after the comment marker is stripped.
const LINE_DIRECTIVE: &str = "surql-ignore";
const FILE_DIRECTIVE: &str = "surql-ignore-file";

/// Which diagnostics a directive covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// One line, resolved when the directive is collected so matching later is
    /// a plain comparison.
    Line(u32),
    /// The whole document.
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suppression {
    pub scope: Scope,
    /// The rule ids this directive covers. Empty means every rule — the bare
    /// `-- surql-ignore` form.
    ///
    /// Ids are not validated here. An id that matches no rule simply matches no
    /// diagnostic, and `analysis.ruleSeverity` is where a misspelled id gets a
    /// did-you-mean. Validating here would need the registry in a module that
    /// otherwise only reads syntax.
    pub rules: Vec<String>,
    /// The directive comment's own range, for a future `unused-suppression`
    /// rule and a "remove this directive" fix.
    pub range: Range,
}

/// Every directive in one document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Suppressions {
    entries: Vec<Suppression>,
}

impl Suppressions {
    /// Collect every directive under `root`.
    ///
    /// One walk, done inside the per-edit analysis, so the cost lands on the
    /// work that is already off the reactor rather than on every publish.
    pub fn collect(root: Node<'_>, source: &str, lines: &LineIndex) -> Self {
        // Almost no document has a directive, and the walk visits every token
        // — 7.5 ms on a 3200-line file. One substring scan settles it first.
        // A false positive here (the text appears inside a string) only costs
        // the walk; the walk itself still reads directives from the tree.
        if !source.contains(LINE_DIRECTIVE) {
            return Self::default();
        }
        let mut entries = Vec::new();
        let first_statement_row = first_statement_row(root);
        walk(root, source, lines, first_statement_row, &mut entries);
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[Suppression] {
        &self.entries
    }

    /// Whether a diagnostic with `code` starting on `line` is suppressed.
    pub fn suppresses(&self, code: &str, line: u32) -> bool {
        self.matching(code, line).is_some()
    }

    /// The index of the directive that suppresses this diagnostic, if one does.
    ///
    /// Returned rather than a bare `bool` so the caller can tell which
    /// directives did work and report the ones that did not.
    pub fn matching(&self, code: &str, line: u32) -> Option<usize> {
        self.entries.iter().position(|entry| {
            let in_scope = match entry.scope {
                Scope::File => true,
                Scope::Line(row) => row == line,
            };
            in_scope && (entry.rules.is_empty() || entry.rules.iter().any(|rule| rule == code))
        })
    }
}

/// Walk every child, not only the named ones: a comment inside an `ERROR`
/// region can arrive unnamed, and that region is exactly where someone reaches
/// for a directive.
fn walk(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
    first_statement_row: Option<usize>,
    out: &mut Vec<Suppression>,
) {
    if matches!(node.kind(), k::COMMENT | k::BLOCK_COMMENT)
        && let Some(entry) = parse_directive(node, source, lines, first_statement_row)
    {
        out.push(entry);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, source, lines, first_statement_row, out);
    }
}

/// The row the first non-comment statement starts on, if the document has one.
///
/// A `surql-ignore-file` directive is only honoured above it. Below, the reader
/// would have to scroll past code to discover that the whole file is affected.
fn first_statement_row(root: Node<'_>) -> Option<usize> {
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .find(|child| !matches!(child.kind(), k::COMMENT | k::BLOCK_COMMENT))
        .map(|child| child.start_position().row)
}

fn parse_directive(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
    first_statement_row: Option<usize>,
) -> Option<Suppression> {
    let text = source.get(node.start_byte()..node.end_byte())?;
    let body = strip_comment_markers(text).trim();
    let rest = body
        .strip_prefix(FILE_DIRECTIVE)
        .map(|rest| (true, rest))
        .or_else(|| body.strip_prefix(LINE_DIRECTIVE).map(|rest| (false, rest)))?;
    let (is_file, rest) = rest;

    // `surql-ignoreX` is not a directive. Only a separator or the end of the
    // comment may follow the keyword.
    let rest = rest.trim_start();
    let rules = match rest.strip_prefix(':') {
        Some(list) => list
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .collect(),
        None if rest.is_empty() => Vec::new(),
        None => return None,
    };

    let row = node.start_position().row;
    let range = lines.range(source, node.start_byte(), node.end_byte());

    if is_file {
        // Below the first statement a file-wide directive is too easy to miss,
        // so it is not honoured there. It is not an error either — reporting it
        // needs a rule, which is a later change.
        return match first_statement_row {
            Some(first) if row > first => None,
            _ => Some(Suppression {
                scope: Scope::File,
                rules,
                range,
            }),
        };
    }

    let target = if trails_code(source, lines, node, row) {
        row
    } else {
        next_code_row(source, lines, row)?
    };
    Some(Suppression {
        scope: Scope::Line(target as u32),
        rules,
        range,
    })
}

/// True when something other than whitespace precedes the comment on its own
/// line — a trailing directive, which covers the line it sits on.
fn trails_code(source: &str, lines: &LineIndex, node: Node<'_>, row: usize) -> bool {
    let Some(line) = lines.line_text(source, row) else {
        return false;
    };
    let line_start = node.start_byte() - node.start_position().column;
    let before = &source[line_start..node.start_byte()];
    !before.trim().is_empty() && !line.trim().is_empty()
}

/// The next row holding something other than whitespace or a comment.
///
/// A directive can therefore sit above a run of other comments and still reach
/// the statement, which is what a reader expects when a fix-up note and a
/// directive share a header block.
fn next_code_row(source: &str, lines: &LineIndex, row: usize) -> Option<usize> {
    let mut candidate = row + 1;
    while candidate < lines.line_count() {
        let text = lines.line_text(source, candidate)?.trim();
        let is_blank_or_comment = text.is_empty()
            || text.starts_with("--")
            || text.starts_with("//")
            || text.starts_with('#')
            || text.starts_with("/*");
        if !is_blank_or_comment {
            return Some(candidate);
        }
        candidate += 1;
    }
    None
}

/// Remove whichever comment opener the author used, and the block closer.
fn strip_comment_markers(text: &str) -> &str {
    for opener in ["--", "//", "#"] {
        if let Some(rest) = text.strip_prefix(opener) {
            return rest;
        }
    }
    if let Some(rest) = text.strip_prefix("/*") {
        return rest.strip_suffix("*/").unwrap_or(rest);
    }
    text
}
