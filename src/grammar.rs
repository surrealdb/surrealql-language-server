use std::collections::HashMap;
use std::sync::LazyLock;

use tree_sitter::Language;

use crate::grammar_generated::{GENERATED_FUNCTIONS, RENAMED_FUNCTIONS};
use crate::semantic::type_expr::TypeExpr;

unsafe extern "C" {
    fn tree_sitter_surrealql() -> *const tree_sitter::ffi::TSLanguage;
}

include!(concat!(env!("OUT_DIR"), "/keywords.rs"));

#[derive(Debug, Clone, Copy)]
pub struct BuiltinNamespace {
    pub name: &'static str,
    pub summary: &'static str,
    pub documentation_url: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct BuiltinFunction {
    pub name: &'static str,
    pub signature: &'static str,
    pub summary: &'static str,
    pub documentation_url: &'static str,
}

/// How many arguments one generated parameter accounts for.
///
/// Mirrors the engine's argument wrappers (`fnc/args.rs`). `Optional` is the
/// engine's `Optional<T>`, which lowers the arity bound only — a supplied
/// argument must still satisfy `ty`. It is *not* the SurrealQL type
/// `option<T>`, which additionally permits `NONE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamForm {
    Required,
    Optional,
    Variadic,
}

/// One parameter of a builtin function, as read from the engine's source.
#[derive(Debug, Clone, Copy)]
pub struct GeneratedParam {
    /// The binding name in the engine's implementation, for parameter labels.
    pub name: &'static str,
    /// A SurrealQL type name, parsed into a `TypeExpr` on first use.
    ///
    /// `any` means "the generator could not model this", which silences the
    /// argument check for the position rather than risking a false positive.
    pub ty: &'static str,
    pub form: ParamForm,
}

/// One method, and the function `fnc::idiom` dispatches it to.
///
/// Generated from the engine's receiver tables. See
/// [`crate::grammar_generated::GENERATED_RECEIVERS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedMethod {
    /// The name written after the dot.
    pub method: &'static str,
    /// The function this resolves to, ready for [`builtin_signature`].
    ///
    /// Where several names share one implementation — `array::all` and
    /// `array::every` do — this is whichever sorts first. They have identical
    /// parameters by construction, so only the name shown in hover differs.
    pub function: &'static str,
    /// The experimental target this method sits behind, when it has one. Every
    /// `file::` method is behind `Files`.
    pub experimental: Option<&'static str>,
}

/// The methods one receiver kind accepts.
#[derive(Debug, Clone, Copy)]
pub struct GeneratedReceiver {
    /// The engine `Value` variant, or `""` for the catch-all table that serves
    /// `bool`, `uuid`, `regex`, `range`, `none` and the rest.
    pub kind: &'static str,
    pub methods: &'static [GeneratedMethod],
}

/// One entry of the generated catalogue.
///
/// Deliberately separate from [`BuiltinFunction`]: that table carries curated
/// prose (`summary`, `documentation_url`) which no generator can produce, and
/// this one carries the argument and return types which no human should
/// transcribe 434 times. Lookups read both.
#[derive(Debug, Clone, Copy)]
pub struct GeneratedFunction {
    pub name: &'static str,
    pub params: &'static [GeneratedParam],
    pub is_async: bool,
    /// The parser accepts this name but no dispatch arm implements it in call
    /// form, so a query using it parses and then fails at run time.
    pub not_callable: bool,
    /// True when the generator read this function's implementation.
    ///
    /// Every argument check must consult this first. `params: &[]` with
    /// `signature_known: false` means "we do not know", and an empty list is
    /// indistinguishable from a genuinely zero-argument function without it —
    /// so checking arity anyway would report "expects 0 arguments" for every
    /// call to a function whose signature the generator could not read.
    pub signature_known: bool,
    /// A SurrealQL type name, parsed into a `TypeExpr` on first use.
    ///
    /// `any` means "the engine declared nothing usable here", which reads as
    /// unknown and silences every check downstream of it. That covers two cases
    /// worth keeping apart in your head: a return type the engine's macros
    /// cannot spell, and a return type that genuinely follows an argument
    /// (`array::first` returns whatever the array holds). Both must stay quiet.
    pub returns: &'static str,
}

