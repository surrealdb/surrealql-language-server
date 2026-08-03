//! Return types, read from the engine's function registry.
//!
//! The implementations under `fnc/` carry no return type: every one of them is
//! `-> Result<Value>`, so [`crate::signatures`] can read arguments and nothing
//! else. The registry under `exec/function/builtin/` is the second table, and it
//! does declare one:
//!
//! ```ignore
//! define_pure_function!(RandUuidV4, "rand::uuid::v4", () -> Uuid, crate::fnc::rand::uuid::v4);
//! ```
//!
//! Six macros declare functions this way, and about thirty functions spell the
//! same thing out by hand as an `impl ScalarFunction`. Both forms reduce to one
//! rule: a string literal names the function, and the identifier after the last
//! arrow is its return kind. That rule is why this reads token streams rather
//! than matching each macro's own grammar — a seventh macro needs no new arm.
//!
//! # What the engine cannot say
//!
//! The macros take the return kind as a bare identifier (`$ret:ident`), so no
//! `Kind` variant that carries a payload can be written. `array::distinct`
//! returns an array and declares `Any`, because `Kind::Array(..)` does not fit
//! through the macro. The engine records the reason itself at
//! `exec/function/builtin/array.rs:3`. Those arrive here as `any`, which is
//! silence, and [`crate::emit::OVERLAY`] fills in the ones a human can be sure
//! of.
//!
//! # Confidence
//!
//! The engine never calls this table. `ScalarFunction::signature` carries
//! `#[allow(unused)]`, and no engine test reads it, so nothing in SurrealDB
//! proves these values right. Two things here do. The generated values agree
//! with all 79 entries of the curated table that this crate wrote by hand, and
//! `cargo run -p xtask --features probe -- verify-returns` runs each function in
//! a memory engine and compares the answer.

use std::collections::BTreeMap;
use std::path::Path;

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use syn::{ImplItem, Item};

/// Every declared return type, keyed by the name an author writes.
///
/// The value is a SurrealQL type name, ready for `TypeExpr::parse`. `any` means
/// the engine declared nothing usable, which silences the check.
pub fn collect(builtin_dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut found = BTreeMap::new();
    collect_dir(builtin_dir, &mut found)?;
    Ok(found)
}

fn collect_dir(dir: &Path, found: &mut BTreeMap<String, String>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    for entry in entries {
        let path = entry
            .map_err(|error| format!("cannot read a directory entry: {error}"))?
            .path();

        // `aggregates/` holds `count`, `math::sum` and the other reducers, one
        // file per namespace.
        if path.is_dir() {
            collect_dir(&path, found)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }

        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let file = syn::parse_file(&source)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        walk(&file.items, found);
    }
    Ok(())
}

fn walk(items: &[Item], out: &mut BTreeMap<String, String>) {
    for item in items {
        match item {
            // `macro_rules! define_array_closure_function { .. }` is a
            // definition, and its body holds `$func_name` and `Kind::$ret`
            // rather than a name and a kind. Only an invocation carries values,
            // and an invocation has no name of its own.
            Item::Macro(item_macro) if item_macro.ident.is_none() => {
                if let Some((name, kind)) = read_invocation(&item_macro.mac.tokens) {
                    out.insert(name, kind);
                }
            }
            Item::Impl(item_impl) => {
                if let Some((name, kind)) = read_impl(item_impl) {
                    out.insert(name, kind);
                }
            }
            Item::Mod(item_mod) => {
                if let Some((_, nested)) = &item_mod.content {
                    walk(nested, out);
                }
            }
            _ => {}
        }
    }
}

/// A macro invocation → the function it declares.
///
/// The first string literal is the name. The return kind is the identifier
/// after the last arrow, which is `->` in the five `define_*_function!` macros
/// and `=>` in the two closure helpers (`exec/function/builtin/array.rs:79`).
/// Taking the *last* arrow is what keeps `(array: Any, check: Any => Any)` from
/// answering with an argument type.
///
/// Returns `None` for an invocation that declares no function, which is how
/// `register_functions!(registry, Rand, RandBool, ..)` drops out.
fn read_invocation(tokens: &TokenStream) -> Option<(String, String)> {
    let mut name: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut after_arrow = false;

    let mut previous: Option<char> = None;
    for token in tokens.clone() {
        match token {
            TokenTree::Literal(literal) => {
                if name.is_none() {
                    name = string_value(&literal.to_string());
                }
                previous = None;
            }
            TokenTree::Punct(punct) => {
                if punct.as_char() == '>' && matches!(previous, Some('-') | Some('=')) {
                    after_arrow = true;
                    // A later arrow replaces this answer, so clear it now.
                    kind = None;
                }
                previous = Some(punct.as_char());
            }
            TokenTree::Ident(ident) => {
                if after_arrow && kind.is_none() {
                    kind = Some(ident.to_string());
                    after_arrow = false;
                }
                previous = None;
            }
            TokenTree::Group(_) => previous = None,
        }
    }

    Some((name?, type_name(&kind?)))
}

