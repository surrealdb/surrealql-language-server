//! Semantic tokens: map tree-sitter node kinds onto the **standard** LSP
//! `SemanticTokenType` / `SemanticTokenModifier` legend so any editor
//! (VS Code, Monaco, …) themes `.surql` with zero per-language
//! configuration.
//!
//! The server already parses every document with the SurrealQL
//! tree-sitter grammar for diagnostics/analysis; here we re-parse the
//! source and do a single descent, emitting one token per highlighted
//! node. Two modifiers are derived purely from tree shape:
//!
//! * `declaration` — the defining occurrence of a symbol (a function
//!   name in `DEFINE FUNCTION`, a `$param`/`$var` binding site).
//! * `defaultLibrary` — a builtin function (`math::abs`, `string::len`)
//!   as opposed to a user `fn::…` function.

use ls_types::{
    Range, SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend,
};
use tree_sitter::{Node, Tree};

use crate::semantic::node_kind as k;
use crate::semantic::text::LineIndex;

// Token-type legend indices. These MUST stay in lock-step with the order
// of `legend()` below — the protocol references token types by position.
const KEYWORD: u32 = 0;
const FUNCTION: u32 = 1;
const PARAMETER: u32 = 2;
const TYPE: u32 = 3;
const STRING: u32 = 4;
const NUMBER: u32 = 5;
const COMMENT: u32 = 6;
const VARIABLE: u32 = 7;

// Token-modifier bits. Likewise positional: bit `i` corresponds to the
// modifier at index `i` of `legend().token_modifiers`.
const MOD_DECLARATION: u32 = 1 << 0;
const MOD_DEFAULT_LIBRARY: u32 = 1 << 1;

/// The legend advertised at `initialize` time. Only standard LSP token
/// types and modifiers are used, so clients theme them out of the box.
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::KEYWORD,   // 0
            SemanticTokenType::FUNCTION,  // 1
            SemanticTokenType::PARAMETER, // 2
            SemanticTokenType::TYPE,      // 3
            SemanticTokenType::STRING,    // 4
            SemanticTokenType::NUMBER,    // 5
            SemanticTokenType::COMMENT,   // 6
            SemanticTokenType::VARIABLE,  // 7
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,     // bit 0
            SemanticTokenModifier::DEFAULT_LIBRARY, // bit 1
        ],
    }
}

/// Map a tree-sitter node kind to a legend index.
///
/// `Some(idx)` means "emit one token covering this whole node and stop
/// descending" — so composite leaves like `RecordId` are coloured as a
/// unit. `None` means "keep walking into the children". Container nodes
/// that merely *wrap* highlighted leaves (notably `ParamDefinition`,
/// which holds a `VariableName` and a `Type`) deliberately return `None`
/// so their parts are coloured individually.
fn token_type(kind: &str) -> Option<u32> {
    if kind == "Keyword" || kind.starts_with("keyword_") {
        return Some(KEYWORD);
    }
    Some(match kind {
        k::FUNCTION_NAME => FUNCTION,
        k::VARIABLE_NAME => PARAMETER,
        k::TYPE_NAME | k::TYPE => TYPE,
        k::STRING | k::FORMAT_STRING | k::REGEX => STRING,
        k::NUMBER | k::INT | k::FLOAT | k::DECIMAL | k::DURATION => NUMBER,
        k::COMMENT | k::BLOCK_COMMENT => COMMENT,
        // `table:id` record literals get their own colour via `variable`.
        k::RECORD_ID => VARIABLE,
        _ => return None,
    })
}

/// Derive the modifier bitset for an emitted node from its position in
/// the tree. Returns `0` for the common "plain reference" case.
fn modifiers(node: Node<'_>, source: &str) -> u32 {
    let parent_kind = node.parent().map(|parent| parent.kind());
    // True when the node sits directly under a `DEFINE <form>` statement.
    let defined_by = |form: &str| {
        parent_kind == Some(k::DEFINE_STATEMENT)
            && node
                .parent()
                .and_then(|parent| define_form(parent, source))
                .as_deref()
                == Some(form)
    };

    match node.kind() {
        k::FUNCTION_NAME => {
            let text = node
                .utf8_text(source.as_bytes())
                .ok()
                .map(str::trim)
                .unwrap_or_default();
            if defined_by("function") {
                MOD_DECLARATION
            } else if text.starts_with("fn::") {
                0
            } else {
                MOD_DEFAULT_LIBRARY
            }
        }
        // A `$var` is a declaration at its binding site: function and
        // closure parameters live in a `ParamDefinition`; `LET` and
        // `DEFINE PARAM` place it directly under their statement.
        k::VARIABLE_NAME => {
            if matches!(parent_kind, Some(k::PARAM_DEFINITION | k::LET_STATEMENT))
                || defined_by("param")
            {
                MOD_DECLARATION
            } else {
                0
            }
        }
        _ => 0,
    }
}

