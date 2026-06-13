//! Semantic tokens: map tree-sitter node kinds onto the **standard** LSP
//! `SemanticTokenType` legend so any editor (VS Code, Monaco, …) themes
//! `.surql` with zero per-language configuration.
//!
//! The server already parses every document with the SurrealQL
//! tree-sitter grammar for diagnostics/analysis; here we re-parse the
//! source and do a single descent, emitting one token per highlighted
//! node. No token modifiers are produced in this first version.

use ls_types::{SemanticToken, SemanticTokenType, SemanticTokensLegend};
use tree_sitter::{Node, Parser};

use crate::grammar::language;
use crate::semantic::node_kind as k;
use crate::semantic::text::offset_to_position;

// Legend indices. These MUST stay in lock-step with the order of
// `legend()` below — the protocol references token types by position.
const KEYWORD: u32 = 0;
const FUNCTION: u32 = 1;
const PARAMETER: u32 = 2;
const TYPE: u32 = 3;
const STRING: u32 = 4;
const NUMBER: u32 = 5;
const COMMENT: u32 = 6;
const VARIABLE: u32 = 7;

/// The token-type legend advertised at `initialize` time. Only standard
/// LSP token types are used, so clients theme them out of the box.
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
        token_modifiers: Vec::new(),
    }
}

/// Map a tree-sitter node kind to a legend index.
///
/// `Some(idx)` means "emit one token covering this whole node and stop
/// descending" — so composite nodes like `RecordId` are coloured as a
/// unit and never double-counted against their children. `None` means
/// "keep walking into the children".
fn token_type(kind: &str) -> Option<u32> {
    Some(match kind {
        k::KEYWORD => KEYWORD,
        k::FUNCTION_NAME => FUNCTION,
        // Both the `$param` binding sites and their `$param` use sites.
        k::PARAM_DEFINITION | k::VARIABLE_NAME => PARAMETER,
        k::TYPE_NAME | k::TYPE => TYPE,
        k::STRING | k::FORMAT_STRING | k::REGEX => STRING,
        k::NUMBER | k::INT | k::FLOAT | k::DECIMAL | k::DURATION => NUMBER,
        k::COMMENT | k::BLOCK_COMMENT => COMMENT,
        // `table:id` record literals get their own colour via `variable`.
        k::RECORD_ID => VARIABLE,
        _ => return None,
    })
}

/// An absolute (pre-delta-encoding) token. LSP requires every token to
/// live on a single line, so multi-line nodes are split into one of
/// these per covered line before encoding.
struct AbsToken {
    line: u32,
    start_char: u32,
    length: u32,
    token_type: u32,
}

/// Parse `source` and return its semantic tokens, delta-encoded per the
/// LSP wire format. Returns an empty vec if the grammar fails to load or
/// the document fails to parse.
pub fn collect_semantic_tokens(source: &str) -> Vec<SemanticToken> {
    let mut parser = Parser::new();
    if parser.set_language(&language()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let mut tokens = Vec::new();
    walk(tree.root_node(), source, &mut tokens);

    // Tree order is already left-to-right / top-to-bottom, but extras
    // (comments) can be reattached out of order, so sort to be safe —
    // the delta encoding below assumes non-decreasing positions.
    tokens.sort_by(|a, b| (a.line, a.start_char).cmp(&(b.line, b.start_char)));
    encode(tokens)
}

fn walk(node: Node<'_>, source: &str, out: &mut Vec<AbsToken>) {
    if let Some(token_type) = token_type(node.kind()) {
        push_node(node, source, token_type, out);
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
fn push_node(node: Node<'_>, source: &str, token_type: u32, out: &mut Vec<AbsToken>) {
    let bytes = source.as_bytes();
    let end = node.end_byte();
    let mut line_start = node.start_byte();
    let mut i = line_start;
    while i < end {
        if bytes[i] == b'\n' {
            push_span(source, line_start, i, token_type, out);
            line_start = i + 1;
        }
        i += 1;
    }
    push_span(source, line_start, end, token_type, out);
}

/// Push a single-line span `[start, end)`. Lengths and character offsets
/// are counted in UTF-16 code units, as the protocol requires.
fn push_span(source: &str, start: usize, end: usize, token_type: u32, out: &mut Vec<AbsToken>) {
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
    });
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
            token_modifiers_bitset: 0,
        });
        prev_line = token.line;
        prev_start = token.start_char;
    }
    encoded
}