/// A hand-written `impl ScalarFunction` → the function it declares.
///
/// `fn name` holds the only string literal in the block, and `fn signature`
/// holds a `Signature::new()` chain that ends in `.returns(..)`. This form is
/// the only one that can spell a payload-carrying kind, which is why
/// `search::analyze` reaches `array` and `array::distinct` does not.
fn read_impl(item_impl: &syn::ItemImpl) -> Option<(String, String)> {
    let (_, path, _) = item_impl.trait_.as_ref()?;
    let trait_name = path.segments.last()?.ident.to_string();
    if !matches!(trait_name.as_str(), "ScalarFunction" | "AggregateFunction") {
        return None;
    }

    let mut name = None;
    let mut kind = None;
    for item in &item_impl.items {
        let ImplItem::Fn(function) = item else {
            continue;
        };
        match function.sig.ident.to_string().as_str() {
            "name" => {
                name = first_string(&function.block.to_token_stream());
            }
            "signature" => {
                kind = returns_argument(&function.block.to_token_stream());
            }
            _ => {}
        }
    }

    Some((name?, type_name(&kind?)))
}

/// The first string literal in a token stream, looking inside groups.
fn first_string(tokens: &TokenStream) -> Option<String> {
    for token in tokens.clone() {
        match token {
            TokenTree::Literal(literal) => {
                if let Some(text) = string_value(&literal.to_string()) {
                    return Some(text);
                }
            }
            TokenTree::Group(group) => {
                if let Some(found) = first_string(&group.stream()) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

/// The argument of the last `.returns(..)` call, rendered as text.
///
/// A method chain is flat in token form — `Signature::new().arg(..).returns(..)`
/// is one level — so this looks for `returns` followed by a parenthesised group.
/// It still recurses, because the chain sits inside the function's block.
fn returns_argument(tokens: &TokenStream) -> Option<String> {
    let mut found = None;
    let mut previous_was_returns = false;
    for token in tokens.clone() {
        match token {
            TokenTree::Ident(ident) => {
                previous_was_returns = ident == "returns";
            }
            TokenTree::Group(group) => {
                if previous_was_returns && group.delimiter() == Delimiter::Parenthesis {
                    found = Some(tidy(&group.stream().to_string()));
                } else if let Some(nested) = returns_argument(&group.stream()) {
                    found = Some(nested);
                }
                previous_was_returns = false;
            }
            _ => previous_was_returns = false,
        }
    }
    found
}

/// `"rand::uuid::v4"` → `rand::uuid::v4`.
fn string_value(literal: &str) -> Option<String> {
    let trimmed = literal.trim();
    let inner = trimmed.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.to_string())
}

/// A `Kind` expression → a SurrealQL type name.
///
/// Accepts both forms the engine writes: the bare `Uuid` a macro passes as
/// `$ret`, and the full `Kind::Array(Box::new(Kind::Any), None)` a hand-written
/// signature spells out. Transcribed from `sql/kind.rs:356`, which is the
/// engine's own renderer for these values.
///
/// A variant this mapper has not been taught answers `any`. Silence, not a
/// guess — the same rule [`crate::kinds::scalar_kind`] follows for arguments.
fn type_name(expression: &str) -> String {
    let rest = strip_kind_path(&tidy(expression));
    let variant = leading_ident(&rest);
    let payload = payload_of(&rest);

    match variant.as_str() {
        "Bool" => "bool".to_string(),
        "Bytes" => "bytes".to_string(),
        "Datetime" => "datetime".to_string(),
        "Decimal" => "decimal".to_string(),
        "Duration" => "duration".to_string(),
        "Float" => "float".to_string(),
        "Int" => "int".to_string(),
        "Number" => "number".to_string(),
        "Object" => "object".to_string(),
        "String" => "string".to_string(),
        "Uuid" => "uuid".to_string(),
        "Regex" => "regex".to_string(),
        "Range" => "range".to_string(),
        "None" => "none".to_string(),
        "Null" => "null".to_string(),

        // `array<any>` is written `array`, exactly as the engine renders it at
        // `sql/kind.rs:413`. The element type is the first payload argument.
        "Array" => collection("array", &payload),
        "Set" => collection("set", &payload),

        // The payload lists the tables or buckets a value may belong to, and
        // every declaration in the registry leaves it empty. Reading it would
        // add a precision no declaration carries.
        "Record" => "record".to_string(),
        "Table" => "table".to_string(),
        "Geometry" => "geometry".to_string(),
        "File" => "file".to_string(),
        "Function" => "function".to_string(),

        // `Either` is a union and `Literal` a single value. The lattice can hold
        // both, but no declaration in the registry uses either, so a mapping
        // here would be untested code. `Any` is the engine saying it cannot
        // express the type.
        _ => "any".to_string(),
    }
}

/// `array` when the element type is unknown, `array<string>` when it is not.
fn collection(base: &str, payload: &str) -> String {
    let element = first_argument(payload);
    if element.is_empty() {
        return base.to_string();
    }
    match type_name(&element).as_str() {
        "any" => base.to_string(),
        known => format!("{base}<{known}>"),
    }
}

/// Drop everything up to and including the first `Kind::`.
///
/// `$crate::expr::Kind::Int` and `Kind::Int` both become `Int`. A bare `Uuid`
/// has no prefix and survives unchanged.
fn strip_kind_path(expression: &str) -> String {
    match expression.split_once("Kind::") {
        Some((_, rest)) => rest.to_string(),
        None => expression.to_string(),
    }
}

fn leading_ident(expression: &str) -> String {
    expression
        .chars()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
        .collect()
}

/// The text inside the outermost parentheses, or empty when there are none.
fn payload_of(expression: &str) -> String {
    let Some(open) = expression.find('(') else {
        return String::new();
    };
    let mut depth = 0usize;
    for (index, ch) in expression.char_indices().skip(open) {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return expression[open + 1..index].to_string();
                }
            }
            _ => {}
        }
    }
    String::new()
}

