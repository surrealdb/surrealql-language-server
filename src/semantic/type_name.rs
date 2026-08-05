//! Which bare words SurrealQL accepts as a type name.
//!
//! SurrealDB's type grammar is a **closed keyword set**, not an open namespace.
//! `parse_concrete_kind` ends with `_ => unexpected!(self, next, "a kind name")`
//! (`surrealdb-core/src/syn/parser/kind.rs:218`), so an unrecognised word is a
//! parse failure in the engine and the query never runs.
//!
//! That is the whole reason this module can be certain where
//! [`crate::semantic::assign`] deliberately is not. `assign` answers "is this
//! coercion safe to judge?", and its doc comment requires silence on an
//! unrecognised name so a new SurrealDB release cannot make the checker flag
//! working code. This module answers a different and much narrower question —
//! "does the parser have this word at all?" — where the answer is a fact about a
//! finite keyword table rather than an inference. The two must not be merged:
//! [`PRIMITIVES`](crate::semantic::assign) is a coercion allowlist and diverges
//! from this list on purpose.

/// The type names SurrealDB's kind grammar accepts.
///
/// Transcribed from the `t!("…")` arms of `parse_concrete_kind` and
/// `parse_inner_single_kind` (`syn/parser/kind.rs:45-220`), then verified one name
/// at a time against a live engine (version 3.2.3).
///
/// Two entries need a word of explanation:
///
/// * `option` is here although it is legal *only* as `option<T>` — bare `option`
///   fails with `expected <`. Arity is a separate rule this module does not model,
///   so the name counts as known.
/// * `value` is deliberately **absent**. `LET $x: value = 1` fails with
///   ``Unexpected token `VALUE`, expected a kind name``, even though `VALUE` is a
///   keyword elsewhere in the language. Note that `assign::PRIMITIVES` does list
///   `value`, because there it means "the top of the lattice", not "a name an
///   author may write".
pub const KIND_NAMES: &[&str] = &[
    "any", "array", "bool", "bytes", "datetime", "decimal", "duration", "file", "float",
    "function", "geometry", "int", "none", "null", "number", "object", "option", "point", "range",
    "record", "regex", "set", "string", "table", "uuid",
];

/// The two boolean literal types, which reach a type position as a bare
/// `TypeName`.
///
/// The engine reads them as literal types (`t!("true") => Kind::Literal(…)`), so
/// `<true> true` and `LET $x: false = 1` are both legal — SurrealDB's own
/// `casting/basic_literal_kind` test relies on it.
///
/// Kept apart from [`KIND_NAMES`] because they are not kind *names*, and they are
/// here at all only because the tree-sitter `LiteralType` rule covers `String`,
/// `Number`, `Duration`, `ArrayType` and `ObjectType` but omits booleans. `NaN`
/// and `Infinity` need no entry: the grammar reads those as
/// `LiteralType(Number(Float))`, so they never arrive as a `TypeName`.
const BOOLEAN_LITERAL_TYPES: &[&str] = &["true", "false"];

/// Constructors whose `<…>` arguments name something other than a type.
///
/// This list is what stops the check reporting valid SurrealQL. `record<person>`,
/// `table<person>`, `file<bucket>` and `geometry<multipoint>` all parse to the very
/// same shape, `ParameterizedType(TypeName, TypeName)`, so the argument cannot be
/// told apart from a type by its node kind alone:
///
/// * `record` and `table` take table names,
/// * `file` takes bucket names,
/// * `geometry` takes a name from its own closed set (`point`, `line`, `polygon`,
///   `multipoint`, `multiline`, `multipolygon`, `collection`), which this module
///   does not check.
///
/// `array`, `set` and `option` are absent on purpose: their arguments really are
/// types, and `array<xxx>` must be reported.
pub const FOREIGN_ARGUMENT_CONSTRUCTORS: &[&str] = &["record", "table", "file", "geometry"];

