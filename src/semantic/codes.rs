//! Stable diagnostic codes attached to every LSP `Diagnostic`.
//!
//! These strings are wire-visible: clients (and this server's own code
//! actions) match on them, so treat them like a public API — never
//! rename an existing code, only add new ones.

use ls_types::NumberOrString;

/// Tree-sitter parse failures (both `ERROR` and `MISSING` nodes).
pub const PARSE: &str = "parse";
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

/// Wrap a code constant in the LSP `Diagnostic.code` representation.
pub fn as_code(value: &str) -> Option<NumberOrString> {
    Some(NumberOrString::String(value.to_string()))
}

/// True when the diagnostic carries the given stable code.
pub fn has_code(diagnostic: &ls_types::Diagnostic, code: &str) -> bool {
    matches!(&diagnostic.code, Some(NumberOrString::String(value)) if value == code)
}
