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
use crate::semantic::text::offset_to_position;

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
    Some(match kind {
        k::KEYWORD => KEYWORD,
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
    match node.kind() {
        // `fn::foo` at `DEFINE FUNCTION fn::foo` is a declaration; any
        // non-`fn::` callee is a builtin from the standard library.
        k::FUNCTION_NAME => {
            if parent_kind == Some(k::DEFINE_STATEMENT) {
                MOD_DECLARATION
            } else if node
                .utf8_text(source.as_bytes())
                .is_ok_and(|text| !text.trim().starts_with("fn::"))
            {
                MOD_DEFAULT_LIBRARY
            } else {
                0
            }
        }
        // A `$var` is a declaration at its binding site: function params
        // and `LET` bindings wrap it in `ParamDefinition`; `DEFINE PARAM`
        // places it directly under the `DefineStatement`.
        k::VARIABLE_NAME => {
            if matches!(parent_kind, Some(k::PARAM_DEFINITION | k::DEFINE_STATEMENT)) {
                MOD_DECLARATION
            } else {
                0
            }
        }
        _ => 0,
    }
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
pub fn collect_semantic_tokens(tree: &Tree, source: &str) -> Vec<SemanticToken> {
    encode(collect_absolute(tree, source))
}

/// Semantic tokens for a single `range` of the document. Any token that
/// overlaps the range is included whole (tokens are never split at the
/// range boundary).
pub fn collect_semantic_tokens_range(
    tree: &Tree,
    source: &str,
    range: Range,
) -> Vec<SemanticToken> {
    let tokens = collect_absolute(tree, source)
        .into_iter()
        .filter(|token| overlaps(token, &range))
        .collect();
    encode(tokens)
}

/// Walk the cached `tree` and gather its tokens as absolute positions,
/// sorted by (line, start).
fn collect_absolute(tree: &Tree, source: &str) -> Vec<AbsToken> {
    let mut tokens = Vec::new();
    walk(tree.root_node(), source, &mut tokens);

    // Tree order is already top-to-bottom, but comments (grammar extras)
    // can reattach out of order, so sort — the delta encoding below
    // assumes non-decreasing positions.
    tokens.sort_by(|a, b| (a.line, a.start_char).cmp(&(b.line, b.start_char)));
    tokens
}

fn walk(node: Node<'_>, source: &str, out: &mut Vec<AbsToken>) {
    if let Some(token_type) = token_type(node.kind()) {
        push_node(node, source, token_type, modifiers(node, source), out);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, source, out);
    }
}

/// Emit one [`AbsToken`] per line the node spans (a single token for the
/// common single-line case). Newlines are ASCII `\n` and never appear
/// inside a multi-byte UTF-8 sequence, so byte scanning is safe.
fn push_node(
    node: Node<'_>,
    source: &str,
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
            push_span(source, line_start, i, token_type, modifiers, out);
            line_start = i + 1;
        }
        i += 1;
    }
    push_span(source, line_start, end, token_type, modifiers, out);
}

/// Push a single-line span `[start, end)`. Lengths and character offsets
/// are counted in UTF-16 code units, as the protocol requires.
fn push_span(
    source: &str,
    start: usize,
    end: usize,
    token_type: u32,
    modifiers: u32,
    out: &mut Vec<AbsToken>,
) {
    if start >= end {
        return;
    }
    let position = offset_to_position(source, start);
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
