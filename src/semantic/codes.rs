//! Stable diagnostic codes attached to every LSP `Diagnostic`.
//!
//! These strings are wire-visible: clients (and this server's own code
//! actions) match on them, so treat them like a public API — never
//! rename an existing code, only add new ones.

use ls_types::NumberOrString;

/// Tree-sitter parse failures (both `ERROR` and `MISSING` nodes).
///
/// Only syntax. The analyzer's own refusals carry [`DOCUMENT_TOO_LARGE`],
/// [`TOO_DEEPLY_NESTED`] and [`BUFFER_DESYNCED`] instead: a machine consumer is
/// told to treat `parse` as "this query does not compile", and a size-limit
/// notice reported under that code would be a lie it acts on.
pub const PARSE: &str = "parse";
/// The document is over `analysis.maxDocumentBytes`, so nothing was analysed.
/// A notice about the server's own limit, not a judgement about the file.
pub const DOCUMENT_TOO_LARGE: &str = "document-too-large";
/// Brackets or tree nodes nest past `MAX_NODE_DEPTH`, so the analyzer declined
/// to descend. SurrealDB's parser refuses this shape too, at a lower limit.
pub const TOO_DEEPLY_NESTED: &str = "too-deeply-nested";
/// The server's copy of the buffer fell out of step with the editor's, so
/// ranged edits are being ignored. Reported in the document because the output
/// channel is not somewhere a user looks when diagnostics stop moving.
pub const BUFFER_DESYNCED: &str = "buffer-desynced";
/// A query targets a table with no known definition.
pub const UNKNOWN_TABLE: &str = "unknown-table";
/// A query touches a field that isn't defined on an explicit table.
pub const UNKNOWN_FIELD: &str = "unknown-field";
/// Static permission evaluation proved the active auth context is
/// denied.
pub const PERMISSION_DENIED: &str = "permission-denied";
/// Static permission evaluation could not decide (row-level rules).
pub const PERMISSION_UNKNOWN: &str = "permission-unknown";
/// The statement target could not be resolved to a static table name.
pub const DYNAMIC_TARGET: &str = "dynamic-target";
/// An argument's type cannot satisfy the declared parameter type.
pub const ARGUMENT_TYPE: &str = "argument-type";
/// A call passes too many or too few arguments.
pub const ARGUMENT_COUNT: &str = "argument-count";
/// A `LET $x: T = …` value cannot satisfy the declared type `T`.
pub const LET_TYPE: &str = "let-type";
/// A `RETURN` inside `DEFINE FUNCTION … -> T` yields a value that cannot
/// satisfy `T`. The engine coerces a function's result to its declared type and
/// fails with `Couldn't coerce return value from function …`.
pub const RETURN_TYPE: &str = "return-type";
/// An arithmetic operator whose operand types SurrealDB rejects, such as
/// `"a" + 1`. The engine fails with `Cannot perform addition with …`
/// (`err/mod.rs`), and the operand tables it checks against are transcribed in
/// [`crate::semantic::operate`].
pub const OPERATOR_TYPE: &str = "operator-type";
/// A method the receiver's type does not have, such as `"abc".nonsense()`. The
/// engine refuses it with `no such method found for the string type`. Only
/// reported when the receiver's type is certain — see
/// [`crate::semantic::method::receiver_kind`].
pub const UNKNOWN_METHOD: &str = "unknown-method";
/// A `$variable` reference that nothing in scope binds.
pub const UNDEFINED_VARIABLE: &str = "undefined-variable";
/// A builtin function called by a name SurrealDB has renamed. The engine still
/// accepts it and records the replacement itself, so this is a warning with a
/// quick fix rather than an error.
pub const RENAMED_FUNCTION: &str = "renamed-function";
/// A builtin the parser accepts that no implementation backs in call form, so
/// the query parses and then fails at run time. A warning rather than an error,
/// because the claim rests on reading the engine's dispatch tables.
pub const NOT_CALLABLE: &str = "not-callable";
/// A `DEFINE FIELD … TYPE T` whose `DEFAULT`, `VALUE` or `COMPUTED` expression cannot
/// satisfy `T`. The engine coerces all three to the declared type and fails with
/// `Couldn't coerce value for field …`.
///
/// `ASSERT` is deliberately **absent**: it is a predicate over `$value`, not a value
/// coerced to `T`, so nothing may compare it against the declared type. Kept separate
/// from [`LET_TYPE`] because this is the first check that fires on `DEFINE FIELD` at
/// all, and a client must be able to suppress it alone.
pub const FIELD_TYPE: &str = "field-type";
/// A type position holds a word SurrealDB's kind grammar does not have, such as
/// `LET $x: xxx = 2`. Unlike every other code here this is a *syntax* fault, not a
/// judgement about a value: the engine refuses to parse it at all
/// (`syn/parser/kind.rs:218`, `expected a kind name`), so the query never runs. It is
/// therefore reported from the syntax pass and is not gated by
/// `analysis.enable_type_checking` — see [`crate::semantic::type_name`].
pub const UNKNOWN_TYPE: &str = "unknown-type";

