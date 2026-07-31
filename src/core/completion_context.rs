//! Cursor-context analysis used by the completion handler.
//!
//! These helpers decide which kind of completion list — statement-head
//! keywords, table names only, column names only, or the full list — makes
//! syntactic sense at the cursor. Extracted from the original `backend.rs` so
//! both the native and WASM dispatchers can call them through the core.
//!
//! Two styles live here, for two different jobs:
//!
//! * [`statement_words`] scans *forward* from the start of the document,
//!   tracking strings, comments and brackets, so it can find where the current
//!   statement begins and hand [`crate::core::statement_shape`] the words the
//!   author has committed to. State that a backward scan cannot recover —
//!   "is this `;` inside a string?" — is why it reads forward.
//! * [`is_table_name_context`] and [`column_completion_context`] scan
//!   *backwards* over a few characters to spot the nine keywords that open a
//!   table or column slot. They stay because the head table has no
//!   expression-consuming primitive: `SELECT math::sum(price) FROM |` needs the
//!   backward scan to stay table-only.

use ls_types::Position;

use crate::core::statement_shape::{SlotYield, head_slot};
use crate::semantic::text::position_to_offset;
use crate::semantic::types::{DocumentAnalysis, QueryFact};

/// Stands in for a string, number, or other literal that was consumed whole.
///
/// The head table counts word positions, so a literal has to occupy one slot
/// rather than vanish — otherwise `SHOW CHANGES FOR TABLE t SINCE '…' ` would
/// look like a shorter statement than it is. Not a legal SurrealQL keyword, so
/// it can never satisfy a literal rule element.
const LITERAL: &str = "\u{1}literal";

/// The complete words of the statement that holds the cursor.
///
/// *Complete* excludes a partial token the author is still typing, because the
/// caller already has that as the completion prefix: `INFO FOR RO|` yields
/// `["INFO", "FOR"]`, so the head table answers for the `INFO FOR` slot and
/// `RO` filters the result.
///
/// Returns `None` where the head table cannot reason about the position — the
/// cursor is inside a string, inside a comment, or inside an unclosed bracket.
/// Every one of those keeps the full completion list.
pub fn statement_words(source: &str, position: Position) -> Option<Vec<String>> {
    let offset = position_to_offset(source, position);
    let before = source.get(..offset)?;

    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    // One mark per open bracket, holding the word count when it opened, so a
    // closed group collapses to a single token instead of leaking its contents:
    // `DEFINE FUNCTION fn::x($a: int) ` must read as four words, not six.
    let mut marks: Vec<usize> = Vec::new();
    let mut chars = before.chars().peekable();

    while let Some(ch) = chars.next() {
        // A quoted run is one token. An unterminated one means the cursor is
        // inside it, where no keyword is legal.
        if matches!(ch, '\'' | '"') {
            flush(&mut current, &mut words);
            if !skip_string(&mut chars, ch) {
                return None;
            }
            words.push(LITERAL.to_string());
            continue;
        }
        if is_line_comment_start(ch, chars.peek().copied()) {
            flush(&mut current, &mut words);
            // A line comment that never ends before the cursor puts the cursor
            // inside it.
            if !skip_line_comment(&mut chars) {
                return None;
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            flush(&mut current, &mut words);
            if !skip_block_comment(&mut chars) {
                return None;
            }
            continue;
        }
        if matches!(ch, '(' | '[' | '{') {
            flush(&mut current, &mut words);
            marks.push(words.len());
            continue;
        }
        if matches!(ch, ')' | ']' | '}') {
            flush(&mut current, &mut words);
            if let Some(mark) = marks.pop() {
                words.truncate(mark);
            }
            words.push(LITERAL.to_string());
            continue;
        }
        // A statement terminator at the top level starts a new statement.
        if ch == ';' && marks.is_empty() {
            words.clear();
            current.clear();
            continue;
        }
        if is_word_char(ch) {
            current.push(ch);
            continue;
        }
        flush(&mut current, &mut words);
    }

    // Inside a bracket the head is already over and the legal set is open.
    if !marks.is_empty() {
        return None;
    }
    // `current` non-empty means the cursor sits mid-word: that trailing run is
    // the prefix, not a word the statement has committed to.
    Some(words)
}

/// The vocabulary legal at the cursor, or [`SlotYield::Expression`] when the
/// position is not a modelled statement head.
pub fn head_slot_at(source: &str, position: Position) -> SlotYield {
    match statement_words(source, position) {
        Some(words) => {
            let borrowed: Vec<&str> = words.iter().map(String::as_str).collect();
            head_slot(&borrowed)
        }
        None => SlotYield::Expression,
    }
}

fn flush(current: &mut String, words: &mut Vec<String>) {
    if !current.is_empty() {
        words.push(std::mem::take(current));
    }
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | ':' | '$' | '.' | '*' | '`' | '-')
}