/// Every namespace SurrealDB actually implements, read from its own function
/// table by `cargo xtask generate-builtins`.
///
/// This replaced a hand-written list that had drifted: it offered `not::` and
/// `sleep::`, which are *bare* functions and not namespaces at all, and it hid
/// eight real ones — `api::`, `bytes::`, `eval::`, `file::`, `schema::`,
/// `sequence::`, `set::` and `value::`. `set::` was the costly omission: 24
/// functions that the method checker already resolved but completion never
/// offered. `tests/generated_catalogue.rs` has asserted the generated list is
/// the correct one since the catalogue was first generated; nothing consumed it.
pub use crate::grammar_generated::GENERATED_NAMESPACES;

/// The method tables, one per receiver `Value` variant plus the engine's
/// catch-all. Read by [`crate::semantic::method`].
pub use crate::grammar_generated::GENERATED_RECEIVERS;

/// Every function name the parser accepts, with the argument types its
/// implementation declares. Completion reads this as well as
/// [`BUILTIN_FUNCTIONS`]: the curated table covers two namespaces, and this one
/// covers all 26.
pub use crate::grammar_generated::GENERATED_FUNCTIONS as GENERATED_FUNCTION_TABLE;

/// Constants such as `math::PI`, which take no arguments and are not called.
pub use crate::grammar_generated::GENERATED_CONSTANTS;