fn define_form(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| k::is_keyword(*child))
        .nth(1)
        .and_then(|child| child.utf8_text(source.as_bytes()).ok())
        .map(|text| text.trim().to_ascii_lowercase())
}

/// An absolute (pre-delta-encoding) token. LSP requires every token to
/// live on a single line, so multi-line nodes are split into one of
/// these per covered line before encoding.
struct AbsToken {
    line: u32,
    start_char: u32,
    length: u32,
    token_type: u32,
    modifiers: u32,
}

/// Full-document semantic tokens, delta-encoded per the LSP wire format.
/// `tree` is the cached parse of `source` (see [`DocumentAnalysis::tree`]).
///
/// [`DocumentAnalysis::tree`]: crate::semantic::types::DocumentAnalysis::tree
pub fn collect_semantic_tokens(tree: &Tree, source: &str, lines: &LineIndex) -> Vec<SemanticToken> {
    encode(collect_absolute(tree, source, lines))
}

/// Semantic tokens for a single `range` of the document. Any token that
/// overlaps the range is included whole (tokens are never split at the
/// range boundary).
pub fn collect_semantic_tokens_range(
    tree: &Tree,
    source: &str,
    lines: &LineIndex,
    range: Range,
) -> Vec<SemanticToken> {
    // Prune the walk to the requested bytes, then apply the same positional
    // filter as before. The pruning is deliberately coarse — it drops only
    // nodes that lie wholly outside the range — so the surviving token set is
    // identical to walking the whole tree and filtering. It is the walk that
    // gets cheaper, not the answer that changes.
    //
    // Before this, a viewport request cost exactly as much as a full-document
    // one: a 40-line window of a 3200-line file collected all 16000 tokens and
    // threw away 99 percent of them.
    let span = ByteSpan {
        start: lines.offset(source, range.start),
        end: lines.offset(source, range.end),
    };
    let tokens = collect_absolute_in(tree, source, lines, Some(span))
        .into_iter()
        .filter(|token| overlaps(token, &range))
        .collect();
    encode(tokens)
}

/// A half-open byte range of the document that the walk is restricted to.
#[derive(Clone, Copy)]
struct ByteSpan {
    start: usize,
    end: usize,
}

impl ByteSpan {
    /// True when `node` cannot contain a token inside this span.
    ///
    /// A node touching the span at one end only is kept: a token is emitted
    /// whole when it overlaps, and the positional filter decides the edges.
    fn excludes(&self, node: Node<'_>) -> bool {
        node.end_byte() <= self.start || node.start_byte() >= self.end
    }
}

/// Walk the cached `tree` and gather its tokens as absolute positions,
/// sorted by (line, start).
fn collect_absolute(tree: &Tree, source: &str, lines: &LineIndex) -> Vec<AbsToken> {
    collect_absolute_in(tree, source, lines, None)
}

fn collect_absolute_in(
    tree: &Tree,
    source: &str,
    lines: &LineIndex,
    span: Option<ByteSpan>,
) -> Vec<AbsToken> {
    let mut tokens = Vec::new();
    walk(tree.root_node(), source, lines, span, &mut tokens);

    // Tree order is already top-to-bottom, but comments (grammar extras)
    // can reattach out of order, so sort — the delta encoding below
    // assumes non-decreasing positions.
    tokens.sort_by(|a, b| (a.line, a.start_char).cmp(&(b.line, b.start_char)));
    tokens
}

fn walk(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
    span: Option<ByteSpan>,
    out: &mut Vec<AbsToken>,
) {
    if let Some(span) = span
        && span.excludes(node)
    {
        return;
    }

    if let Some(token_type) = token_type(node.kind()) {
        push_node(
            node,
            source,
            lines,
            token_type,
            modifiers(node, source),
            out,
        );
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, source, lines, span, out);
    }
}

/// Emit one [`AbsToken`] per line the node spans (a single token for the
/// common single-line case). Newlines are ASCII `\n` and never appear
/// inside a multi-byte UTF-8 sequence, so byte scanning is safe.
fn push_node(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
    token_type: u32,
    modifiers: u32,
    out: &mut Vec<AbsToken>,
) {
    let bytes = source.as_bytes();
    let end = node.end_byte();
    let mut line_start = node.start_byte();
    let mut i = line_start;
    while i < end {
        if bytes[i] == b'\n' {
            push_span(source, lines, line_start, i, token_type, modifiers, out);
            line_start = i + 1;
        }
        i += 1;
    }
    push_span(source, lines, line_start, end, token_type, modifiers, out);
}

/// Push a single-line span `[start, end)`. Lengths and character offsets
/// are counted in UTF-16 code units, as the protocol requires.
fn push_span(
    source: &str,
    lines: &LineIndex,
    start: usize,
    end: usize,
    token_type: u32,
    modifiers: u32,
    out: &mut Vec<AbsToken>,
) {
    if start >= end {
        return;
    }
    let position = lines.position(source, start);
    let length: u32 = source[start..end]
        .chars()
        .map(|ch| ch.len_utf16() as u32)
        .sum();
    out.push(AbsToken {
        line: position.line,
        start_char: position.character,
        length,
        token_type,
        modifiers,
    });
}

