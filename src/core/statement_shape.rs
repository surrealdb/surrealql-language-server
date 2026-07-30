//! Closed-vocabulary statement-head slots.
//!
//! A SurrealQL statement opens with a run of keywords whose legal
//! continuations are a small, closed set. `INFO FOR ` accepts exactly nine
//! words; `USE ` accepts four. Today the completion handler offers the whole
//! catalogue in every one of those positions (`crate::core::server`, the
//! unguarded call to `MergedSemanticModel::completion_items`), so `INFO FOR `
//! returns ~375 items of which nine are legal.
//!
//! This module is the vocabulary those positions need. It is a flat table of
//! literal word prefixes, looked up by exact length with a wildcard for a
//! name the user chose. Deliberately *not* a parser and deliberately not a
//! model of clause order:
//!
//! * **No clause spine.** `SELECT`'s clause order is strict, but classifying a
//!   position inside it requires knowing whether the last word was a clause
//!   keyword or a field that happens to be spelled `order` — SurrealQL accepts
//!   `SELECT order FROM t` (`parse_ident` admits keyword-like tokens). Guessing
//!   there hides legal completions in `WHERE … AND `, which is the busiest
//!   position in the language. Those positions keep today's behaviour.
//! * **Only closed sets.** A rule exists only where the legal continuation is
//!   a finite keyword set. Where an expression is legal — `DEFAULT `, a name
//!   the user invents, `INFO FOR USER ` (the engine parses that name as a full
//!   expression) — the answer is [`SlotYield::Expression`], which means "keep
//!   today's list". An empty list would be a wrong answer, not silence.
//!
//! That makes the table subtractive-only: it can never fire unless a literal
//! keyword prefix matches, so no position that works today can regress.
//!
//! Every vocabulary below is transcribed from the SurrealDB parser, not from
//! the documentation site. The citations are paths under
//! `surrealdb/core/src/syn/parser/`.

/// Matches exactly one word of any spelling — a table, index, or user name the
/// author chose. Not a valid SurrealQL keyword, so it cannot collide with one.
pub const ANY: &str = "*";

/// What the completion list may offer at one statement-head slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotYield {
    /// Exactly these keywords. Nothing else is legal here.
    Keywords(&'static [&'static str]),
    /// These keywords, plus the table names the model knows. Used where the
    /// slot accepts either (`DEFINE EVENT e ON ` takes `TABLE` or a table).
    KeywordsAndTables(&'static [&'static str]),
    /// Table names only.
    Tables,
    /// The names of the `DEFINE ANALYZER`s the model knows. Used only where the
    /// name must already exist.
    Analyzers,
    /// The legal set is open, or we have not modelled it. Callers must keep
    /// the behaviour they have today and offer their full list.
    Expression,
}

/// `ROOT | NAMESPACE | NS | DATABASE | DB` — the `<base>` of an `ON` clause
/// (`stmt/parts.rs:444-454`; `NS`/`DB` lex to the same tokens as the long
/// spellings, `lexer/keywords.rs:120,208`).
const BASE: &[&str] = &["ROOT", "NAMESPACE", "NS", "DATABASE", "DB"];

/// The six `INFO FOR` targets, with both spellings of the three that have one
/// (`stmt/mod.rs:417-476`). `SC` and `SCOPE` are *not* here: SurrealDB 3.x
/// dropped both, although the pinned tree-sitter grammar still offers them.
const INFO_TARGETS: &[&str] = &[
    "ROOT",
    "NAMESPACE",
    "NS",
    "DATABASE",
    "DB",
    "TABLE",
    "TB",
    "USER",
    "INDEX",
];

/// The 16 `DEFINE` sub-forms (`stmt/define.rs:51-78`). `TOKEN`, `SCOPE` and
/// `MODEL` are gone in 3.x — the words still lex, but no parser arm accepts
/// them.
const DEFINE_FORMS: &[&str] = &[
    "NAMESPACE",
    "DATABASE",
    "FUNCTION",
    "USER",
    "PARAM",
    "TABLE",
    "API",
    "EVENT",
    "FIELD",
    "INDEX",
    "ANALYZER",
    "ACCESS",
    "CONFIG",
    "BUCKET",
    "SEQUENCE",
    "MODULE",
];

/// The 16 `REMOVE` sub-forms (`stmt/remove.rs:25-320`).
const REMOVE_FORMS: &[&str] = &[
    "NAMESPACE",
    "DATABASE",
    "TABLE",
    "FUNCTION",
    "MODULE",
    "ACCESS",
    "USER",
    "PARAM",
    "EVENT",
    "FIELD",
    "INDEX",
    "ANALYZER",
    "SEQUENCE",
    "API",
    "BUCKET",
    "CONFIG",
];

