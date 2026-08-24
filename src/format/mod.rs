//! The SurrealQL formatter.
//!
//! Token-stream based, not a pretty-printer that rebuilds statements from the
//! tree. It walks the parse tree's leaves in order, emits each one verbatim,
//! and decides only what goes *between* them. That is a deliberate limit: it
//! cannot reflow a long line, and it also cannot lose, reorder or invent a
//! token, which is the property that matters most in a tool that rewrites
//! someone's schema.
//!
//! Comments are leaves like any other, so they survive by construction rather
//! than by a reattachment pass.

use tree_sitter::Node;

use crate::semantic::node_kind as k;

/// Two spaces, matching the SurrealQL in this repository's fixtures and docs.
const INDENT: &str = "  ";

/// Format `source`, or return it unchanged if it cannot be parsed cleanly.
///
/// Returning the input on a parse error is the central safety rule. A formatter
/// that rewrites text the parser did not understand destroys work, and the one
/// moment a user is most likely to hit format-on-save is mid-edit.
pub fn format(source: &str) -> String {
    let Some(tree) = parse(source) else {
        return source.to_string();
    };
    let root = tree.root_node();
    if has_error(root) {
        return source.to_string();
    }

    let mut tokens = Vec::new();
    collect_tokens(root, source, &mut tokens);
    if tokens.is_empty() {
        return source.to_string();
    }

    let rendered = render(&tokens);
    // Belt and braces: a format that no longer parses, or that changed the
    // tokens, is refused rather than returned. This cannot currently happen —
    // the renderer only changes whitespace — but the cost of being wrong here
    // is someone's file.
    match parse(&rendered) {
        Some(reparsed) if !has_error(reparsed.root_node()) => {
            let mut after = Vec::new();
            collect_tokens(reparsed.root_node(), &rendered, &mut after);
            let same = after.len() == tokens.len()
                && after
                    .iter()
                    .zip(tokens.iter())
                    .all(|(left, right)| left.text == right.text);
            if same { rendered } else { source.to_string() }
        }
        _ => source.to_string(),
    }
}

fn parse(source: &str) -> Option<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&crate::grammar::language()).ok()?;
    parser.parse(source, None)
}

fn has_error(node: Node<'_>) -> bool {
    node.has_error()
}

struct Token<'a> {
    text: &'a str,
    kind: &'a str,
    /// The kind of the node that owns this token.
    ///
    /// SurrealQL reuses punctuation across contexts: `:` separates a record id
    /// (`person:1`, tight) and introduces a type (`$x: string`, spaced); `->`
    /// is a graph hop (tight) and a return arrow (spaced); `<` and `>` are
    /// comparison operators (spaced) and type brackets (tight). The token text
    /// alone cannot tell them apart — the parent can.
    parent_kind: &'a str,
    /// Inside a record id, where every token binds tightly:
    /// `person:1`, `|item:1..10000|`. Written as an ancestor test rather than a
    /// list of token texts because the pieces — the pipes, the `..`, the
    /// colon — are ordinary punctuation everywhere else.
    in_record_id: bool,
    /// Blank lines the author left before this token, capped at one. Paragraph
    /// breaks are how a schema file is organised, so they are kept; a run of
    /// six is not.
    blank_line_before: bool,
}

fn collect_tokens<'a>(node: Node<'_>, source: &'a str, out: &mut Vec<Token<'a>>) {
    if node.child_count() == 0 {
        let text = &source[node.start_byte()..node.end_byte()];
        if text.is_empty() {
            return;
        }
        let gap_start = out
            .last()
            .map(|_| node.start_byte())
            .unwrap_or(node.start_byte());
        let preceding = &source[..gap_start];
        let blank_line_before = !out.is_empty()
            && preceding
                .chars()
                .rev()
                .take_while(|character| character.is_whitespace())
                .filter(|character| *character == '\n')
                .count()
                >= 2;
        out.push(Token {
            text,
            kind: node.kind(),
            parent_kind: node.parent().map(|parent| parent.kind()).unwrap_or(""),
            in_record_id: has_ancestor(node, &["RecordId", "RangeRecordId", "RecordIdRange"]),
            blank_line_before,
        });
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_tokens(child, source, out);
    }
}

fn has_ancestor(node: Node<'_>, kinds: &[&str]) -> bool {
    let mut current = node.parent();
    while let Some(node) = current {
        if kinds.contains(&node.kind()) {
            return true;
        }
        current = node.parent();
    }
    false
}