fn is_line_comment_start(ch: char, next: Option<char>) -> bool {
    ch == '#' || (ch == '-' && next == Some('-')) || (ch == '/' && next == Some('/'))
}

/// Consumes to the closing quote. False when the run never closes.
fn skip_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, quote: char) -> bool {
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            chars.next();
            continue;
        }
        if ch == quote {
            return true;
        }
    }
    false
}

/// Consumes to the end of the line. False when the comment runs to the cursor.
fn skip_line_comment(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    for ch in chars.by_ref() {
        if ch == '\n' {
            return true;
        }
    }
    false
}

/// Consumes to `*/`. False when the comment runs to the cursor.
fn skip_block_comment(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut previous = ' ';
    for ch in chars.by_ref() {
        if previous == '*' && ch == '/' {
            return true;
        }
        previous = ch;
    }
    false
}

/// Returns true when the cursor is positioned in a SurrealQL slot that
/// only syntactically accepts a table name. Currently detects:
///
///   * `SELECT ... FROM |`               (single or comma-separated tables)
///   * `INSERT INTO |`
///   * `UPDATE |`
///   * `DELETE FROM |`
///
/// The check walks backwards from the cursor over (a) the partial
/// identifier being typed, then (b) any sequence of comma-separated
/// identifiers (so `FROM a, b, |` still resolves to `FROM`), and inspects
/// the keyword token immediately preceding that span.
pub fn is_table_name_context(source: &str, position: Position) -> bool {
    let offset = position_to_offset(source, position);
    let Some(before) = source.get(..offset) else {
        return false;
    };
    let chars: Vec<char> = before.chars().collect();
    let mut i = chars.len();

    while i > 0 && is_table_ident_char(chars[i - 1]) {
        i -= 1;
    }
    loop {
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        if i == 0 || chars[i - 1] != ',' {
            break;
        }
        i -= 1;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && is_table_ident_char(chars[i - 1]) {
            i -= 1;
        }
    }
    let keyword_end = i;
    while i > 0 && is_table_ident_char(chars[i - 1]) {
        i -= 1;
    }
    if i == keyword_end {
        return false;
    }
    let keyword: String = chars[i..keyword_end].iter().collect();
    matches!(
        keyword.to_ascii_uppercase().as_str(),
        "FROM" | "INTO" | "UPDATE"
    )
}

fn is_table_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '`'
}

/// Classification of a column-name slot near the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnSlot {
    /// The cursor is in a position that *only* accepts column names —
    /// `SELECT ... |, FROM tbl`, `UPDATE tbl SET |`, or after a `tbl.`
    /// qualifier. Suggestions should be column-only. The contained flag
    /// is true when emitting a leading `*` is appropriate (SELECT only).
    Strict { allow_star: bool },
}