pub const BUILTIN_NAMESPACE_DOCS: &[BuiltinNamespace] = &[
    BuiltinNamespace {
        name: "string::",
        summary: "Builtin functions for string validation and text manipulation.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinNamespace {
        name: "type::",
        summary: "Builtin functions for type coercion, inspection, and record construction.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
];

pub const BUILTIN_FUNCTIONS: &[BuiltinFunction] = &[
    BuiltinFunction {
        name: "string::capitalize",
        signature: "string::capitalize(string) -> string",
        summary: "Capitalizes each word in a string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::concat",
        signature: "string::concat(value...) -> string",
        summary: "Concatenates values into a single string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::contains",
        signature: "string::contains(string, predicate: string) -> bool",
        summary: "Checks whether a string contains another string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::ends_with",
        signature: "string::ends_with(string, other: string) -> bool",
        summary: "Checks whether a string ends with another string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::join",
        signature: "string::join(delimiter: value, value...) -> string",
        summary: "Joins values together with a delimiter.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::len",
        signature: "string::len(string) -> number",
        summary: "Returns the length of a string in characters.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::lowercase",
        signature: "string::lowercase(string) -> string",
        summary: "Converts a string to lowercase.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::matches",
        signature: "string::matches(string, pattern: string|regex) -> bool",
        summary: "Performs a regex match on a string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::repeat",
        signature: "string::repeat(string, count: number) -> string",
        summary: "Repeats a string a number of times.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::replace",
        signature: "string::replace(string, pattern: string, replacement: string) -> string",
        summary: "Replaces part of a string with another string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::slice",
        signature: "string::slice(string, from: number, to?: number) -> string",
        summary: "Returns a substring from a string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::slug",
        signature: "string::slug(string) -> string",
        summary: "Converts a string into a slug-safe form.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::split",
        signature: "string::split(string, delimiter: string|regex) -> array<string>",
        summary: "Splits a string by a delimiter or regex.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::starts_with",
        signature: "string::starts_with(string, other: string) -> bool",
        summary: "Checks whether a string starts with another string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::trim",
        signature: "string::trim(string) -> string",
        summary: "Trims leading and trailing whitespace.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::uppercase",
        signature: "string::uppercase(string) -> string",
        summary: "Converts a string to uppercase.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::words",
        signature: "string::words(string) -> array<string>",
        summary: "Splits a string into words.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::html::sanitize",
        signature: "string::html::sanitize(string) -> string",
        summary: "Sanitizes HTML while keeping safe markup intact.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_alphanum",
        signature: "string::is_alphanum(any) -> bool",
        summary: "Checks whether a value is alphanumeric text.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_alpha",
        signature: "string::is_alpha(any) -> bool",
        summary: "Checks whether a value only contains letters.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_ascii",
        signature: "string::is_ascii(any) -> bool",
        summary: "Checks whether a value only contains ASCII characters.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_datetime",
        signature: "string::is_datetime(any) -> bool",
        summary: "Checks whether a value is a valid datetime string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_domain",
        signature: "string::is_domain(any) -> bool",
        summary: "Checks whether a value is a valid domain name.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_email",
        signature: "string::is_email(any) -> bool",
        summary: "Checks whether a value is a valid email address.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_hexadecimal",
        signature: "string::is_hexadecimal(any) -> bool",
        summary: "Checks whether a value is valid hexadecimal text.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_ip",
        signature: "string::is_ip(any) -> bool",
        summary: "Checks whether a value is a valid IP address.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_ipv4",
        signature: "string::is_ipv4(any) -> bool",
        summary: "Checks whether a value is a valid IPv4 address.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_ipv6",
        signature: "string::is_ipv6(any) -> bool",
        summary: "Checks whether a value is a valid IPv6 address.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_latitude",
        signature: "string::is_latitude(any) -> bool",
        summary: "Checks whether a value is a valid latitude.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_longitude",
        signature: "string::is_longitude(any) -> bool",
        summary: "Checks whether a value is a valid longitude.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_numeric",
        signature: "string::is_numeric(any) -> bool",
        summary: "Checks whether a value is numeric text.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_semver",
        signature: "string::is_semver(any) -> bool",
        summary: "Checks whether a value is a semantic-version string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_ulid",
        signature: "string::is_ulid(any) -> bool",
        summary: "Checks whether a value is a valid ULID.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_url",
        signature: "string::is_url(any) -> bool",
        summary: "Checks whether a value is a valid URL.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "string::is_uuid",
        signature: "string::is_uuid(any) -> bool",
        summary: "Checks whether a value is a valid UUID string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/string",
    },
    BuiltinFunction {
        name: "type::array",
        signature: "type::array(any) -> array",
        summary: "Converts a value into an array.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::bool",
        signature: "type::bool(any) -> bool",
        summary: "Converts a value into a boolean.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::bytes",
        signature: "type::bytes(any) -> bytes",
        summary: "Converts a value into bytes.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::datetime",
        signature: "type::datetime(any) -> datetime",
        summary: "Converts a value into a datetime.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::decimal",
        signature: "type::decimal(any) -> decimal",
        summary: "Converts a value into a decimal.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::duration",
        signature: "type::duration(any) -> duration",
        summary: "Converts a value into a duration.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::field",
        signature: "type::field(any) -> field",
        summary: "Projects a single field in a SELECT statement.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::fields",
        signature: "type::fields(any...) -> array<field>",
        summary: "Projects multiple fields in a SELECT statement.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::file",
        signature: "type::file(path: string, mime?: string) -> file",
        summary: "Builds a file pointer from string input.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::float",
        signature: "type::float(any) -> float",
        summary: "Converts a value into a float.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::int",
        signature: "type::int(any) -> int",
        summary: "Converts a value into an integer.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::number",
        signature: "type::number(any) -> number",
        summary: "Converts a value into a number.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::of",
        signature: "type::of(value: any) -> string",
        summary: "Returns the SurrealQL type name of a value.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::point",
        signature: "type::point(array|point) -> point",
        summary: "Converts a value into a geometry point.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::range",
        signature: "type::range(range|array) -> range<record>",
        summary: "Converts a value into a range.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::record",
        signature: "type::record(table: any, key: any) -> record",
        summary: "Builds a record id from a table and key.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::string",
        signature: "type::string(any) -> string",
        summary: "Converts a value into a string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::string_lossy",
        signature: "type::string_lossy(any) -> string",
        summary: "Converts a value into a string, replacing invalid byte sequences.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::table",
        // A table value, not a string: the implementation is
        // `val.cast_to::<Table>()`, and `type::of(type::table('a'))` answers
        // `'table'`. The old `-> string` could report a wrong argument type.
        signature: "type::table(record|string) -> table",
        summary: "Extracts or coerces a table name.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::uuid",
        signature: "type::uuid(any) -> uuid",
        summary: "Converts a value into a UUID.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_array",
        signature: "type::is_array(any) -> bool",
        summary: "Checks whether a value is an array.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_bool",
        signature: "type::is_bool(any) -> bool",
        summary: "Checks whether a value is a boolean.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_bytes",
        signature: "type::is_bytes(any) -> bool",
        summary: "Checks whether a value is bytes.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_collection",
        signature: "type::is_collection(any) -> bool",
        summary: "Checks whether a value is a geometry collection.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_datetime",
        signature: "type::is_datetime(any) -> bool",
        summary: "Checks whether a value is a datetime.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_decimal",
        signature: "type::is_decimal(any) -> bool",
        summary: "Checks whether a value is a decimal.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_duration",
        signature: "type::is_duration(any) -> bool",
        summary: "Checks whether a value is a duration.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_float",
        signature: "type::is_float(any) -> bool",
        summary: "Checks whether a value is a float.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_geometry",
        signature: "type::is_geometry(any) -> bool",
        summary: "Checks whether a value is a geometry value.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_int",
        signature: "type::is_int(any) -> bool",
        summary: "Checks whether a value is an integer.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_line",
        signature: "type::is_line(any) -> bool",
        summary: "Checks whether a value is a geometry line.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_multiline",
        signature: "type::is_multiline(any) -> bool",
        summary: "Checks whether a value is a geometry multiline.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_multipoint",
        signature: "type::is_multipoint(any) -> bool",
        summary: "Checks whether a value is a geometry multipoint.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_multipolygon",
        signature: "type::is_multipolygon(any) -> bool",
        summary: "Checks whether a value is a geometry multipolygon.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_none",
        signature: "type::is_none(any) -> bool",
        summary: "Checks whether a value is NONE.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_null",
        signature: "type::is_null(any) -> bool",
        summary: "Checks whether a value is NULL.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_number",
        signature: "type::is_number(any) -> bool",
        summary: "Checks whether a value is numeric.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_object",
        signature: "type::is_object(any) -> bool",
        summary: "Checks whether a value is an object.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_point",
        signature: "type::is_point(any) -> bool",
        summary: "Checks whether a value is a geometry point.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_polygon",
        signature: "type::is_polygon(any) -> bool",
        summary: "Checks whether a value is a geometry polygon.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_range",
        signature: "type::is_range(any) -> bool",
        summary: "Checks whether a value is a range.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_record",
        signature: "type::is_record(any, table?: string) -> bool",
        summary: "Checks whether a value is a record, optionally on a specific table.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_string",
        signature: "type::is_string(any) -> bool",
        summary: "Checks whether a value is a string.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
    BuiltinFunction {
        name: "type::is_uuid",
        signature: "type::is_uuid(any) -> bool",
        summary: "Checks whether a value is a UUID.",
        documentation_url: "https://surrealdb.com/docs/surrealql/functions/database/type",
    },
];

pub const SPECIAL_VARIABLES: &[(&str, &str)] = &[
    ("$this", "Current record in scope."),
    ("$auth", "Authenticated user/session context."),
    ("$action", "Current action within hooks or permissions."),
    ("$file", "Current file input context."),
    ("$target", "Current target record."),
    ("$value", "Current value being evaluated."),
    ("$parent", "Parent object or record in scope."),
    ("$access", "Access definition context."),
    ("$event", "Current event payload."),
    ("$before", "State before a change."),
    ("$after", "State after a change."),
    ("$request", "Current request metadata."),
    ("$reference", "Reference clause context."),
    ("$token", "Current auth token payload."),
    ("$session", "Current session details."),
    ("$input", "Current input payload."),
];

pub fn builtin_namespace(name: &str) -> Option<&'static BuiltinNamespace> {
    let normalized = name.trim().to_ascii_lowercase();
    BUILTIN_NAMESPACE_DOCS
        .iter()
        .find(|namespace| namespace.name.eq_ignore_ascii_case(&normalized))
}

/// Builtins indexed by their normalized name, together with the return
/// type parsed out of the signature string.
///
/// Built once. The previous linear scan was fine when only hover used it;
/// type checking looks a function up at every call site.
static BUILTIN_INDEX: LazyLock<HashMap<&'static str, (&'static BuiltinFunction, TypeExpr)>> =
    LazyLock::new(|| {
        BUILTIN_FUNCTIONS
            .iter()
            .map(|function| {
                let returns = parse_return_type(function.signature);
                (function.name, (function, returns))
            })
            .collect()
    });