fn render(tokens: &[Token<'_>]) -> String {
    let mut out = String::new();
    let mut depth: usize = 0;
    let mut at_line_start = true;

    for (index, token) in tokens.iter().enumerate() {
        let previous = index.checked_sub(1).map(|index| &tokens[index]);

        if token.text == "}" {
            depth = depth.saturating_sub(1);
        }

        let newline = previous.is_some_and(|previous| breaks_line(previous, token));
        if newline {
            out.push('\n');
            if token.blank_line_before {
                out.push('\n');
            }
            at_line_start = true;
        }

        if at_line_start {
            for _ in 0..depth {
                out.push_str(INDENT);
            }
            at_line_start = false;
        } else if let Some(previous) = previous
            && needs_space(previous, token)
        {
            out.push(' ');
        }

        out.push_str(token.text);

        if token.text == "{" {
            depth += 1;
        }
    }
    out.push('\n');
    out
}

/// Whether a line break goes between two tokens.
fn breaks_line(previous: &Token<'_>, next: &Token<'_>) -> bool {
    // A statement ends at its semicolon.
    if previous.text == ";" {
        return true;
    }
    // Braces own their own lines, so a block reads as a block.
    if previous.text == "{" || next.text == "}" {
        return true;
    }
    // A line comment ends the line it is on, always: anything after it on the
    // same line would be swallowed by the comment.
    if previous.kind == k::COMMENT {
        return true;
    }
    // A block comment written above a statement stays above it.
    if previous.kind == k::BLOCK_COMMENT {
        return true;
    }
    // A comment written on its own line stays on its own line.
    next.kind == k::COMMENT && previous.text == ";"
}

/// Whether a single space goes between two tokens on the same line.
fn needs_space(previous: &Token<'_>, next: &Token<'_>) -> bool {
    // Nothing hugs the inside of an opening bracket, or the outside of a
    // closing one.
    if matches!(previous.text, "(" | "[") || matches!(next.text, ")" | "]" | "," | ";") {
        return false;
    }
    // Everything inside a record id is tight: `person:1`, `|item:1..10000|`.
    if previous.in_record_id && next.in_record_id {
        return false;
    }
    // Namespace and field access bind their neighbours: `string::len`,
    // `person.email`.
    if matches!(previous.text, "::" | "." | "$" | "@") || matches!(next.text, "::" | ".") {
        return false;
    }
    // A graph hop binds tightly to what it reaches: `->knows->person`. The
    // return arrow of a function signature is a different token in a different
    // parent, and keeps its spaces: `-> string`.
    if is_graph_arrow(previous) {
        return false;
    }
    // ...but a keyword before a hop still needs its space, or `SELECT ->knows`
    // becomes `SELECT->knows`.
    if is_graph_arrow(next) {
        return previous.kind == "Keyword";
    }
    // Type brackets hug their contents: `array<string>`. The opening bracket is
    // tight on its right and the closing one on its left; what follows a
    // closing bracket is ordinary text and keeps its space, so
    // `array<string> = []` reads correctly.
    if is_type_bracket(previous) && previous.text == "<" {
        return false;
    }
    if is_type_bracket(next) {
        return false;
    }
    // A record id is `person:1`. A type annotation is `$x: string` — no space
    // before the colon, one after.
    if previous.text == ":" {
        return previous.parent_kind != "RecordId";
    }
    if next.text == ":" {
        return false;
    }
    if next.text == "(" {
        // A call hugs its name; a keyword does not hug a parenthesised group.
        return previous.kind == "Keyword";
    }
    true
}

fn is_graph_arrow(token: &Token<'_>) -> bool {
    matches!(token.text, "->" | "<-" | "<~" | "~>") && token.parent_kind == k::LOOKUP
}

fn is_type_bracket(token: &Token<'_>) -> bool {
    matches!(token.text, "<" | ">") && token.parent_kind != "Operator"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_input_is_returned_unchanged() {
        let broken = "DEFINE TABLE @@@nonsense@@@;";
        assert_eq!(format(broken), broken);
    }

    #[test]
    fn formatting_is_idempotent() {
        let source = "DEFINE TABLE person SCHEMAFULL;\nDEFINE FIELD email ON person TYPE string;";
        let once = format(source);
        assert_eq!(format(&once), once);
    }

    #[test]
    fn every_comment_survives() {
        let source = "-- a leading note\nDEFINE TABLE person SCHEMAFULL; -- trailing\n";
        let formatted = format(source);
        assert!(formatted.contains("-- a leading note"));
        assert!(formatted.contains("-- trailing"));
    }

    #[test]
    fn a_blank_line_between_statements_survives() {
        let source = "DEFINE TABLE a SCHEMAFULL;\n\nDEFINE TABLE b SCHEMAFULL;\n";
        let formatted = format(source);
        assert!(
            formatted.contains("a SCHEMAFULL;\n\nDEFINE"),
            "paragraph breaks organise a schema file: {formatted:?}"
        );
    }

    #[test]
    fn a_run_of_blank_lines_collapses_to_one() {
        let source = "DEFINE TABLE a SCHEMAFULL;\n\n\n\n\nDEFINE TABLE b SCHEMAFULL;\n";
        assert!(!format(source).contains("\n\n\n"));
    }

    #[test]
    fn statements_get_one_line_each() {
        let source = "DEFINE TABLE a SCHEMAFULL; DEFINE TABLE b SCHEMAFULL;";
        let formatted = format(source);
        assert_eq!(formatted.lines().count(), 2, "{formatted:?}");
    }
}