/// Returns the column-completion classification for the cursor. Returns
/// `None` when the cursor is not in any column-name slot we recognise.
///
/// Strategy: walk backwards from the cursor over the partial identifier,
/// then over any `<ident> <ws>* (= <expr>)? <ws>* ,` runs (so multi-column
/// SELECT/SET lists still detect the leading `SELECT`/`SET`), and then
/// inspect the previous keyword token.
///
/// The algorithm intentionally avoids a full SurrealQL parse — it covers
/// the common, syntactically-unambiguous cases listed below and degrades
/// to `None` for anything unfamiliar (sub-queries, parenthesised
/// expressions, ON clauses, etc.).
pub fn column_completion_context(source: &str, position: Position) -> Option<ColumnSlot> {
    let offset = position_to_offset(source, position);
    let before = source.get(..offset)?;
    let chars: Vec<char> = before.chars().collect();
    let mut i = chars.len();

    while i > 0 && is_table_ident_char(chars[i - 1]) {
        i -= 1;
    }
    loop {
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        if i == 0 || chars[i - 1] != ',' {
            break;
        }
        i -= 1;
        while i > 0 {
            let c = chars[i - 1];
            if c == ',' {
                break;
            }
            if matches!(c, '\'' | '"' | '(' | ')' | '{' | '}' | '[' | ']' | ';') {
                return None;
            }
            i -= 1;
        }
    }
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    let keyword_end = i;
    while i > 0 && is_table_ident_char(chars[i - 1]) {
        i -= 1;
    }
    if i == keyword_end {
        return None;
    }
    let keyword: String = chars[i..keyword_end]
        .iter()
        .collect::<String>()
        .to_ascii_uppercase();
    match keyword.as_str() {
        "SELECT" => Some(ColumnSlot::Strict { allow_star: true }),
        "SET" => Some(ColumnSlot::Strict { allow_star: false }),
        // `WHERE`, `AND`, `OR` and `BY` used to return a `Loose` variant that
        // the handler never matched on, so they already behaved as `None`:
        // the full list, with fields sorted to the top by
        // `crate::semantic::model`. Keep that. Narrowing an expression
        // position hides the fields, variables and functions that are all
        // legal there.
        _ => None,
    }
}

pub fn completion_prefix(source: &str, position: Position, record_type_context: bool) -> String {
    let prefix = crate::semantic::text::token_prefix(source, position).unwrap_or_default();
    if record_type_context {
        prefix
            .rsplit_once('<')
            .map(|(_, suffix)| suffix.to_string())
            .unwrap_or(prefix)
    } else {
        prefix
    }
}

pub fn active_query_fact<'a>(
    analysis: &'a DocumentAnalysis,
    position: Position,
) -> Option<&'a QueryFact> {
    analysis
        .query_facts
        .iter()
        .find(|fact| range_contains_position(fact.location.range, position))
}

fn range_contains_position(range: ls_types::Range, position: Position) -> bool {
    position_gte(position, range.start) && position_lte(position, range.end)
}

fn position_lte(left: Position, right: Position) -> bool {
    left.line < right.line || (left.line == right.line && left.character <= right.character)
}

fn position_gte(left: Position, right: Position) -> bool {
    left.line > right.line || (left.line == right.line && left.character >= right.character)
}

pub fn completion_table_qualifier(source: &str, position: Position) -> Option<String> {
    let offset = position_to_offset(source, position);
    let before_cursor = source.get(..offset)?;
    let (left, right) = before_cursor.rsplit_once('.')?;
    if !right.chars().all(is_field_prefix_char) {
        return None;
    }

    let raw: String = left
        .chars()
        .rev()
        .take_while(|ch| is_table_qualifier_char(*ch))
        .collect();
    // A `$variable` is not a table. `$` is not a qualifier character, so the scan
    // stops just after it and `$s.` used to yield the table name `s` — whereupon
    // `column_completion_items` found no fields on a table called `s` and the
    // handler answered with an *empty* popup rather than letting the global list
    // through. That was the sharpest completion defect in the server.
    if left.chars().rev().nth(raw.chars().count()) == Some('$') {
        return None;
    }
    let qualifier: String = raw.chars().rev().collect();
    let qualifier = qualifier.trim_matches('`');
    if qualifier.is_empty() {
        return None;
    }
    if qualifier
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
    {
        return None;
    }

    let table = qualifier.split(':').next().unwrap_or(qualifier).trim();
    if table.is_empty() {
        None
    } else {
        Some(table.to_string())
    }
}