/// The 17 `ALTER` sub-forms (`stmt/alter.rs:26-46`).
const ALTER_FORMS: &[&str] = &[
    "SYSTEM",
    "NAMESPACE",
    "DATABASE",
    "TABLE",
    "EVENT",
    "INDEX",
    "FIELD",
    "PARAM",
    "SEQUENCE",
    "BUCKET",
    "ANALYZER",
    "FUNCTION",
    "USER",
    "ACCESS",
    "CONFIG",
    "API",
    "MODULE",
];

/// `DEFINE TABLE <name> ` (`stmt/define.rs:663-731`). The view clause opens
/// with `AS`, not `VIEW`; `DEFINE TABLE t VIEW …` is a parse error.
const DEFINE_TABLE_CLAUSES: &[&str] = &[
    "COMMENT",
    "DROP",
    "TYPE",
    "SCHEMALESS",
    "SCHEMAFULL",
    "PERMISSIONS",
    "CHANGEFEED",
    "AS",
    "GRAPHQL_ALIAS",
    "GRAPHQL_DEPRECATED",
];

/// `DEFINE FIELD <name> ON <table> ` (`stmt/define.rs:954-1033`).
const DEFINE_FIELD_CLAUSES: &[&str] = &[
    "TYPE",
    "FLEXIBLE",
    "READONLY",
    "VALUE",
    "ASSERT",
    "DEFAULT",
    "PERMISSIONS",
    "COMMENT",
    "REFERENCE",
    "COMPUTED",
    "GRAPHQL_ALIAS",
    "GRAPHQL_DEPRECATED",
];

/// `DEFINE EVENT <name> ON <table> ` (`stmt/define.rs:846-920`). `RETRY` and
/// `MAXDEPTH` are only legal after `ASYNC`; offering them one slot early is
/// harmless, because the alternative is offering the whole catalogue.
const DEFINE_EVENT_CLAUSES: &[&str] = &["WHEN", "THEN", "COMMENT", "ASYNC", "RETRY", "MAXDEPTH"];

/// `DEFINE INDEX <name> ON <table> ` (`stmt/define.rs:1051-1338`). The index
/// kinds are `Idx Uniq Hnsw DiskAnn FullText Count` (`sql/index.rs:11-24`) —
/// `MTREE` and `SEARCH` are gone in 3.x.
const DEFINE_INDEX_CLAUSES: &[&str] = &[
    "FIELDS",
    "COLUMNS",
    "UNIQUE",
    "COUNT",
    "FULLTEXT",
    "HNSW",
    "DISKANN",
    "CONCURRENTLY",
    "COMMENT",
];

/// `DEFINE TABLE t TYPE ` (`stmt/define.rs:671-687`).
const TABLE_TYPES: &[&str] = &["NORMAL", "RELATION", "ANY"];

/// `PERMISSIONS ` (`stmt/parts.rs:324-353`).
const PERMISSIONS_HEADS: &[&str] = &["NONE", "FULL", "FOR"];

/// Keywords this table offers that the pinned tree-sitter grammar cannot lex.
///
/// The engine accepts every one of them, so hiding them would deny the author
/// a legal clause. Offering them means the grammar reports a syntax error on a
/// clause the server itself suggested. That is a pre-existing grammar gap, not
/// one this table introduces — recorded here so a grammar bump can shorten the
/// list, and asserted by `offers_outside_the_grammar_are_declared`.
pub const OFFERS_THE_GRAMMAR_CANNOT_PARSE: &[&str] = &[
    "GRAPHQL_ALIAS",
    "GRAPHQL_DEPRECATED",
    "SYSTEM",
    "DISKANN",
    "RETRY",
    "MAXDEPTH",
];

struct HeadRule {
    /// The words that must already be typed, in order. [`ANY`] matches one
    /// word of any spelling.
    prefix: &'static [&'static str],
    /// What the slot directly after `prefix` offers.
    yields: SlotYield,
}

