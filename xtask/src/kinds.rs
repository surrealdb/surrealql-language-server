//! Rust parameter type → SurrealQL type name.
//!
//! The engine expresses a builtin's argument types as the destructured tuple
//! type of its `pub fn`, and converts each one through the `HasKind` trait
//! (`surrealdb/core/src/expr/kind.rs:402-527`). This module is the same
//! mapping, read in the other direction.
//!
//! **An unrecognised type maps to `any`, never to a guess.** `any` is the top
//! of the language server's type lattice, so it silences the argument check for
//! that parameter. A wrong mapping would invent a diagnostic against valid
//! SurrealQL, which costs far more than a missing one.

/// How many arguments one parameter accounts for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamForm {
    /// Exactly one argument, and it must satisfy the type.
    Required,
    /// Zero or one argument. From the engine's `Optional<T>`.
    ///
    /// Distinct from the SurrealQL type `option<T>`: `Optional<T>` only lowers
    /// the arity bound (`fnc/args.rs:83-97`). A supplied argument must still
    /// coerce to `T`, so `T` is the type and this is the arity.
    Optional,
    /// Zero or more arguments, each satisfying the type. From `Rest<T>` and
    /// from the self-validating `Any`.
    Variadic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub ty: String,
    pub form: ParamForm,
}

/// The context types the engine injects. A tuple built only from these is not
/// a user-visible signature.
///
/// `Option<&Options>` and `Option<&CursorDoc>` appear here and nowhere else,
/// which is why `Option<T>` is never read as an optional parameter — the
/// engine spells that `Optional<T>`.
const CONTEXT_TYPES: &[&str] = &[
    "Stk",
    "FrozenContext",
    "Options",
    "CursorDoc",
    "QueryExecutor",
    "Context",
];

/// True when this tuple element is engine-injected rather than author-supplied.
pub fn is_context_type(rendered: &str) -> bool {
    CONTEXT_TYPES
        .iter()
        .any(|context| rendered.contains(context))
}

/// The SurrealQL type name for a Rust parameter type, and the arity it implies.
///
/// `wrapper` handling mirrors `fnc/args.rs`:
///
/// | Rust | SurrealQL | Arity |
/// |------|-----------|-------|
/// | `T` | the mapping below | one |
/// | `Optional<T>` | `T` | zero or one |
/// | `Rest<T>` | `T` | zero or more |
/// | `Any` | `any` | zero or more |
/// | `Cast<T>` | `any` | one |
/// | `FromPublic<T>` | `T` | one |
/// | `Box<T>` | `T` | one (transparent) |
///
/// Returns `None` when the arity this type implies cannot be known, which makes
/// the whole signature unknown and therefore silent.
///
/// A wrapper's arity comes from its `FromArg` impl, and an impl may live
/// anywhere and declare anything: `NoneOrRange<T>` in `fnc/rand.rs:36` declares
/// `Arity { lower: 0, upper: Some(2) }` — zero arguments or two, never one.
/// Assuming an unrecognised generic is one required argument reported a wrong
/// count on every `rand::int(0, 100)` in SurrealDB's own corpus. So an
/// unrecognised *generic* gives up, while an unrecognised *bare* name is safe:
/// the blanket `impl<T: Coerce> FromArg for T` (`fnc/args.rs:143`) fixes its
/// arity at one.
pub fn map_type(rendered: &str) -> Option<(String, ParamForm)> {
    let trimmed = rendered.trim();

    if let Some(inner) = unwrap_generic(trimmed, "Optional") {
        let (ty, _) = map_type(inner)?;
        return Some((ty, ParamForm::Optional));
    }
    if let Some(inner) = unwrap_generic(trimmed, "Rest") {
        let (ty, _) = map_type(inner)?;
        return Some((ty, ParamForm::Variadic));
    }
    if let Some(inner) = unwrap_generic(trimmed, "FromPublic") {
        return map_type(inner);
    }
    // `Box<T>` is transparent: a smart pointer, not an argument wrapper.
    if let Some(inner) = unwrap_generic(trimmed, "Box") {
        return map_type(inner);
    }
    // `Cast<T>` runs the looser casting rules rather than coercion, so the
    // engine accepts inputs the declared type does not describe:
    // `string::matches($s, 'a.*')` is legal although the parameter is a
    // regular expression. Typing it `any` keeps the check silent there.
    if unwrap_generic(trimmed, "Cast").is_some() {
        return Some(("any".to_string(), ParamForm::Required));
    }
    // `Any(Vec<Value>)` means the function validates its own arguments, so the
    // arity is unbounded and nothing about the types is known.
    if trimmed == "Any" {
        return Some(("any".to_string(), ParamForm::Variadic));
    }
    if let Some(inner) = unwrap_generic(trimmed, "Vec") {
        let (ty, _) = map_type(inner)?;
        return Some((format!("array<{ty}>"), ParamForm::Required));
    }

    // Any other generic carries an unknown arity.
    if trimmed.contains('<') {
        return None;
    }

    Some((scalar_kind(trimmed).to_string(), ParamForm::Required))
}