/// Pull the return type out of a signature such as
/// `"string::slice(string, from: number, to?: number) -> string"`.
///
/// Splitting on the *last* `->` matters: a parameter type could contain
/// one in principle, and the return type never can.
fn parse_return_type(signature: &str) -> TypeExpr {
    match signature.rsplit_once("->") {
        Some((_, returns)) => TypeExpr::parse(returns.trim()),
        None => TypeExpr::Unknown,
    }
}

pub fn builtin_function(name: &str) -> Option<&'static BuiltinFunction> {
    let normalized = normalize_builtin_function_name(name);
    BUILTIN_INDEX
        .get(normalized.as_str())
        .map(|(function, _)| *function)
}

/// The type a builtin returns, from the two tables that record one.
///
/// This is the only answer to "what does this call evaluate to", and both the
/// call form and the method form read it. They used to disagree:
/// `math::abs(-1)` was unknown while `(-1).abs()` was a number, because the two
/// paths consulted different tables.
///
/// The curated table is read first. It is narrower — 79 entries against 434 —
/// but a human wrote each one against the documentation, and where it is more
/// specific than the engine it stays more specific: the engine's macros cannot
/// spell `array<string>`, so `string::split` declares `Any` there and
/// `array<string>` here. A curated entry that [`TypeExpr::parse`] cannot model
/// (`field`, `array<field>`) yields `Unknown` or `Other`, which carries no
/// information, so those fall through to the engine rather than shadowing it.
///
/// Both tables can only ever silence a diagnostic when they are vague, never
/// invent one: the assignability rules treat `Unknown`, `Other` and `any` as
/// "no information".
pub fn builtin_return_type(name: &str) -> Option<TypeExpr> {
    let normalized = normalize_builtin_function_name(name);

    if let Some((_, curated)) = BUILTIN_INDEX.get(normalized.as_str())
        && !matches!(curated, TypeExpr::Unknown | TypeExpr::Other(_))
    {
        return Some(curated.clone());
    }

    GENERATED_INDEX
        .get(normalized.as_str())
        .and_then(|signature| signature.declared_return())
        .cloned()
}