/// True when a single-line token intersects `range` (half-open at the
/// range end, matching how editors request viewport ranges).
fn overlaps(token: &AbsToken, range: &Range) -> bool {
    let token_start = (token.line, token.start_char);
    let token_end = (token.line, token.start_char + token.length);
    let range_start = (range.start.line, range.start.character);
    let range_end = (range.end.line, range.end.character);
    token_start < range_end && token_end > range_start
}

/// Delta-encode absolute tokens into the flat `[Δline, Δstart, len, type,
/// modifiers]` representation that [`SemanticTokens`] serialises.
fn encode(tokens: Vec<AbsToken>) -> Vec<SemanticToken> {
    let mut encoded = Vec::with_capacity(tokens.len());
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;
    for token in tokens {
        // Both subtractions below are unchecked `u32`. They are sound only
        // because `collect` sorted the tokens into non-decreasing position:
        // a hundred lines away, in a different function. Release builds have
        // overflow checks off, so breaking that invariant would not panic here;
        // it would silently emit garbage token positions and paint the file
        // wrong. Assert it where it is relied on.
        debug_assert!(
            (token.line, token.start_char) >= (prev_line, prev_start),
            "semantic tokens must be sorted before delta encoding: \
             ({prev_line}, {prev_start}) then ({}, {})",
            token.line,
            token.start_char,
        );

        let delta_line = token.line - prev_line;
        let delta_start = if delta_line == 0 {
            token.start_char - prev_start
        } else {
            token.start_char
        };
        encoded.push(SemanticToken {
            delta_line,
            delta_start,
            length: token.length,
            token_type: token.token_type,
            token_modifiers_bitset: token.modifiers,
        });
        prev_line = token.line;
        prev_start = token.start_char;
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{ByteSpan, collect_absolute, collect_absolute_in, encode, overlaps};
    use crate::grammar::language;
    use crate::semantic::text::LineIndex;
    use ls_types::{Position, Range};
    use tree_sitter::{Parser, Tree};

    fn parse(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser.set_language(&language()).expect("grammar loads");
        parser.parse(source, None).expect("parses")
    }

    /// The ranged pass as it was before pruning: walk everything, then filter.
    /// This is the specification for the pruned walk.
    fn unpruned_range(
        tree: &Tree,
        source: &str,
        lines: &LineIndex,
        range: Range,
    ) -> Vec<ls_types::SemanticToken> {
        let tokens = collect_absolute(tree, source, lines)
            .into_iter()
            .filter(|token| overlaps(token, &range))
            .collect();
        encode(tokens)
    }

    /// Pruning the walk must not change which tokens come back — only what it
    /// costs to find them. Checked over every window of a multi-statement
    /// document, including windows that start and end mid-statement.
    #[test]
    fn pruning_the_walk_does_not_change_the_tokens() {
        let sources = [
            "",
            "SELECT * FROM person;",
            "SELECT name, email FROM person WHERE age > 21;\nUPDATE person SET age = 30;\n",
            // A multi-line node (a block), so a window can cut through one.
            "DEFINE FUNCTION fn::f($a: int) {\n    LET $b = $a + 1;\n    RETURN $b;\n};\nSELECT 1;\n",
            // Comments are grammar extras and can reattach out of tree order.
            "-- a comment\nSELECT 1;\n/* block\n   comment */\nSELECT 2;\n",
            "SET sym = '₹';\nSELECT sym;\nSELECT '🚀';\n",
        ];

        for source in sources {
            let tree = parse(source);
            let lines = LineIndex::new(source);
            let line_count = source.split('\n').count() as u32;

            for start_line in 0..line_count + 1 {
                for end_line in start_line..line_count + 1 {
                    for end_char in [0u32, 3, 40] {
                        let range = Range::new(
                            Position::new(start_line, 0),
                            Position::new(end_line, end_char),
                        );
                        assert_eq!(
                            super::collect_semantic_tokens_range(&tree, source, &lines, range),
                            unpruned_range(&tree, source, &lines, range),
                            "pruned walk differs for {range:?} of {source:?}"
                        );
                    }
                }
            }
        }
    }

    /// The pruning must actually prune: a small window of a long document has
    /// to visit far fewer nodes than the whole tree.
    #[test]
    fn pruning_the_walk_collects_far_fewer_tokens() {
        let source = "SELECT name, email, age FROM person WHERE age > 21;\n".repeat(400);
        let tree = parse(&source);
        let lines = LineIndex::new(&source);
        let span = ByteSpan {
            start: lines.offset(&source, Position::new(0, 0)),
            end: lines.offset(&source, Position::new(40, 0)),
        };

        let all = collect_absolute(&tree, &source, &lines).len();
        let pruned = collect_absolute_in(&tree, &source, &lines, Some(span)).len();

        assert!(
            pruned * 5 < all,
            "pruning collected {pruned} of {all} tokens — not pruning enough"
        );
    }
}