/// True when SurrealDB's kind grammar has this name.
///
/// Case-insensitive, because the engine lexes these through `UniCase`
/// (`syn/lexer/keywords.rs`) and so accepts `INT` and `String`.
pub fn is_known(name: &str) -> bool {
    KIND_NAMES
        .iter()
        .chain(BOOLEAN_LITERAL_TYPES)
        .any(|known| known.eq_ignore_ascii_case(name))
}

/// True when `name` is a constructor whose `<…>` arguments are not types.
pub fn takes_foreign_arguments(name: &str) -> bool {
    FOREIGN_ARGUMENT_CONSTRUCTORS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(name))
}

/// The closest real type name to `name`, for a "did you mean" hint.
///
/// Two guards, both borrowed from [`super::analyzer`]'s `keyword_typo_hint`:
///
/// * a `jaro_winkler` score above 0.86, and
/// * a length difference of three characters or less.
///
/// The length guard is what makes this safe on a list of very short words. Without
/// it the prefix bonus pairs any two names that merely start alike. Three rather
/// than two because the useful abbreviations are exactly three characters short of
/// their target — `str`, `rec`, `num` and `obj` all land on the right name at 0.88,
/// while unrelated words (`text`, `json`, `char`) stay well under the threshold.
pub fn nearest(name: &str) -> Option<&'static str> {
    KIND_NAMES
        .iter()
        .filter(|known| known.len().abs_diff(name.len()) <= 3)
        .map(|known| {
            (
                strsim::jaro_winkler(&name.to_ascii_lowercase(), known),
                *known,
            )
        })
        .filter(|(score, _)| *score > 0.86)
        .max_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, known)| known)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_every_name_the_engine_accepts() {
        for name in KIND_NAMES {
            assert!(is_known(name), "{name} must be known");
        }
    }

    #[test]
    fn ignores_letter_case_because_the_engine_does() {
        // `LET $a: INT = 2` and `LET $a: String = 2` both parse in the engine.
        for name in ["INT", "String", "RECORD", "Option", "TABLE"] {
            assert!(is_known(name), "{name} must be known");
        }
    }

    #[test]
    fn rejects_a_word_the_kind_grammar_does_not_have() {
        for name in ["xxx", "strng", "integer", "boolean", "text", "json", "char"] {
            assert!(!is_known(name), "{name} must not be known");
        }
    }

    #[test]
    fn rejects_value_although_it_is_a_keyword_elsewhere() {
        // `LET $x: value = 1` -> "Unexpected token `VALUE`, expected a kind name".
        // `assign::PRIMITIVES` lists it for a different purpose; see this module's
        // doc comment.
        assert!(!is_known("value"));
    }

    #[test]
    fn accepts_table_which_the_coercion_allowlist_omits() {
        // `LET $x: table = 1` parses. `assign::PRIMITIVES` does not list `table`,
        // which is a separate, pre-existing divergence.
        assert!(is_known("table"));
    }

    #[test]
    fn suggests_the_obvious_correction() {
        assert_eq!(nearest("strng"), Some("string"));
        assert_eq!(nearest("nubmer"), Some("number"));
        assert_eq!(nearest("recrod"), Some("record"));
        assert_eq!(nearest("datetim"), Some("datetime"));
    }

    #[test]
    fn suggests_the_target_of_a_short_abbreviation() {
        // The reason the length guard is three and not two.
        assert_eq!(nearest("str"), Some("string"));
        assert_eq!(nearest("rec"), Some("record"));
        assert_eq!(nearest("num"), Some("number"));
        assert_eq!(nearest("obj"), Some("object"));
    }

    #[test]
    fn stays_quiet_when_nothing_is_close() {
        // A wrong suggestion is worse than none: it sends the reader to a type that
        // is not what they meant.
        for name in ["xxx", "zzz", "yyy", "text", "json", "char", "varchar"] {
            assert_eq!(nearest(name), None, "{name} must get no suggestion");
        }
    }
}