fn normalize_builtin_function_name(name: &str) -> String {
    let mut normalized = name.trim().to_ascii_lowercase();
    if normalized.contains("::is::") {
        normalized = normalized.replace("::is::", "::is_");
    }
    // The engine records every rename itself, so resolve through its own table
    // rather than hand-maintaining the special cases. `type::thing` →
    // `type::record` is one of the 62 pairs.
    if let Some((_, current)) = RENAMED_FUNCTIONS
        .iter()
        .find(|(previous, _)| previous.eq_ignore_ascii_case(&normalized))
    {
        normalized = (*current).to_string();
    }
    normalized
}

/// A builtin's argument types, from the generated catalogue.
///
/// Holds the generated entry next to one parsed [`TypeExpr`] per parameter, in
/// the same order. Parsing happens once, in [`GENERATED_INDEX`], because a type
/// checker looks a function up at every call site.
#[derive(Debug)]
pub struct BuiltinSignature {
    pub generated: &'static GeneratedFunction,
    /// One entry per element of `generated.params`, same order.
    pub param_types: Vec<TypeExpr>,
    /// `generated.returns`, parsed. `any` parses to `Scalar("any")`, which the
    /// assignability rules treat as the top type, so callers that want "we know
    /// nothing" must read [`Self::declared_return`] rather than this field.
    pub return_type: TypeExpr,
}

impl BuiltinSignature {
    /// The fewest arguments a caller may supply.
    ///
    /// Only a trailing run of optional and variadic parameters may be omitted.
    pub fn required_arity(&self) -> usize {
        self.generated
            .params
            .iter()
            .rposition(|param| param.form == ParamForm::Required)
            .map_or(0, |index| index + 1)
    }

    /// The most arguments a caller may supply, or `None` when a variadic
    /// parameter makes the count unbounded.
    pub fn maximum_arity(&self) -> Option<usize> {
        if self
            .generated
            .params
            .iter()
            .any(|param| param.form == ParamForm::Variadic)
        {
            return None;
        }
        Some(self.generated.params.len())
    }

