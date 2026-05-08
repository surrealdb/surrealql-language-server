//! Cursor-context analysis used by the completion handler.
//!
//! These helpers walk the source text backwards from the cursor to
//! decide which kind of completion list (table names only, column
//! names only, or generic) makes syntactic sense at that position.
//! Extracted from the original `backend.rs` so both the native and
//! WASM dispatchers can call them through the core.

use ls_types::Position;

use crate::semantic::text::position_to_offset;
use crate::semantic::types::{DocumentAnalysis, QueryFact};

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
    /// The cursor is in an expression position where columns are useful
    /// but other things (functions, $vars, literals) are also legal —
    /// `WHERE | / AND | / OR |`, `ORDER BY |`, `GROUP BY |`. Columns
    /// should be surfaced at the top of the dropdown but the rest of the
    /// generic completions should still appear.
    Loose,
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
        "WHERE" | "AND" | "OR" | "BY" => Some(ColumnSlot::Loose),
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

    let qualifier = left
        .chars()
        .rev()
        .take_while(|ch| is_table_qualifier_char(*ch))
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
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