/// Every modelled statement-head slot.
///
/// Order is irrelevant: [`head_slot`] matches on length and breaks ties by
/// [`specificity`], so the table stays readable rather than sorted.
const HEAD_RULES: &[HeadRule] = &[
    // ── INFO FOR ── stmt/mod.rs:417-476
    HeadRule {
        prefix: &["INFO"],
        yields: SlotYield::Keywords(&["FOR"]),
    },
    HeadRule {
        prefix: &["INFO", "FOR"],
        yields: SlotYield::Keywords(INFO_TARGETS),
    },
    HeadRule {
        prefix: &["INFO", "FOR", "TABLE"],
        yields: SlotYield::Tables,
    },
    HeadRule {
        prefix: &["INFO", "FOR", "TB"],
        yields: SlotYield::Tables,
    },
    // `VERSION` then `STRUCTURE`, in that order. The three database-level
    // targets take both; `USER` and `INDEX` take only `STRUCTURE`.
    HeadRule {
        prefix: &["INFO", "FOR", "ROOT"],
        yields: SlotYield::Keywords(&["VERSION", "STRUCTURE"]),
    },
    HeadRule {
        prefix: &["INFO", "FOR", "NAMESPACE"],
        yields: SlotYield::Keywords(&["VERSION", "STRUCTURE"]),
    },
    HeadRule {
        prefix: &["INFO", "FOR", "NS"],
        yields: SlotYield::Keywords(&["VERSION", "STRUCTURE"]),
    },
    HeadRule {
        prefix: &["INFO", "FOR", "DATABASE"],
        yields: SlotYield::Keywords(&["VERSION", "STRUCTURE"]),
    },
    HeadRule {
        prefix: &["INFO", "FOR", "DB"],
        yields: SlotYield::Keywords(&["VERSION", "STRUCTURE"]),
    },
    HeadRule {
        prefix: &["INFO", "FOR", "TABLE", ANY],
        yields: SlotYield::Keywords(&["VERSION", "STRUCTURE"]),
    },
    HeadRule {
        prefix: &["INFO", "FOR", "TB", ANY],
        yields: SlotYield::Keywords(&["VERSION", "STRUCTURE"]),
    },
    HeadRule {
        prefix: &["INFO", "FOR", "INDEX", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["INFO", "FOR", "INDEX", ANY, "ON"],
        yields: SlotYield::KeywordsAndTables(&["TABLE"]),
    },
    HeadRule {
        prefix: &["INFO", "FOR", "USER", ANY],
        yields: SlotYield::Keywords(&["ON", "STRUCTURE"]),
    },
    HeadRule {
        prefix: &["INFO", "FOR", "USER", ANY, "ON"],
        yields: SlotYield::Keywords(BASE),
    },
    // ── USE ── stmt/mod.rs:367-393
    HeadRule {
        prefix: &["USE"],
        yields: SlotYield::Keywords(&["NAMESPACE", "NS", "DATABASE", "DB", "DEFAULT"]),
    },
    HeadRule {
        prefix: &["USE", "NAMESPACE", ANY],
        yields: SlotYield::Keywords(&["DATABASE", "DB"]),
    },
    HeadRule {
        prefix: &["USE", "NS", ANY],
        yields: SlotYield::Keywords(&["DATABASE", "DB"]),
    },
    // ── SHOW CHANGES ── stmt/mod.rs:617-651
    HeadRule {
        prefix: &["SHOW"],
        yields: SlotYield::Keywords(&["CHANGES"]),
    },
    HeadRule {
        prefix: &["SHOW", "CHANGES"],
        yields: SlotYield::Keywords(&["FOR"]),
    },
    HeadRule {
        prefix: &["SHOW", "CHANGES", "FOR"],
        yields: SlotYield::Keywords(&["TABLE", "DATABASE"]),
    },
    HeadRule {
        prefix: &["SHOW", "CHANGES", "FOR", "TABLE"],
        yields: SlotYield::Tables,
    },
    HeadRule {
        prefix: &["SHOW", "CHANGES", "FOR", "TABLE", ANY],
        yields: SlotYield::Keywords(&["SINCE"]),
    },
    HeadRule {
        prefix: &["SHOW", "CHANGES", "FOR", "DATABASE"],
        yields: SlotYield::Keywords(&["SINCE"]),
    },
    // ── REBUILD ── stmt/mod.rs:545-570
    HeadRule {
        prefix: &["REBUILD"],
        yields: SlotYield::Keywords(&["INDEX"]),
    },
    HeadRule {
        prefix: &["REBUILD", "INDEX"],
        yields: SlotYield::Keywords(&["IF"]),
    },
    HeadRule {
        prefix: &["REBUILD", "INDEX", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["REBUILD", "INDEX", ANY, "ON"],
        yields: SlotYield::KeywordsAndTables(&["TABLE"]),
    },
    // ── DEFINE ── stmt/define.rs:51-78
    HeadRule {
        prefix: &["DEFINE"],
        yields: SlotYield::Keywords(DEFINE_FORMS),
    },
    // `[IF NOT EXISTS] | [OVERWRITE]` follows the sub-form keyword, before the
    // name (define.rs:81-108). `DEFINE TABLE ` also usefully offers existing
    // tables, because `OVERWRITE` targets one.
    HeadRule {
        prefix: &["DEFINE", "TABLE"],
        yields: SlotYield::KeywordsAndTables(&["IF", "OVERWRITE"]),
    },
    HeadRule {
        prefix: &["DEFINE", "TABLE", ANY],
        yields: SlotYield::Keywords(DEFINE_TABLE_CLAUSES),
    },
    HeadRule {
        prefix: &["DEFINE", "TABLE", ANY, "TYPE"],
        yields: SlotYield::Keywords(TABLE_TYPES),
    },
    HeadRule {
        prefix: &["DEFINE", "TABLE", ANY, "PERMISSIONS"],
        yields: SlotYield::Keywords(PERMISSIONS_HEADS),
    },
    HeadRule {
        prefix: &["DEFINE", "FIELD"],
        yields: SlotYield::Keywords(&["IF", "OVERWRITE"]),
    },
    HeadRule {
        prefix: &["DEFINE", "FIELD", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["DEFINE", "FIELD", ANY, "ON"],
        yields: SlotYield::KeywordsAndTables(&["TABLE"]),
    },
    HeadRule {
        prefix: &["DEFINE", "FIELD", ANY, "ON", ANY],
        yields: SlotYield::Keywords(DEFINE_FIELD_CLAUSES),
    },
    HeadRule {
        prefix: &["DEFINE", "FIELD", ANY, "ON", "TABLE", ANY],
        yields: SlotYield::Keywords(DEFINE_FIELD_CLAUSES),
    },
    HeadRule {
        prefix: &["DEFINE", "EVENT"],
        yields: SlotYield::Keywords(&["IF", "OVERWRITE"]),
    },
    HeadRule {
        prefix: &["DEFINE", "EVENT", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["DEFINE", "EVENT", ANY, "ON"],
        yields: SlotYield::KeywordsAndTables(&["TABLE"]),
    },
    HeadRule {
        prefix: &["DEFINE", "EVENT", ANY, "ON", ANY],
        yields: SlotYield::Keywords(DEFINE_EVENT_CLAUSES),
    },
    HeadRule {
        prefix: &["DEFINE", "EVENT", ANY, "ON", "TABLE", ANY],
        yields: SlotYield::Keywords(DEFINE_EVENT_CLAUSES),
    },
    HeadRule {
        prefix: &["DEFINE", "INDEX"],
        yields: SlotYield::Keywords(&["IF", "OVERWRITE"]),
    },
    HeadRule {
        prefix: &["DEFINE", "INDEX", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["DEFINE", "INDEX", ANY, "ON"],
        yields: SlotYield::KeywordsAndTables(&["TABLE"]),
    },
    HeadRule {
        prefix: &["DEFINE", "INDEX", ANY, "ON", ANY],
        yields: SlotYield::Keywords(DEFINE_INDEX_CLAUSES),
    },
    HeadRule {
        prefix: &["DEFINE", "INDEX", ANY, "ON", "TABLE", ANY],
        yields: SlotYield::Keywords(DEFINE_INDEX_CLAUSES),
    },
    HeadRule {
        prefix: &["DEFINE", "USER"],
        yields: SlotYield::Keywords(&["IF", "OVERWRITE"]),
    },
    HeadRule {
        prefix: &["DEFINE", "USER", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["DEFINE", "USER", ANY, "ON"],
        yields: SlotYield::Keywords(BASE),
    },
    HeadRule {
        prefix: &["DEFINE", "ACCESS"],
        yields: SlotYield::Keywords(&["IF", "OVERWRITE"]),
    },
    HeadRule {
        prefix: &["DEFINE", "ACCESS", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["DEFINE", "ACCESS", ANY, "ON"],
        yields: SlotYield::Keywords(BASE),
    },
    HeadRule {
        prefix: &["DEFINE", "CONFIG"],
        yields: SlotYield::Keywords(&["IF", "OVERWRITE", "API", "GRAPHQL", "DEFAULT"]),
    },
    // ── REMOVE ── stmt/remove.rs:25-320
    HeadRule {
        prefix: &["REMOVE"],
        yields: SlotYield::Keywords(REMOVE_FORMS),
    },
    // `AND EXPUNGE` precedes `IF EXISTS`, and only NAMESPACE/DATABASE/TABLE
    // accept it (remove.rs:25-45).
    HeadRule {
        prefix: &["REMOVE", "NAMESPACE"],
        yields: SlotYield::Keywords(&["AND", "IF"]),
    },
    HeadRule {
        prefix: &["REMOVE", "DATABASE"],
        yields: SlotYield::Keywords(&["AND", "IF"]),
    },
    HeadRule {
        prefix: &["REMOVE", "TABLE"],
        yields: SlotYield::KeywordsAndTables(&["AND", "IF"]),
    },
    HeadRule {
        prefix: &["REMOVE", "NAMESPACE", "AND"],
        yields: SlotYield::Keywords(&["EXPUNGE"]),
    },
    HeadRule {
        prefix: &["REMOVE", "DATABASE", "AND"],
        yields: SlotYield::Keywords(&["EXPUNGE"]),
    },
    HeadRule {
        prefix: &["REMOVE", "TABLE", "AND"],
        yields: SlotYield::Keywords(&["EXPUNGE"]),
    },
    HeadRule {
        prefix: &["REMOVE", "FIELD", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["REMOVE", "FIELD", ANY, "ON"],
        yields: SlotYield::KeywordsAndTables(&["TABLE"]),
    },
    HeadRule {
        prefix: &["REMOVE", "EVENT", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["REMOVE", "EVENT", ANY, "ON"],
        yields: SlotYield::KeywordsAndTables(&["TABLE"]),
    },
    HeadRule {
        prefix: &["REMOVE", "INDEX", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["REMOVE", "INDEX", ANY, "ON"],
        yields: SlotYield::KeywordsAndTables(&["TABLE"]),
    },
    HeadRule {
        prefix: &["REMOVE", "USER", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["REMOVE", "USER", ANY, "ON"],
        yields: SlotYield::Keywords(BASE),
    },
    HeadRule {
        prefix: &["REMOVE", "ACCESS", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["REMOVE", "ACCESS", ANY, "ON"],
        yields: SlotYield::Keywords(BASE),
    },
    HeadRule {
        prefix: &["REMOVE", "ANALYZER"],
        yields: SlotYield::Analyzers,
    },
    HeadRule {
        prefix: &["ALTER", "ANALYZER"],
        yields: SlotYield::Analyzers,
    },
    HeadRule {
        prefix: &["REMOVE", "CONFIG"],
        yields: SlotYield::Keywords(&["IF", "GRAPHQL", "API", "DEFAULT"]),
    },
    // ── ALTER ── stmt/alter.rs:26-46
    HeadRule {
        prefix: &["ALTER"],
        yields: SlotYield::Keywords(ALTER_FORMS),
    },
    HeadRule {
        prefix: &["ALTER", "TABLE"],
        yields: SlotYield::KeywordsAndTables(&["IF"]),
    },
    HeadRule {
        prefix: &["ALTER", "FIELD", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["ALTER", "FIELD", ANY, "ON"],
        yields: SlotYield::KeywordsAndTables(&["TABLE"]),
    },
    HeadRule {
        prefix: &["ALTER", "EVENT", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["ALTER", "EVENT", ANY, "ON"],
        yields: SlotYield::KeywordsAndTables(&["TABLE"]),
    },
    HeadRule {
        prefix: &["ALTER", "INDEX", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["ALTER", "INDEX", ANY, "ON"],
        yields: SlotYield::KeywordsAndTables(&["TABLE"]),
    },
    HeadRule {
        prefix: &["ALTER", "USER", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["ALTER", "USER", ANY, "ON"],
        yields: SlotYield::Keywords(BASE),
    },
    HeadRule {
        prefix: &["ALTER", "ACCESS", ANY],
        yields: SlotYield::Keywords(&["ON"]),
    },
    HeadRule {
        prefix: &["ALTER", "ACCESS", ANY, "ON"],
        yields: SlotYield::Keywords(BASE),
    },
    // ── ACCESS ── stmt/mod.rs:129-271
    HeadRule {
        prefix: &["ACCESS", ANY],
        yields: SlotYield::Keywords(&["ON", "GRANT", "SHOW", "REVOKE", "PURGE"]),
    },
    HeadRule {
        prefix: &["ACCESS", ANY, "ON"],
        yields: SlotYield::Keywords(BASE),
    },
    HeadRule {
        prefix: &["ACCESS", ANY, "GRANT"],
        yields: SlotYield::Keywords(&["FOR"]),
    },
    HeadRule {
        prefix: &["ACCESS", ANY, "GRANT", "FOR"],
        yields: SlotYield::Keywords(&["USER", "RECORD"]),
    },
    HeadRule {
        prefix: &["ACCESS", ANY, "SHOW"],
        yields: SlotYield::Keywords(&["ALL", "GRANT", "WHERE"]),
    },
    HeadRule {
        prefix: &["ACCESS", ANY, "REVOKE"],
        yields: SlotYield::Keywords(&["ALL", "GRANT", "WHERE"]),
    },
    HeadRule {
        prefix: &["ACCESS", ANY, "PURGE"],
        yields: SlotYield::Keywords(&["EXPIRED", "REVOKED"]),
    },
    // ── shared: `IF` opens `IF NOT EXISTS` in every DEFINE/REMOVE/ALTER head,
    //    and `IF EXISTS` in REMOVE/ALTER/REBUILD. Both spellings are legal
    //    depending on the form, so offer both words.
    HeadRule {
        prefix: &["DEFINE", ANY, "IF"],
        yields: SlotYield::Keywords(&["NOT", "EXISTS"]),
    },
    HeadRule {
        prefix: &["DEFINE", ANY, "IF", "NOT"],
        yields: SlotYield::Keywords(&["EXISTS"]),
    },
    HeadRule {
        prefix: &["REMOVE", ANY, "IF"],
        yields: SlotYield::Keywords(&["EXISTS"]),
    },
    HeadRule {
        prefix: &["ALTER", ANY, "IF"],
        yields: SlotYield::Keywords(&["EXISTS"]),
    },
    HeadRule {
        prefix: &["REBUILD", "INDEX", "IF"],
        yields: SlotYield::Keywords(&["EXISTS"]),
    },
];

/// Slots identified by the *last* words typed rather than by the whole
/// statement.
///
/// The head table matches a statement from its first word, which cannot reach a
/// clause deep inside a long statement:
/// `DEFINE INDEX i ON person FIELDS name FULLTEXT ANALYZER ` is nine words in,
/// and enumerating every path to it is hopeless.
///
/// Kept to slots where a two-word tail is unambiguous on its own. `FULLTEXT
/// ANALYZER` appears in exactly one construct, so matching it needs no context.
const TAIL_RULES: &[(&[&str], SlotYield)] = &[(&["FULLTEXT", "ANALYZER"], SlotYield::Analyzers)];

/// The vocabulary legal directly after `words`.
///
/// `words` holds the statement's words up to the cursor, in order, already
/// split on whitespace and with the statement terminator removed. Matching is
/// case-insensitive, because SurrealQL keywords are.
///
/// Returns [`SlotYield::Expression`] when no rule matches, which every caller
/// must read as "offer what you offer today".
pub fn head_slot(words: &[&str]) -> SlotYield {
    let mut best: Option<(u32, SlotYield)> = None;
    for rule in HEAD_RULES {
        if !matches_prefix(rule.prefix, words) {
            continue;
        }
        let score = specificity(rule.prefix);
        if best.is_none_or(|(highest, _)| score > highest) {
            best = Some((score, rule.yields));
        }
    }
    if let Some((_, yields)) = best {
        return yields;
    }

    // No head rule matched. Try the tails.
    for (tail, yields) in TAIL_RULES {
        if words.len() >= tail.len() && matches_prefix(tail, &words[words.len() - tail.len()..]) {
            return *yields;
        }
    }

    SlotYield::Expression
}

/// How specific a prefix is, weighted so that the slot nearest the cursor
/// decides.
///
/// Two rules can match with the same number of wildcards. `DEFINE TABLE IF `
/// matches both `["DEFINE", "TABLE", ANY]` (reading `IF` as a table name) and
/// `["DEFINE", ANY, "IF"]` (reading it as the head of `IF NOT EXISTS`). The
/// second is right: a table actually called `IF` needs backticks, and the
/// prologue is the far commoner intent. Generalised, the word immediately
/// before the cursor is the strongest signal, so bit `i` carries position `i`
/// and a plain comparison weighs the last position most.
fn specificity(prefix: &[&str]) -> u32 {
    debug_assert!(prefix.len() <= 32, "a rule prefix must fit the score mask");
    prefix
        .iter()
        .enumerate()
        .filter(|(_, word)| **word != ANY)
        .fold(0, |mask, (index, _)| mask | 1 << index)
}

fn matches_prefix(prefix: &[&str], words: &[&str]) -> bool {
    prefix.len() == words.len()
        && prefix
            .iter()
            .zip(words)
            .all(|(expected, actual)| *expected == ANY || expected.eq_ignore_ascii_case(actual))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::KEYWORDS;

    fn keywords(words: &[&str]) -> Vec<&'static str> {
        match head_slot(words) {
            SlotYield::Keywords(list) | SlotYield::KeywordsAndTables(list) => list.to_vec(),
            other => panic!("expected a keyword slot for {words:?}, got {other:?}"),
        }
    }

    #[test]
    fn info_for_offers_exactly_the_nine_engine_targets() {
        // The reported bug: this position returned ~375 items.
        assert_eq!(
            keywords(&["INFO", "FOR"]),
            vec![
                "ROOT",
                "NAMESPACE",
                "NS",
                "DATABASE",
                "DB",
                "TABLE",
                "TB",
                "USER",
                "INDEX"
            ]
        );
    }

    #[test]
    fn info_for_does_not_offer_the_scope_targets_surrealdb_dropped() {
        let offered = keywords(&["INFO", "FOR"]);
        assert!(
            !offered.contains(&"SC") && !offered.contains(&"SCOPE"),
            "SurrealDB 3.x has no INFO FOR SCOPE arm (stmt/mod.rs:417-476), got {offered:?}"
        );
    }

    #[test]
    fn info_alone_offers_only_for() {
        assert_eq!(keywords(&["INFO"]), vec!["FOR"]);
    }

    #[test]
    fn info_for_table_offers_tables() {
        assert_eq!(head_slot(&["INFO", "FOR", "TABLE"]), SlotYield::Tables);
        assert_eq!(head_slot(&["INFO", "FOR", "TB"]), SlotYield::Tables);
    }

    #[test]
    fn matching_ignores_keyword_case() {
        assert_eq!(head_slot(&["info", "for"]), head_slot(&["INFO", "FOR"]));
        assert_eq!(head_slot(&["Info", "For", "Table"]), SlotYield::Tables);
    }

    #[test]
    fn a_spelled_word_beats_a_wildcard_at_the_same_slot() {
        // `["DEFINE", ANY, "IF"]` and `["REBUILD","INDEX","IF"]` both have
        // length 3; the wildcard rule must not shadow a literal one.
        assert_eq!(keywords(&["REBUILD", "INDEX", "IF"]), vec!["EXISTS"]);
    }

    #[test]
    fn the_slot_nearest_the_cursor_breaks_a_wildcard_tie() {
        // `DEFINE TABLE IF ` matches `["DEFINE","TABLE",ANY]` and
        // `["DEFINE",ANY,"IF"]` with one wildcard each. `IF` opens the
        // `IF NOT EXISTS` prologue; a table named `IF` needs backticks.
        assert_eq!(keywords(&["DEFINE", "TABLE", "IF"]), vec!["NOT", "EXISTS"]);
        // The same tie the other way round: a real name still reaches the bag.
        assert!(keywords(&["DEFINE", "TABLE", "person"]).contains(&"SCHEMAFULL"));
    }

    #[test]
    fn use_offers_the_four_scope_keywords_and_default() {
        assert_eq!(
            keywords(&["USE"]),
            vec!["NAMESPACE", "NS", "DATABASE", "DB", "DEFAULT"]
        );
    }

    #[test]
    fn define_offers_the_sixteen_sub_forms() {
        let offered = keywords(&["DEFINE"]);
        assert_eq!(offered.len(), 16, "got {offered:?}");
        assert!(offered.contains(&"SEQUENCE") && offered.contains(&"MODULE"));
    }

    #[test]
    fn define_does_not_offer_the_sub_forms_surrealdb_removed() {
        let offered = keywords(&["DEFINE"]);
        for gone in ["TOKEN", "SCOPE", "MODEL"] {
            assert!(!offered.contains(&gone), "3.x has no DEFINE {gone} arm");
        }
    }

    #[test]
    fn alter_offers_the_seventeen_sub_forms() {
        assert_eq!(keywords(&["ALTER"]).len(), 17);
    }

    #[test]
    fn remove_offers_the_sixteen_sub_forms() {
        assert_eq!(keywords(&["REMOVE"]).len(), 16);
    }

    #[test]
    fn define_table_clause_bag_uses_as_not_view() {
        let offered = keywords(&["DEFINE", "TABLE", "person"]);
        assert!(offered.contains(&"AS"), "the view clause opens with AS");
        assert!(
            !offered.contains(&"VIEW"),
            "DEFINE TABLE t VIEW … is a parse error (define.rs:710-722)"
        );
    }

    #[test]
    fn define_table_type_offers_only_the_three_table_types() {
        assert_eq!(
            keywords(&["DEFINE", "TABLE", "person", "TYPE"]),
            vec!["NORMAL", "RELATION", "ANY"]
        );
    }

    #[test]
    fn remove_namespace_offers_and_before_if() {
        // `REMOVE NAMESPACE [AND EXPUNGE] [IF EXISTS] <name>` — AND comes
        // first (remove.rs:25-45).
        assert_eq!(keywords(&["REMOVE", "NAMESPACE"]), vec!["AND", "IF"]);
        assert_eq!(keywords(&["REMOVE", "NAMESPACE", "AND"]), vec!["EXPUNGE"]);
    }

    #[test]
    fn on_clause_offers_the_three_bases_with_both_spellings() {
        assert_eq!(
            keywords(&["DEFINE", "USER", "bob", "ON"]),
            vec!["ROOT", "NAMESPACE", "NS", "DATABASE", "DB"]
        );
    }

    #[test]
    fn field_and_event_on_slots_offer_the_table_keyword_and_table_names() {
        for words in [
            vec!["DEFINE", "FIELD", "name", "ON"],
            vec!["DEFINE", "EVENT", "audit", "ON"],
            vec!["DEFINE", "INDEX", "idx", "ON"],
            vec!["REMOVE", "FIELD", "name", "ON"],
            vec!["ALTER", "FIELD", "name", "ON"],
        ] {
            assert_eq!(
                head_slot(&words),
                SlotYield::KeywordsAndTables(&["TABLE"]),
                "{words:?} must offer TABLE and the known tables"
            );
        }
    }

    #[test]
    fn the_field_clause_bag_survives_the_optional_table_keyword() {
        let without = head_slot(&["DEFINE", "FIELD", "name", "ON", "person"]);
        let with = head_slot(&["DEFINE", "FIELD", "name", "ON", "TABLE", "person"]);
        assert_eq!(without, with, "`ON TABLE t` and `ON t` are the same slot");
    }

    #[test]
    fn access_purge_offers_only_the_two_grant_states() {
        assert_eq!(
            keywords(&["ACCESS", "api", "PURGE"]),
            vec!["EXPIRED", "REVOKED"]
        );
    }

    #[test]
    fn an_unmodelled_position_falls_back_to_the_current_behaviour() {
        // Every one of these keeps today's full list rather than guessing.
        for words in [
            vec![],
            vec!["SELECT"],
            vec!["SELECT", "*", "FROM", "person"],
            vec![
                "SELECT", "*", "FROM", "person", "WHERE", "age", ">", "3", "AND",
            ],
            vec!["CREATE"],
            vec!["LET"],
            vec!["DEFINE", "FIELD", "name", "ON", "person", "DEFAULT"],
            vec!["INFO", "FOR", "USER"],
            vec!["REMOVE", "USER"],
            vec!["SLEEP"],
            vec!["KILL"],
        ] {
            assert_eq!(
                head_slot(&words),
                SlotYield::Expression,
                "{words:?} must not be narrowed"
            );
        }
    }

    #[test]
    fn no_slot_ever_yields_an_empty_keyword_list() {
        // An empty list is a wrong answer; the fallback for "we do not know"
        // is `Expression`.
        for rule in HEAD_RULES {
            match rule.yields {
                SlotYield::Keywords(list) | SlotYield::KeywordsAndTables(list) => assert!(
                    !list.is_empty(),
                    "{:?} yields an empty keyword list",
                    rule.prefix
                ),
                SlotYield::Tables | SlotYield::Analyzers | SlotYield::Expression => {}
            }
        }
    }

    #[test]
    fn every_rule_prefix_starts_with_a_statement_keyword() {
        for rule in HEAD_RULES {
            let first = rule.prefix.first().expect("a rule needs a prefix");
            assert_ne!(*first, ANY, "a rule must not open with a wildcard");
            assert!(
                KEYWORDS.contains(first),
                "`{first}` is not a grammar keyword"
            );
        }
    }

    #[test]
    fn offers_outside_the_grammar_are_declared() {
        // Catches a typo in the tables: any offered word that the pinned
        // grammar cannot lex must be a known, documented gap.
        for rule in HEAD_RULES {
            let offered = match rule.yields {
                SlotYield::Keywords(list) | SlotYield::KeywordsAndTables(list) => list,
                SlotYield::Tables | SlotYield::Analyzers | SlotYield::Expression => continue,
            };
            for word in offered {
                assert!(
                    KEYWORDS.contains(word) || OFFERS_THE_GRAMMAR_CANNOT_PARSE.contains(word),
                    "`{word}` (offered at {:?}) is neither a grammar keyword nor a \
                     declared gap — fix the spelling or add it to \
                     OFFERS_THE_GRAMMAR_CANNOT_PARSE",
                    rule.prefix
                );
            }
        }
    }

    #[test]
    fn every_declared_grammar_gap_is_really_absent_from_the_grammar() {
        // Keeps the gap list honest: a grammar bump that adds one of these
        // words must shorten the list rather than leave a stale entry.
        for word in OFFERS_THE_GRAMMAR_CANNOT_PARSE {
            assert!(
                !KEYWORDS.contains(word),
                "`{word}` is in the grammar now — remove it from \
                 OFFERS_THE_GRAMMAR_CANNOT_PARSE"
            );
        }
    }
}
