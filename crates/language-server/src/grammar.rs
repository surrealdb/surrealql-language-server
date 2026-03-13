use tree_sitter::Language;

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

pub const BUILTIN_NAMESPACES: &[&str] = &[
    "array::",
    "crypto::",
    "duration::",
    "encoding::",
    "geo::",
    "http::",
    "math::",
    "meta::",
    "not::",
    "object::",
    "parse::",
    "rand::",
    "record::",
    "search::",
    "session::",
    "sleep::",
    "string::",
    "time::",
    "type::",
    "vector::",
];

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
        signature: "type::table(record|string) -> string",
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

pub fn builtin_function(name: &str) -> Option<&'static BuiltinFunction> {
    let normalized = normalize_builtin_function_name(name);
    BUILTIN_FUNCTIONS
        .iter()
        .find(|function| function.name.eq_ignore_ascii_case(&normalized))
}

fn normalize_builtin_function_name(name: &str) -> String {
    let mut normalized = name.trim().to_ascii_lowercase();
    if normalized.contains("::is::") {
        normalized = normalized.replace("::is::", "::is_");
    }
    if normalized == "type::thing" {
        normalized = "type::record".to_string();
    }
    normalized
}

pub fn language() -> Language {
    unsafe { Language::from_raw(tree_sitter_surrealql()) }
}
