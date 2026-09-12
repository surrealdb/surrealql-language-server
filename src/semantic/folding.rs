//! Folding ranges and selection ranges: two small structural reads of the
//! cached parse tree.
//!
//! Both are near-free ([`DocumentAnalysis`](crate::semantic::types::DocumentAnalysis)
//! already holds the tree, so neither re-parses), and both are what make an
//! editor feel native. Folding collapses a `DEFINE FUNCTION` body or a long
//! object literal; selection range is what expand-selection binds to in VS Code,
//! Zed, Helix and Neovim alike.
//!
//! Neither reads the *semantic* model, only the tree, so both answer for a
//! document whose semantic analysis was skipped.
//!
//! They answer *nothing* for a document the analyzer **refused**, though, and
//! that is not a bug to fix here: a refusal past the size or nesting cap stores
//! a parse of the empty string, since parsing the real text is precisely what
//! was declined. Walking that empty tree is what these functions then do. The
//! alternative, parsing a document specifically because it was too large or too
//! deeply nested to parse, is the cost the cap exists to avoid.

use ls_types::{FoldingRange, FoldingRangeKind, Position, Range, SelectionRange};
use tree_sitter::{Node, Tree};

use crate::semantic::limits;
use crate::semantic::node_kind as k;
use crate::semantic::text::LineIndex;

/// Node kinds worth a fold of their own.
///
/// Deliberately conservative: a fold for every nested expression makes the
/// gutter unreadable. These are the shapes a reader actually collapses: a whole
/// statement, a block body, and the two container literals that grow long.
const FOLDABLE: &[&str] = &[
    k::DEFINE_STATEMENT,
    k::SELECT_STATEMENT,
    k::CREATE_STATEMENT,
    k::UPDATE_STATEMENT,
    k::UPSERT_STATEMENT,
    k::DELETE_STATEMENT,
    k::RELATE_STATEMENT,
    k::INSERT_STATEMENT,
    k::FOR_STATEMENT,
    k::IF_ELSE_STATEMENT,
    k::BLOCK,
    k::OBJECT,
    k::ARRAY,
    k::JS_FUNCTION_BODY,
];

/// Every foldable region in `tree`, in source order.
///
/// A region folds only when it spans more than one line: a single-line object is
/// already as short as it gets, and offering to fold it is noise.
pub fn folding_ranges(tree: &Tree) -> Vec<FoldingRange> {
    let mut ranges = Vec::new();
    collect_folds(tree.root_node(), 0, &mut ranges);
    collect_comment_runs(tree, &mut ranges);
    ranges.sort_by_key(|range| (range.start_line, range.end_line));
    ranges
}

fn collect_folds(node: Node<'_>, depth: u32, out: &mut Vec<FoldingRange>) {
    if limits::too_deep(depth) {
        return;
    }

    if FOLDABLE.contains(&node.kind())
        && let Some(range) = multi_line_fold(node, None)
    {
        out.push(range);
    }

    if node.child_count() == 0 {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_folds(child, depth + 1, out);
    }
}

/// A fold covering `node`, or `None` when it fits on one line.
///
/// The last line is excluded so the closing brace stays visible when the region
/// is collapsed: the convention every editor's built-in folding follows.
fn multi_line_fold(node: Node<'_>, kind: Option<FoldingRangeKind>) -> Option<FoldingRange> {
    let start_line = node.start_position().row as u32;
    let end_line = node.end_position().row as u32;
    if end_line <= start_line {
        return None;
    }
    Some(FoldingRange {
        start_line,
        end_line: end_line - 1,
        kind,
        ..FoldingRange::default()
    })
}

/// Consecutive comment lines fold as one block.
///
/// A licence header or a long explanation above a `DEFINE` is the other thing
/// people collapse, and the grammar emits each comment line as its own node, so
/// they have to be joined here.
fn collect_comment_runs(tree: &Tree, out: &mut Vec<FoldingRange>) {
    let mut comments: Vec<(u32, u32)> = Vec::new();
    let mut cursor = tree.walk();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), k::COMMENT | k::BLOCK_COMMENT) {
            comments.push((
                node.start_position().row as u32,
                node.end_position().row as u32,
            ));
            continue;
        }
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    comments.sort_unstable();

    let mut index = 0;
    while index < comments.len() {
        let (start, mut end) = comments[index];
        let mut next = index + 1;
        // Adjacent means "the next comment starts on the line after this one
        // ends": a blank line between two comments makes them two blocks.
        while next < comments.len() && comments[next].0 == end + 1 {
            end = comments[next].1;
            next += 1;
        }
        if end > start {
            out.push(FoldingRange {
                start_line: start,
                end_line: end,
                kind: Some(FoldingRangeKind::Comment),
                ..FoldingRange::default()
            });
        }
        index = next;
    }
}

/// The nested selection chain at `position`: the node under the cursor, then
/// each ancestor out to the whole document.
///
/// This is what "expand selection" walks. Returning the chain rather than one
/// range is the protocol's design: the editor holds it and steps through.
pub fn selection_range(
    tree: &Tree,
    source: &str,
    lines: &LineIndex,
    position: Position,
) -> Option<SelectionRange> {
    let offset = lines.offset(source, position);
    let mut node = tree
        .root_node()
        .named_descendant_for_byte_range(offset, offset)
        .or_else(|| tree.root_node().descendant_for_byte_range(offset, offset))?;

    // Innermost first, then outward. Each step must be strictly larger, or an
    // editor's expand-selection appears stuck.
    let mut chain: Vec<Range> = vec![node_range(node, source, lines)];
    while let Some(parent) = node.parent() {
        let range = node_range(parent, source, lines);
        if Some(&range) != chain.last() {
            chain.push(range);
        }
        node = parent;
    }

    // Build outside-in so each range owns the next as its parent.
    let mut selection: Option<SelectionRange> = None;
    for range in chain.into_iter().rev() {
        selection = Some(SelectionRange {
            range,
            parent: selection.map(Box::new),
        });
    }
    selection
}

fn node_range(node: Node<'_>, source: &str, lines: &LineIndex) -> Range {
    lines.range(source, node.start_byte(), node.end_byte())
}