    /// One label per parameter, for signature help and inlay hints.
    ///
    /// `from: int`, `to?: int`, `value: any...` — the name and type come from
    /// the engine's own implementation, and the suffix carries the arity.
    pub fn param_labels(&self) -> Vec<String> {
        self.generated
            .params
            .iter()
            .map(|param| match param.form {
                ParamForm::Required => format!("{}: {}", param.name, param.ty),
                ParamForm::Optional => format!("{}?: {}", param.name, param.ty),
                ParamForm::Variadic => format!("{}: {}...", param.name, param.ty),
            })
            .collect()
    }

    /// `math::clamp(arg: number, min: number, max: number) -> number`.
    ///
    /// Rendered from the engine's parameters rather than from prose, so it
    /// covers every function rather than the 79 with a curated signature
    /// string. Returns `None` when the generator could not read the
    /// implementation, because an invented signature is worse than none.
    ///
    /// The return type is appended only when one is known. A reader who sees no
    /// arrow learns that too, which is why nothing is written in its place.
    ///
    /// It comes from [`builtin_return_type`], not from
    /// [`Self::declared_return`], so what a reader sees is what the type checker
    /// uses. The two differ wherever the curated table is more specific than the
    /// engine — `string::split` is `array<string>` there and `any` in the
    /// registry — and a signature that disagreed with the diagnostics would be
    /// worse than no signature.
    pub fn display_signature(&self) -> Option<String> {
        if !self.generated.signature_known {
            return None;
        }
        let rendered = format!(
            "{}({})",
            self.generated.name,
            self.param_labels().join(", ")
        );
        match builtin_return_type(self.generated.name) {
            Some(returns) => Some(format!("{rendered} -> {returns}")),
            None => Some(rendered),
        }
    }

    /// The return type, when the engine declared one worth reading.
    ///
    /// `None` where the catalogue says `any`, which is the engine's way of
    /// saying it cannot express the type — either because its macros take a bare
    /// identifier, or because the type follows an argument. `None` reads as
    /// unknown and stays silent; `Some(Scalar("any"))` would read as the top
    /// type and claim the call can go anywhere.
    pub fn declared_return(&self) -> Option<&TypeExpr> {
        if self.generated.returns == "any" {
            return None;
        }
        Some(&self.return_type)
    }

    /// The declared type of the argument at `index`, following a trailing
    /// variadic for every argument beyond it.
    pub fn param_type_at(&self, index: usize) -> Option<&TypeExpr> {
        if let Some(found) = self.param_types.get(index) {
            return Some(found);
        }
        // A variadic absorbs the rest, so the last parameter types them all.
        match self.generated.params.last() {
            Some(last) if last.form == ParamForm::Variadic => self.param_types.last(),
            _ => None,
        }
    }
}

/// The generated catalogue, indexed by name with parameter types parsed.
static GENERATED_INDEX: LazyLock<HashMap<&'static str, BuiltinSignature>> = LazyLock::new(|| {
    GENERATED_FUNCTIONS
        .iter()
        .map(|generated| {
            let param_types = generated
                .params
                .iter()
                .map(|param| TypeExpr::parse(param.ty))
                .collect();
            (
                generated.name,
                BuiltinSignature {
                    generated,
                    param_types,
                    return_type: TypeExpr::parse(generated.returns),
                },
            )
        })
        .collect()
});

/// The argument types of a builtin function.
///
/// Returns `None` for a name the engine does not have. Callers must also check
/// [`GeneratedFunction::signature_known`] before drawing any conclusion from an
/// empty parameter list: an unread signature and a genuinely zero-argument
/// function look identical without it.
pub fn builtin_signature(name: &str) -> Option<&'static BuiltinSignature> {
    let normalized = normalize_builtin_function_name(name);
    GENERATED_INDEX.get(normalized.as_str())
}

/// True when the engine's parser accepts this name.
///
/// Wider than [`builtin_function`], which only knows the 79 hand-written
/// entries that carry prose.
pub fn is_builtin_function(name: &str) -> bool {
    let normalized = normalize_builtin_function_name(name);
    GENERATED_INDEX.contains_key(normalized.as_str())
}

