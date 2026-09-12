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
use crate::semantic::text::{LineIndex, preceding_char};
use crate::semantic::types::{DocumentAnalysis, LookupDirection, QueryFact};

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
pub fn statement_words(source: &str, lines: &LineIndex, position: Position) -> Option<Vec<String>> {
    let offset = lines.offset(source, position);
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
pub fn head_slot_at(source: &str, lines: &LineIndex, position: Position) -> SlotYield {
    match statement_words(source, lines, position) {
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
pub fn is_table_name_context(source: &str, lines: &LineIndex, position: Position) -> bool {
    let offset = lines.offset(source, position);
    let Some(before) = source.get(..offset) else {
        return false;
    };
    // Walks backward in byte offsets. The previous version collected the whole
    // prefix into a `Vec<char>` to index it, which allocated about 4 bytes per
    // character of everything before the cursor on every completion request.
    let mut i = before.len();
    skip_back_while(before, &mut i, is_table_ident_char);
    loop {
        skip_back_while(before, &mut i, char::is_whitespace);
        match preceding_char(before, i) {
            Some((start, ',')) => i = start,
            _ => break,
        }
        skip_back_while(before, &mut i, char::is_whitespace);
        skip_back_while(before, &mut i, is_table_ident_char);
    }
    let keyword_end = i;
    skip_back_while(before, &mut i, is_table_ident_char);
    if i == keyword_end {
        return false;
    }
    matches!(
        before[i..keyword_end].to_ascii_uppercase().as_str(),
        "FROM" | "INTO" | "UPDATE"
    )
}

/// Move `i` left over every character that satisfies `predicate`.
fn skip_back_while(source: &str, i: &mut usize, predicate: impl Fn(char) -> bool) {
    while let Some((start, ch)) = preceding_char(source, *i) {
        if !predicate(ch) {
            return;
        }
        *i = start;
    }
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
pub fn column_completion_context(
    source: &str,
    lines: &LineIndex,
    position: Position,
) -> Option<ColumnSlot> {
    let offset = lines.offset(source, position);
    let before = source.get(..offset)?;
    let mut i = before.len();

    skip_back_while(before, &mut i, is_table_ident_char);
    loop {
        skip_back_while(before, &mut i, char::is_whitespace);
        match preceding_char(before, i) {
            Some((start, ',')) => i = start,
            _ => break,
        }
        loop {
            // Stop at the keyword that opened the list. Without this the scan
            // reads straight through it — on `SELECT id,|` it consumed
            // `SELECT id` whole, landed at offset 0, and the keyword lookup
            // below then found nothing and answered `None`. Every position
            // after a comma therefore fell through to the full catalogue, so
            // only the *first* column of a `SELECT` or `SET` list ever
            // completed.
            if ends_with_list_keyword(&before[..i]) {
                break;
            }
            match preceding_char(before, i) {
                None => break,
                Some((_, ',')) => break,
                Some((_, '\'' | '"' | '(' | ')' | '{' | '}' | '[' | ']' | ';')) => return None,
                Some((start, _)) => i = start,
            }
        }
    }
    skip_back_while(before, &mut i, char::is_whitespace);
    let keyword_end = i;
    skip_back_while(before, &mut i, is_table_ident_char);
    if i == keyword_end {
        return None;
    }
    let keyword = before[i..keyword_end].to_ascii_uppercase();
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

/// True when `text` ends with a keyword that opens a comma-separated column
/// list.
///
/// Word-bounded, so a table named `preset` does not read as a `SET`.
fn ends_with_list_keyword(text: &str) -> bool {
    let trimmed = text.trim_end();
    ["SELECT", "SET"].iter().any(|keyword| {
        let Some(tail) = trimmed
            .len()
            .checked_sub(keyword.len())
            .and_then(|at| trimmed.get(at..))
        else {
            return false;
        };
        tail.eq_ignore_ascii_case(keyword)
            && trimmed[..trimmed.len() - keyword.len()]
                .chars()
                .next_back()
                .is_none_or(|ch| !is_table_ident_char(ch))
    })
}

/// A cursor sitting just after a graph arrow, where the author is naming the
/// next hop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSlot {
    /// Which way the arrow the cursor follows points.
    pub direction: LookupDirection,
    /// The name written immediately before that arrow.
    ///
    /// `None` for a traversal with no written base — `SELECT ->|` — whose
    /// anchor is the statement's own `FROM` table instead.
    pub anchor: Option<String>,
    /// True when [`Self::anchor`] names an *edge* table, so the legal
    /// continuations are the tables that edge reaches rather than the edges
    /// leaving a table.
    ///
    /// Decided by counting arrows, because a traversal alternates: the first
    /// hop leaves a table, the second leaves the edge the first named, and so
    /// on. `person->knows->|` is at the second hop, so it wants tables.
    pub from_edge: bool,
}

/// Classify the cursor as a graph hop, or `None` when it is not one.
///
/// Scans backward over the partial name, then over the chain of
/// `arrow name arrow name …` behind it. Text rather than tree, like every
/// other classifier here, and for the same reason: the document is mid-edit,
/// and `SELECT -> FROM person` does not parse.
pub fn graph_edge_context(
    source: &str,
    lines: &LineIndex,
    position: Position,
) -> Option<GraphSlot> {
    let offset = lines.offset(source, position);
    let before = source.get(..offset)?;

    let mut index = before.len();
    skip_back_over_name(before, &mut index);
    let direction = take_arrow_back(before, &mut index)?;

    // Walk the whole chain behind the cursor. The *first* name found is the
    // anchor — the one this hop leaves. The rest are walked only to count the
    // arrows, because the count is what says whether the anchor is a table or
    // an edge.
    let mut arrows = 1;
    let mut at = index;
    let mut anchor: Option<&str> = None;
    loop {
        let name_end = at;
        let mut name_start = at;
        skip_back_over_name(before, &mut name_start);
        if name_start == name_end {
            // Nothing between this arrow and whatever precedes it, so the
            // traversal writes no base: `SELECT ->|`.
            break;
        }
        anchor.get_or_insert(&before[name_start..name_end]);

        let mut behind = name_start;
        if take_arrow_back(before, &mut behind).is_none() {
            break;
        }
        arrows += 1;
        at = behind;
    }

    Some(GraphSlot {
        direction,
        // A record id anchors on its table: `person:alice->` leaves `person`.
        anchor: anchor
            .and_then(|name| name.split(':').next())
            .map(|name| name.trim_matches('`').to_string())
            .filter(|name| !name.is_empty()),
        from_edge: arrows % 2 == 0,
    })
}

/// The tables a graph hop starts from.
///
/// A written base answers for itself. A traversal with none —
/// `SELECT ->| FROM person` — starts from the statement's own target, which is
/// why the ranking only applies once a table follows `FROM`.
pub fn graph_anchors(
    slot: &GraphSlot,
    analysis: &DocumentAnalysis,
    position: Position,
) -> Vec<String> {
    if let Some(anchor) = &slot.anchor {
        return vec![anchor.clone()];
    }
    if let Some(fact) = active_query_fact(analysis, position)
        && !fact.target_tables.is_empty()
    {
        return fact.target_tables.clone();
    }
    // The parse is of the document *mid-edit*: `SELECT -> FROM person` is not
    // valid SurrealQL, so it yields an ERROR node and no usable query fact.
    // The table is right there in the text, though, so read it from there.
    statement_target_in_text(&analysis.text, &analysis.line_index, position)
        .into_iter()
        .collect()
}

/// The keywords a statement's target table follows.
///
/// `DELETE` is here for `DELETE person`; the guard below steps over the `FROM`
/// of `DELETE FROM person` so that form reads its table too.
const TARGET_KEYWORDS: [&str; 6] = ["FROM", "INTO", "UPDATE", "UPSERT", "CREATE", "DELETE"];

/// The table a statement targets, read from the raw text.
///
/// The fallback for a statement the parser cannot make sense of yet — which is
/// the *normal* state while typing. `SELECT  FROM person` has an empty
/// projection and yields an `ERROR` node, so there is no query fact to read the
/// target from, at exactly the moment the author wants the column list. Same
/// for `SELECT -> FROM person`, and for any list left with a trailing comma.
///
/// Deliberately small: one statement, the first target keyword, the name after
/// it.
pub fn statement_target_in_text(
    source: &str,
    lines: &LineIndex,
    position: Position,
) -> Option<String> {
    let cursor = lines.offset(source, position);
    let start = source[..cursor].rfind(';').map_or(0, |at| at + 1);
    let end = source[cursor..]
        .find(';')
        .map_or(source.len(), |at| cursor + at);
    let words: Vec<&str> = source.get(start..end)?.split_whitespace().collect();

    let is_keyword = |word: &str| {
        TARGET_KEYWORDS
            .iter()
            .any(|keyword| word.eq_ignore_ascii_case(keyword))
    };

    for (index, word) in words.iter().enumerate() {
        if !is_keyword(word) {
            continue;
        }
        let Some(next) = words.get(index + 1) else {
            continue;
        };
        // `DELETE FROM person` — the table follows the *second* keyword.
        if is_keyword(next) {
            continue;
        }
        let name: String = next
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
            .collect();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// Move `index` left over one graph arrow, reporting which way it points.
///
/// Longest first: `<->` ends in `->`, so a shortest-first read would take the
/// `->` and leave a stray `<` for the name scan.
fn take_arrow_back(source: &str, index: &mut usize) -> Option<LookupDirection> {
    for (arrow, direction) in [
        ("<->", LookupDirection::Both),
        ("->", LookupDirection::Right),
        ("<-", LookupDirection::Left),
        ("<~", LookupDirection::Left),
    ] {
        if source[..*index].ends_with(arrow) {
            *index -= arrow.len();
            return Some(direction);
        }
    }
    None
}

/// Move `index` left over one hop's name.
///
/// A hyphen is part of a table name — `my-table` is one name — so it is walked
/// over. But a hyphen that belongs to an arrow is not: swallowing it would
/// merge two hops into one, or hide the arrow entirely. Both halves have to be
/// excluded, the `-` that opens a `->` and the `-` that closes a `<-`.
///
/// A colon is included so a record-id base scans whole; the caller keeps the
/// table half.
fn skip_back_over_name(source: &str, index: &mut usize) {
    while let Some((start, ch)) = preceding_char(source, *index) {
        let is_name = ch.is_ascii_alphanumeric() || matches!(ch, '_' | '`' | ':');
        let is_arrow_half = source[start..].starts_with("->") || source[..start].ends_with('<');
        let is_inner_hyphen = ch == '-' && !is_arrow_half;
        if !(is_name || is_inner_hyphen) {
            return;
        }
        *index = start;
    }
}

pub fn completion_prefix(
    source: &str,
    lines: &LineIndex,
    position: Position,
    record_type_context: bool,
) -> String {
    let prefix = crate::semantic::text::token_prefix(source, lines, position).unwrap_or_default();
    if record_type_context {
        prefix
            .rsplit_once('<')
            .map(|(_, suffix)| suffix.to_string())
            .unwrap_or(prefix)
    } else {
        prefix
    }
}

pub fn active_query_fact(analysis: &DocumentAnalysis, position: Position) -> Option<&QueryFact> {
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

pub fn completion_table_qualifier(
    source: &str,
    lines: &LineIndex,
    position: Position,
) -> Option<String> {
    let offset = lines.offset(source, position);
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
        statement_words(
            source,
            &LineIndex::new(source),
            Position { line, character },
        )
    }

    fn words(source: &str) -> Vec<String> {
        words_at_end(source).expect("expected a classifiable position")
    }

    /// The classifier at a cursor placed at the very end of `source`.
    fn slot(source: &str) -> Option<GraphSlot> {
        let line = source.lines().count().saturating_sub(1) as u32;
        let character = source.lines().last().map_or(0, str::len) as u32;
        graph_edge_context(
            source,
            &LineIndex::new(source),
            Position { line, character },
        )
    }

    #[test]
    fn a_cursor_after_an_arrow_is_a_graph_slot() {
        let found = slot("SELECT * FROM person->").expect("a graph slot");
        assert_eq!(found.direction, LookupDirection::Right);
        assert_eq!(found.anchor.as_deref(), Some("person"));
        assert!(!found.from_edge, "the first hop leaves a table");
    }

    #[test]
    fn a_half_typed_hop_name_is_still_a_graph_slot() {
        let found = slot("SELECT * FROM person->kno").expect("a graph slot");
        assert_eq!(found.anchor.as_deref(), Some("person"));
    }

    #[test]
    fn every_arrow_spelling_is_classified() {
        for (arrow, expected) in [
            ("->", LookupDirection::Right),
            ("<-", LookupDirection::Left),
            ("<~", LookupDirection::Left),
            ("<->", LookupDirection::Both),
        ] {
            let source = format!("SELECT * FROM person{arrow}");
            let found = slot(&source).unwrap_or_else(|| panic!("a graph slot for {source}"));
            assert_eq!(found.direction, expected, "for {arrow}");
            assert_eq!(found.anchor.as_deref(), Some("person"), "for {arrow}");
        }
    }

    /// The second hop leaves the edge the first named, so it wants the tables
    /// that edge reaches — not another edge.
    #[test]
    fn the_second_hop_reads_from_the_edge() {
        let found = slot("SELECT * FROM person->knows->").expect("a graph slot");
        assert_eq!(found.anchor.as_deref(), Some("knows"));
        assert!(found.from_edge);
    }

    /// And the third is back to leaving a table.
    #[test]
    fn the_third_hop_reads_from_a_table_again() {
        let found = slot("SELECT * FROM person->knows->person->").expect("a graph slot");
        assert_eq!(found.anchor.as_deref(), Some("person"));
        assert!(!found.from_edge);
    }

    /// A traversal with no written base takes its anchor from the statement,
    /// which the handler resolves — the classifier only reports that there is
    /// none here.
    #[test]
    fn a_bodiless_traversal_reports_no_written_anchor() {
        let found = slot("SELECT ->").expect("a graph slot");
        assert_eq!(found.anchor, None);
        assert!(!found.from_edge, "the first hop still leaves a table");

        let second = slot("SELECT ->knows->").expect("a graph slot");
        assert_eq!(second.anchor.as_deref(), Some("knows"));
        assert!(second.from_edge);
    }

    #[test]
    fn a_record_id_base_anchors_on_its_table() {
        let found = slot("SELECT * FROM person:alice->").expect("a graph slot");
        assert_eq!(found.anchor.as_deref(), Some("person"));
    }

    /// A hyphen belongs to the name, but the `-` of a `->` does not.
    #[test]
    fn a_hyphenated_anchor_keeps_its_hyphen() {
        let found = slot("SELECT * FROM my-table->").expect("a graph slot");
        assert_eq!(found.anchor.as_deref(), Some("my-table"));
    }

    #[test]
    fn a_cursor_that_follows_no_arrow_is_not_a_graph_slot() {
        for source in [
            "SELECT * FROM person",
            "SELECT * FROM ",
            "SELECT name FROM person WHERE age > ",
            "LET $x: record<person",
            "SELECT 1 - ",
        ] {
            assert_eq!(slot(source), None, "{source} is not a graph slot");
        }
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
        assert_eq!(
            head_slot_at(source, &LineIndex::new(source), position),
            SlotYield::Expression
        );
    }

    #[test]
    fn the_head_slot_answers_for_a_modelled_head() {
        let source = "INFO FOR ";
        let position = Position {
            line: 0,
            character: source.len() as u32,
        };
        assert!(matches!(
            head_slot_at(source, &LineIndex::new(source), position),
            SlotYield::Keywords(_)
        ));
    }
}