/// The first comma-separated argument, ignoring commas inside brackets.
///
/// `Box::new(Kind::String), None` → `Box::new(Kind::String)`.
fn first_argument(payload: &str) -> String {
    let mut depth = 0usize;
    for (index, ch) in payload.char_indices() {
        match ch {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return payload[..index].trim().to_string(),
            _ => {}
        }
    }
    payload.trim().to_string()
}

/// `Kind :: Array (Box :: new (Kind :: Any) , None)` → the same without the
/// spacing `proc-macro2` adds.
fn tidy(rendered: &str) -> String {
    rendered
        .replace(" :: ", "::")
        .replace(":: ", "::")
        .replace(" ::", "::")
        .replace(" (", "(")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace(" ,", ",")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> BTreeMap<String, String> {
        let file = syn::parse_file(source).expect("test source must parse");
        let mut out = BTreeMap::new();
        walk(&file.items, &mut out);
        out
    }

    #[test]
    fn a_zero_argument_declaration_is_read() {
        let found = parse(
            "define_pure_function!(RandUuidV4, \"rand::uuid::v4\", () -> Uuid, crate::fnc::rand::uuid::v4);",
        );
        assert_eq!(found["rand::uuid::v4"], "uuid");
    }

    #[test]
    fn the_argument_types_never_answer_for_the_return_type() {
        // Every argument here is a `Kind` name too, so a first-match rule would
        // report `Number`.
        let found = parse(
            "define_pure_function!(MathClamp, \"math::clamp\", (value: Number, min: Number, max: Number) -> Int, crate::fnc::math::clamp);",
        );
        assert_eq!(found["math::clamp"], "int");
    }

    #[test]
    fn an_optional_argument_list_does_not_disturb_the_arrow() {
        let found = parse(
            "define_pure_function!(RandTime, \"rand::time\", (?min: Datetime, ?max: Datetime) -> Datetime, crate::fnc::rand::time);",
        );
        assert_eq!(found["rand::time"], "datetime");
    }

    #[test]
    fn a_closure_helper_uses_a_fat_arrow() {
        // `define_array_closure_function!` separates its return with `=>`, and
        // its arguments are bare rather than parenthesised.
        let found = parse(
            "define_array_closure_function!(ArrayMap, \"array::map\", crate::fnc::array::map, array: Any, mapper: Any => Bool);",
        );
        assert_eq!(found["array::map"], "bool");
    }

    #[test]
    fn a_macro_definition_declares_nothing() {
        // The body holds `$func_name` and `Kind::$ret`, not a name and a kind.
        let found = parse(
            "macro_rules! define_thing {
                ($struct_name:ident, $func_name:literal, () -> $ret:ident) => {
                    impl ScalarFunction for $struct_name {
                        fn name(&self) -> &'static str { $func_name }
                        fn signature(&self) -> Signature {
                            Signature::new().returns(Kind::$ret)
                        }
                    }
                };
            }",
        );
        assert!(found.is_empty(), "got {:?}", found);
    }

    #[test]
    fn a_registration_call_declares_nothing() {
        let found = parse("register_functions!(registry, Rand, RandBool, RandUuidV4,);");
        assert!(found.is_empty(), "got {:?}", found);
    }

    #[test]
    fn a_hand_written_signature_is_read() {
        let found = parse(
            "impl ScalarFunction for SequenceNextval {
                fn name(&self) -> &'static str { \"sequence::nextval\" }
                fn signature(&self) -> Signature {
                    Signature::new().arg(\"sequence\", Kind::String).returns(Kind::Int)
                }
            }",
        );
        assert_eq!(found["sequence::nextval"], "int");
    }

    #[test]
    fn a_hand_written_signature_can_spell_a_collection() {
        // The form that reaches a kind the macros cannot express.
        let found = parse(
            "impl ScalarFunction for SearchAnalyze {
                fn name(&self) -> &'static str { \"search::analyze\" }
                fn signature(&self) -> Signature {
                    Signature::new().returns(Kind::Array(Box::new(Kind::Any), None))
                }
            }",
        );
        assert_eq!(
            found["search::analyze"], "array",
            "`array<any>` is written `array`"
        );
    }

    #[test]
    fn an_aggregate_declares_its_return_type() {
        let found = parse(
            "impl AggregateFunction for CountAll {
                fn name(&self) -> &'static str { \"count\" }
                fn signature(&self) -> Signature { Signature::new().returns(Kind::Int) }
            }",
        );
        assert_eq!(found["count"], "int");
    }

    #[test]
    fn an_unrelated_impl_is_skipped() {
        let found = parse(
            "impl Default for Thing {
                fn name(&self) -> &'static str { \"not::a::function\" }
            }",
        );
        assert!(found.is_empty());
    }

    #[test]
    fn a_typed_element_survives_into_the_type_name() {
        assert_eq!(
            type_name("Kind::Array(Box::new(Kind::String), None)"),
            "array<string>"
        );
        assert_eq!(
            type_name("Kind::Set(Box::new(Kind::Number), None)"),
            "set<number>"
        );
    }

    #[test]
    fn a_kind_the_macro_cannot_express_becomes_any() {
        // `Any` is the engine saying "this does not fit through `$ret:ident`".
        assert_eq!(type_name("Any"), "any");
        assert_eq!(type_name("Kind::Any"), "any");
        assert_eq!(
            type_name("Kind::Either(vec![Kind::None, Kind::Int])"),
            "any"
        );
        assert_eq!(type_name("Kind::Literal(KindLiteral::Bool(true))"), "any");
    }

    #[test]
    fn a_payload_carrying_kind_keeps_its_base_name() {
        assert_eq!(type_name("Kind::Record(Vec::new())"), "record");
        assert_eq!(type_name("Kind::Geometry(Vec::new())"), "geometry");
        assert_eq!(type_name("Kind::File(Vec::new())"), "file");
    }

    #[test]
    fn a_fully_qualified_path_resolves_to_the_variant() {
        assert_eq!(type_name("$crate::expr::Kind::Datetime"), "datetime");
    }

    #[test]
    fn an_unknown_variant_degrades_to_any() {
        assert_eq!(type_name("Kind::SomethingNewInSurrealDb"), "any");
    }
}
