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

/// Wrap a code constant in the LSP `Diagnostic.code` representation.
pub fn as_code(value: &str) -> Option<NumberOrString> {
    Some(NumberOrString::String(value.to_string()))
}

/// True when the diagnostic carries the given stable code.
pub fn has_code(diagnostic: &ls_types::Diagnostic, code: &str) -> bool {
    matches!(&diagnostic.code, Some(NumberOrString::String(value)) if value == code)
}