/// Every code this server emits, in the order `docs/diagnostics.md` documents
/// them.
///
/// Exists so three things cannot drift apart: the codes, the prose that explains
/// them, and the `codeDescription` link every diagnostic carries. A code with no
/// section, or a section with no code, fails
/// `every_code_is_documented` in `tests/compat.rs`.
pub const ALL: &[&str] = &[
    PARSE,
    DOCUMENT_TOO_LARGE,
    TOO_DEEPLY_NESTED,
    BUFFER_DESYNCED,
    UNKNOWN_TYPE,
    UNKNOWN_TABLE,
    UNKNOWN_FIELD,
    PERMISSION_DENIED,
    PERMISSION_UNKNOWN,
    DYNAMIC_TARGET,
    ARGUMENT_TYPE,
    ARGUMENT_COUNT,
    LET_TYPE,
    RETURN_TYPE,
    FIELD_TYPE,
    OPERATOR_TYPE,
    UNKNOWN_METHOD,
    UNDEFINED_VARIABLE,
    RENAMED_FUNCTION,
    NOT_CALLABLE,
];

/// Where the prose for `code` lives, for `Diagnostic.codeDescription`.
///
/// Points into this repository at **this build's own tag**, not at `master`.
/// `master` is the branch that moves: a link to it would drift away from the
/// binary the user is running, which is the opposite of what a stable code is
/// for. Every published build (crates.io, npm, the release binaries) comes from
/// a `v<version>` tag, so the link resolves for anything anyone installs.
///
/// The one case it does not resolve is a build from a working tree whose
/// version has not been tagged yet, which is a state only this repository's own
/// developers are ever in.
///
/// A `codeDescription` pointing at a 404 renders as a dead hyperlink in VS Code,
/// which is worse than none: `every_code_is_documented` is what keeps the anchor
/// half honest.
pub fn description(code: &str) -> Option<ls_types::CodeDescription> {
    if !ALL.contains(&code) {
        return None;
    }
    let href: ls_types::Uri = format!(
        "https://github.com/surrealdb/surrealql-language-server/blob/v{}/docs/diagnostics.md#{code}",
        env!("CARGO_PKG_VERSION")
    )
    .parse()
    .ok()?;
    Some(ls_types::CodeDescription { href })
}

/// The codes whose behavior on a `SCHEMALESS` table the
/// `analysis.schemalessDiagnostics` setting decides. Every other code is
/// unconditional: it judges an expression, not the schema, so a loose schema
/// says nothing about whether it is right.
pub const SCHEMALESS_SCOPED_CODES: &[&str] = &[
    UNKNOWN_FIELD,
    PERMISSION_DENIED,
    PERMISSION_UNKNOWN,
    FIELD_TYPE,
    UNKNOWN_TYPE,
];

/// Whether `code` is reported on a table declared `SCHEMALESS`, under the
/// `analysis.schemalessDiagnostics` value `mode`.
///
/// Stated in the positive so one predicate drives both directions: the
/// `unknown-field` check is an *allowlist* (it fires only where the schema is
/// closed, [`crate::semantic::model`]), while the other four are emitted
/// unconditionally and have to be filtered out.
///
/// `strict` answers `true` for everything — it is the opt-in that makes a
/// `SCHEMALESS` table behave exactly like a `SCHEMAFULL` one. `errors` keeps
/// only the two faults the engine itself raises: it coerces `DEFAULT`/`VALUE`/
/// `COMPUTED` to the declared type regardless of schema mode ([`FIELD_TYPE`]),
/// and it refuses to parse an unknown type name at all ([`UNKNOWN_TYPE`]).
///
/// An unrecognized `mode` answers as `quiet`, the default. `validate_and_repair`
/// in [`crate::config`] already replaced it and warned, so this is unreachable
/// in practice.
pub fn reports_on_schemaless(code: &str, mode: &str) -> bool {
    match mode {
        "strict" => true,
        "errors" => matches!(code, FIELD_TYPE | UNKNOWN_TYPE),
        _ => false,
    }
}

/// Wrap a code constant in the LSP `Diagnostic.code` representation.
pub fn as_code(value: &str) -> Option<NumberOrString> {
    Some(NumberOrString::String(value.to_string()))
}

/// True when the diagnostic carries the given stable code.
pub fn has_code(diagnostic: &ls_types::Diagnostic, code: &str) -> bool {
    matches!(&diagnostic.code, Some(NumberOrString::String(value)) if value == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_reports_nothing_in_scope() {
        for code in SCHEMALESS_SCOPED_CODES {
            assert!(
                !reports_on_schemaless(code, "quiet"),
                "`{code}` must stay silent under `quiet`"
            );
        }
    }

    #[test]
    fn errors_reports_only_the_faults_the_engine_raises() {
        assert!(reports_on_schemaless(FIELD_TYPE, "errors"));
        assert!(reports_on_schemaless(UNKNOWN_TYPE, "errors"));
        assert!(!reports_on_schemaless(UNKNOWN_FIELD, "errors"));
        assert!(!reports_on_schemaless(PERMISSION_DENIED, "errors"));
        assert!(!reports_on_schemaless(PERMISSION_UNKNOWN, "errors"));
    }

    #[test]
    fn strict_reports_everything_in_scope() {
        for code in SCHEMALESS_SCOPED_CODES {
            assert!(
                reports_on_schemaless(code, "strict"),
                "`{code}` must report under `strict`"
            );
        }
    }

    /// An unknown value must not accidentally select `strict`; the config
    /// layer repairs it to the default, and this is the belt-and-braces half.
    #[test]
    fn an_unknown_mode_behaves_as_quiet() {
        for code in SCHEMALESS_SCOPED_CODES {
            assert!(!reports_on_schemaless(code, "nonsense"));
        }
    }
}
