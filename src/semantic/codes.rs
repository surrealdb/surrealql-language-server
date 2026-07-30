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

/// Wrap a code constant in the LSP `Diagnostic.code` representation.
pub fn as_code(value: &str) -> Option<NumberOrString> {
    Some(NumberOrString::String(value.to_string()))
}

/// True when the diagnostic carries the given stable code.
pub fn has_code(diagnostic: &ls_types::Diagnostic, code: &str) -> bool {
    matches!(&diagnostic.code, Some(NumberOrString::String(value)) if value == code)
}