/// The current spelling of a renamed function, when `name` is an old one.
pub fn renamed_builtin(name: &str) -> Option<&'static str> {
    let normalized = name.trim().to_ascii_lowercase();
    RENAMED_FUNCTIONS
        .iter()
        .find(|(previous, _)| previous.eq_ignore_ascii_case(&normalized))
        .map(|(_, current)| *current)
}

pub fn language() -> Language {
    unsafe { Language::from_raw(tree_sitter_surrealql()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(name: &str) -> &'static BuiltinSignature {
        builtin_signature(name).unwrap_or_else(|| panic!("`{name}` must be in the catalogue"))
    }

    #[test]
    fn a_builtin_from_a_namespace_with_no_curated_prose_still_has_types() {
        // `math::` is one of the 18 namespaces the hand-written table never
        // covered, so this is the gap the generated catalogue closes.
        let clamp = signature("math::clamp");
        assert!(clamp.generated.signature_known);
        assert_eq!(clamp.param_types.len(), 3);
        for parsed in &clamp.param_types {
            assert_eq!(*parsed, TypeExpr::Scalar("number".to_string()));
        }
    }

    #[test]
    fn parameter_types_are_parsed_not_left_as_strings() {
        let sum = signature("math::sum");
        assert_eq!(
            sum.param_types[0],
            TypeExpr::Array(Box::new(TypeExpr::Scalar("number".to_string()))),
            "array<number> must reach the checker as an Array, not an Other"
        );
    }

    #[test]
    fn a_set_parameter_parses_as_a_set() {
        // 28 parameters are typed `set`. Before the string parser learned it,
        // every one degraded to silence.
        let contains = signature("set::contains");
        assert_eq!(contains.param_types[0], TypeExpr::Scalar("set".to_string()));
    }

    #[test]
    fn required_arity_counts_only_the_leading_required_run() {
        // `string::slice(string, from?, to?)`
        let slice = signature("string::slice");
        assert_eq!(slice.required_arity(), 1);
        assert_eq!(slice.maximum_arity(), Some(3));
    }

    #[test]
    fn a_variadic_makes_the_maximum_arity_unbounded() {
        let concat = signature("string::concat");
        assert_eq!(concat.maximum_arity(), None);
        assert_eq!(
            concat.required_arity(),
            0,
            "a self-validating variadic requires nothing"
        );
    }

    #[test]
    fn a_zero_argument_function_has_a_known_signature_and_no_parameters() {
        // The distinction the argument checks depend on.
        let now = signature("time::now");
        assert!(now.generated.signature_known);
        assert!(now.generated.params.is_empty());
        assert_eq!(now.required_arity(), 0);
        assert_eq!(now.maximum_arity(), Some(0));
    }

    #[test]
    fn a_variadic_types_every_argument_beyond_its_position() {
        let concat = signature("array::concat");
        let first = concat.param_type_at(0).cloned();
        assert_eq!(
            concat.param_type_at(7).cloned(),
            first,
            "index 7 is covered"
        );
    }

    #[test]
    fn a_non_variadic_function_types_nothing_past_its_last_parameter() {
        assert!(signature("array::at").param_type_at(5).is_none());
    }

    #[test]
    fn an_old_function_name_resolves_to_its_current_spelling() {
        // The engine's own rename table, rather than a hand-maintained case.
        assert!(builtin_signature("type::thing").is_some());
        assert_eq!(renamed_builtin("type::thing"), Some("type::record"));
        assert_eq!(renamed_builtin("type::record"), None);
    }

    #[test]
    fn the_catalogue_knows_names_the_curated_table_never_had() {
        assert!(is_builtin_function("vector::distance::knn"));
        assert!(is_builtin_function("crypto::argon2::compare"));
        assert!(!is_builtin_function("string::definitely_not_a_function"));
    }

    #[test]
    fn a_cast_parameter_is_permissive() {
        // `string::matches(string, Cast<Regex>)` — the engine casts, so a
        // string literal pattern is legal and must not be flagged.
        let matches = signature("string::matches");
        assert_eq!(
            matches.param_types[1],
            TypeExpr::Scalar("any".to_string()),
            "a Cast parameter must stay permissive"
        );
    }
}