fn is_table_qualifier_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | ':' | '-' | '`')
}

fn is_field_prefix_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | ':' | '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words at a cursor placed at the very end of `source`.
    fn words_at_end(source: &str) -> Option<Vec<String>> {
        let line = source.lines().count().saturating_sub(1) as u32;
        let character = source.lines().last().map_or(0, str::len) as u32;
        statement_words(source, Position { line, character })
    }

    fn words(source: &str) -> Vec<String> {
        words_at_end(source).expect("expected a classifiable position")
    }

    #[test]
    fn a_trailing_space_commits_the_last_word() {
        assert_eq!(words("INFO FOR "), vec!["INFO", "FOR"]);
    }

    #[test]
    fn a_half_typed_word_is_left_for_the_prefix() {
        assert_eq!(words("INFO FOR RO"), vec!["INFO", "FOR"]);
        assert_eq!(words("INFO F"), vec!["INFO"]);
    }

    #[test]
    fn a_top_level_semicolon_starts_a_new_statement() {
        assert_eq!(
            words("SELECT * FROM person; INFO FOR "),
            vec!["INFO", "FOR"]
        );
        assert_eq!(words("USE NS a;\nINFO FOR "), vec!["INFO", "FOR"]);
    }

    #[test]
    fn a_closed_bracket_group_collapses_to_one_word() {
        // Six raw tokens, four words: the parameter list is one slot.
        assert_eq!(
            words("DEFINE FUNCTION fn::x($a: int) ").len(),
            4,
            "got {:?}",
            words("DEFINE FUNCTION fn::x($a: int) ")
        );
    }

    #[test]
    fn an_unclosed_bracket_is_not_classifiable() {
        // `(` is a completion trigger character, so this state occurs on every
        // keystroke inside a call.
        assert_eq!(words_at_end("RETURN string::len("), None);
        assert_eq!(words_at_end("CREATE person CONTENT { name: "), None);
        assert_eq!(words_at_end("SELECT * FROM (SELECT * FROM "), None);
    }

    #[test]
    fn a_cursor_inside_a_string_is_not_classifiable() {
        assert_eq!(words_at_end("INFO FOR TABLE 'unterm"), None);
    }

    #[test]
    fn a_closed_string_is_one_word() {
        assert_eq!(words("KILL 'abc' ").len(), 2);
    }

    #[test]
    fn a_semicolon_inside_a_string_does_not_split_the_statement() {
        assert_eq!(words("INFO FOR TABLE 'a;b' ").len(), 4);
    }

    #[test]
    fn a_cursor_inside_a_comment_is_not_classifiable() {
        assert_eq!(words_at_end("INFO FOR -- note "), None);
        assert_eq!(words_at_end("INFO FOR # note "), None);
        assert_eq!(words_at_end("INFO FOR /* note "), None);
    }

    #[test]
    fn a_finished_comment_is_skipped() {
        assert_eq!(words("INFO /* note */ FOR "), vec!["INFO", "FOR"]);
        assert_eq!(words("INFO -- note\nFOR "), vec!["INFO", "FOR"]);
    }

    #[test]
    fn the_head_slot_falls_back_to_expression_when_unclassifiable() {
        let source = "RETURN string::len(";
        let position = Position {
            line: 0,
            character: source.len() as u32,
        };
        assert_eq!(head_slot_at(source, position), SlotYield::Expression);
    }

    #[test]
    fn the_head_slot_answers_for_a_modelled_head() {
        let source = "INFO FOR ";
        let position = Position {
            line: 0,
            character: source.len() as u32,
        };
        assert!(matches!(
            head_slot_at(source, position),
            SlotYield::Keywords(_)
        ));
    }
}