/// The `HasKind` impls, transcribed from `expr/kind.rs:402-527`.
fn scalar_kind(name: &str) -> &'static str {
    match name.trim_start_matches('&').trim() {
        "bool" => "bool",
        "i64" | "isize" | "usize" | "u64" | "u32" | "u8" | "i32" => "int",
        "f64" | "f32" => "float",
        "Decimal" => "decimal",
        "String" | "Strand" | "str" => "string",
        "Bytes" => "bytes",
        "Number" => "number",
        "Datetime" => "datetime",
        "Duration" => "duration",
        "Uuid" => "uuid",
        "Range" => "range",
        "Array" => "array",
        "Set" => "set",
        "Object" => "object",
        "RecordId" | "Thing" => "record",
        // Deliberately permissive. A geometry reaches a call in three shapes the
        // language server types differently: `geometry`, a `point` tuple
        // (`(-0.12, 51.5)`, which `Kind` treats as sugar for
        // `geometry<point>`), and an object literal
        // `{ type: 'Point', coordinates: [...] }` that the parser turns into a
        // geometry (`sql/literal.rs:286`). The lattice sees the last two as
        // `point` and `Object`, so a `geometry` parameter reported all three —
        // six hits in SurrealDB's own `geo::` and casting tests. Widening the
        // assignability rules cannot reach the object-literal case, so the
        // honest fix is here.
        "Geometry" => "any",
        "Closure" => "function",
        "Regex" => "regex",
        "File" => "file",
        "TableName" | "Table" => "table",
        // `Value` is the commonest parameter type in the engine and has no
        // `HasKind` impl, so it needs this explicit rule rather than a lookup.
        "Value" => "any",
        // Anything this mapper has not been taught. Silence, not a guess.
        _ => "any",
    }
}

/// `Name<inner>` → `inner`, respecting nesting depth.
fn unwrap_generic<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let rest = input.strip_prefix(name)?;
    let rest = rest.trim_start();
    let inner = rest.strip_prefix('<')?.strip_suffix('>')?;
    Some(inner.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_type_is_one_required_argument() {
        assert_eq!(
            map_type("String"),
            Some(("string".into(), ParamForm::Required))
        );
        assert_eq!(map_type("i64"), Some(("int".into(), ParamForm::Required)));
        assert_eq!(
            map_type("Number"),
            Some(("number".into(), ParamForm::Required))
        );
    }

    #[test]
    fn optional_lowers_the_arity_but_keeps_the_type() {
        // Not `option<int>`: a supplied argument must still be an int.
        assert_eq!(
            map_type("Optional<i64>"),
            Some(("int".into(), ParamForm::Optional))
        );
    }

    #[test]
    fn rest_is_a_typed_variadic() {
        assert_eq!(
            map_type("Rest<Array>"),
            Some(("array".into(), ParamForm::Variadic))
        );
    }

    #[test]
    fn any_is_an_untyped_variadic() {
        assert_eq!(map_type("Any"), Some(("any".into(), ParamForm::Variadic)));
    }

    #[test]
    fn cast_is_permissive_because_the_engine_casts() {
        assert_eq!(
            map_type("Cast<Regex>"),
            Some(("any".into(), ParamForm::Required)),
            "string::matches($s, 'a.*') is legal, so this must not be typed regex"
        );
    }

    #[test]
    fn vec_becomes_an_array_of_the_element_type() {
        assert_eq!(
            map_type("Vec<Number>"),
            Some(("array<number>".into(), ParamForm::Required))
        );
        assert_eq!(
            map_type("Vec<String>"),
            Some(("array<string>".into(), ParamForm::Required))
        );
    }

    #[test]
    fn value_maps_to_any() {
        assert_eq!(map_type("Value"), Some(("any".into(), ParamForm::Required)));
    }

    #[test]
    fn an_unknown_bare_type_degrades_to_any_with_arity_one() {
        // The blanket `impl<T: Coerce> FromArg for T` fixes a bare type's arity
        // at one, so only the SurrealQL type is in doubt.
        assert_eq!(
            map_type("SomeTypeAddedNextRelease"),
            Some(("any".into(), ParamForm::Required))
        );
    }

    #[test]
    fn an_unknown_generic_gives_up_because_its_arity_is_unknown() {
        // `NoneOrRange<i64>` (`fnc/rand.rs:36`) declares zero-or-two. Guessing
        // one required argument reported a wrong count on every
        // `rand::int(0, 100)` in SurrealDB's own corpus.
        assert_eq!(map_type("NoneOrRange<i64>"), None);
        assert_eq!(map_type("SomeWrapper<String>"), None);
    }

    #[test]
    fn box_is_transparent_not_a_wrapper() {
        assert_eq!(
            map_type("Box<Closure>"),
            Some(("function".into(), ParamForm::Required))
        );
    }

    #[test]
    fn context_types_are_recognised() {
        assert!(is_context_type("&mut Stk"));
        assert!(is_context_type("&FrozenContext"));
        assert!(is_context_type("Option<&Options>"));
        assert!(is_context_type("Option<&CursorDoc>"));
        assert!(!is_context_type("String"));
        assert!(!is_context_type("Optional<i64>"));
    }
}
