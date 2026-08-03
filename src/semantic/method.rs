//! Which function a `value.method()` call resolves to.
//!
//! SurrealQL lets most builtins be called as a method, and the mapping is **not**
//! `<receiver type>::<method>`. The server used to guess that convention, which
//! is right for the eight receivers whose namespace happens to be their type name
//! and wrong for everything else: `(5).round()` is `math::round`,
//! `123.to_float()` is `type::float`, `"abc".is_alphanum()` is
//! `string::is_alphanum`, `$point.area()` is `geo::area`.
//!
//! The real tables are generated from the engine's own `fnc::idiom` into
//! [`GENERATED_RECEIVERS`] — 12 tables, 820 arms. This module is the lookup over
//! them.
//!
//! # The gate
//!
//! [`receiver_kind`] answers `None` for any type that is not certainly one engine
//! `Value` variant, and a `None` means the caller reports nothing. That matters
//! more here than it looks: the `String` table and the catch-all table disagree
//! about arity for four method names, so picking the *wrong* table is worse than
//! picking none.

use crate::grammar::{GENERATED_RECEIVERS, GeneratedMethod, builtin_return_type};
use crate::semantic::type_expr::TypeExpr;

/// The engine `Value` variant a type certainly denotes.
///
/// The `Some("")` case is the engine's catch-all arm, which really does serve
/// `bool`, `uuid`, `regex`, `range`, `none` and `null` with 48 methods. Returning
/// `None` for them instead would report `true.to_string()` as an unknown method.
pub fn receiver_kind(ty: &TypeExpr) -> Option<&'static str> {
    match ty {
        TypeExpr::Array(_) | TypeExpr::Tuple(_) => Some("Array"),
        TypeExpr::Set(_) => Some("Set"),
        TypeExpr::Object(_) => Some("Object"),
        TypeExpr::Record(_) => Some("RecordId"),

        TypeExpr::Scalar(name) => match name.to_ascii_lowercase().as_str() {
            // One table serves all three numeric kinds, because the engine
            // matches `Value::Number` without looking inside it.
            "int" | "float" | "decimal" | "number" => Some("Number"),
            "string" => Some("String"),
            "datetime" => Some("Datetime"),
            "duration" => Some("Duration"),
            "bytes" => Some("Bytes"),
            "file" => Some("File"),
            "object" => Some("Object"),
            "array" => Some("Array"),
            "set" => Some("Set"),
            "record" => Some("RecordId"),
            // One table serves all seven geometry shapes, for the same reason.
            "geometry" | "point" => Some("Geometry"),
            // The catch-all arm. These are real receivers with real methods.
            "bool" | "uuid" | "regex" | "range" | "table" | "function" | "none" | "null" => {
                Some("")
            }
            // `any`, `value`, and every name this has not been taught.
            _ => None,
        },

        // Absence of information, or a shape that could be several kinds at run
        // time. An `option<string>` may hold a string *or* NONE, and those two
        // read different tables.
        TypeExpr::Unknown | TypeExpr::Other(_) | TypeExpr::Option(_) | TypeExpr::Union(_) => None,

        // A literal behaves as its family does.
        TypeExpr::Literal(_) => crate::semantic::assign::widen(ty)
            .as_ref()
            .and_then(receiver_kind),
    }
}

/// Every table that could apply to this receiver.
///
/// Usually one. The exception is an object literal shaped like GeoJSON:
/// `{ type: "Point", coordinates: [0, 0] }` is a `Value::Geometry` to the engine
/// (`sql/literal.rs`), but the type lattice can only see an object, and the two
/// tables hold disjoint methods. `xtask/src/kinds.rs` records the same three-way
/// ambiguity for `geometry` *parameters*, and resolves it the same way: admit
/// both rather than guess one.
///
/// `Object` and `Geometry` share no method name outside the block every receiver
/// shares, so trying both cannot pick a wrong arity.
fn candidate_kinds(ty: &TypeExpr) -> Vec<&'static str> {
    let Some(kind) = receiver_kind(ty) else {
        return Vec::new();
    };
    if kind == "Object" && looks_like_geojson(ty) {
        return vec!["Object", "Geometry"];
    }
    vec![kind]
}

/// True for an object type carrying both GeoJSON discriminator keys.
fn looks_like_geojson(ty: &TypeExpr) -> bool {
    let TypeExpr::Object(fields) = ty else {
        return false;
    };
    let has = |wanted: &str| {
        fields
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(wanted))
    };
    has("type") && has("coordinates")
}

/// The methods a receiver kind accepts.
pub fn methods_for(kind: &str) -> &'static [GeneratedMethod] {
    GENERATED_RECEIVERS
        .iter()
        .find(|receiver| receiver.kind == kind)
        .map(|receiver| receiver.methods)
        .unwrap_or(&[])
}

/// The method `name` on a receiver of type `ty`.
///
/// `None` means either "we cannot tell what this receiver is" or "this receiver
/// has no such method". Callers that report an unknown method must distinguish
/// the two with [`receiver_kind`].
pub fn resolve(ty: &TypeExpr, name: &str) -> Option<&'static GeneratedMethod> {
    candidate_kinds(ty).into_iter().find_map(|kind| {
        methods_for(kind)
            .iter()
            .find(|method| method.method == name)
    })
}

/// A friendly name for a receiver kind, for a diagnostic message.
pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "RecordId" => "record",
        "Datetime" => "datetime",
        "Number" => "number",
        "String" => "string",
        "Duration" => "duration",
        "Bytes" => "bytes",
        "Geometry" => "geometry",
        "Object" => "object",
        "Array" => "array",
        "Set" => "set",
        "File" => "file",
        _ => "this value",
    }
}

/// The type a resolved function returns, when that is knowable.
///
/// One line, because there is one answer. This used to hold three hand-written
/// tables — the `type::` conversions, the `math::` functions and the
/// `duration::`/`time::` accessors — because the generated catalogue carried no
/// return types and every builtin is `-> Result<Value>` in Rust. The engine's
/// own registry declares them (`exec/function/builtin/`), the generator now
/// reads it, and all three tables became a second opinion on data the engine
/// states itself. A second opinion can only drift.
///
/// `None` reads as `unknown` and is silent. See [`builtin_return_type`] for
/// which of the two tables answers.
pub fn return_type(function: &str) -> Option<TypeExpr> {
    builtin_return_type(function)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(name: &str) -> TypeExpr {
        TypeExpr::Scalar(name.to_string())
    }

    #[test]
    fn the_three_remapped_receivers_resolve() {
        // The whole point of the generated tables. None of these follows the
        // `<receiver>::<method>` convention the server used to guess.
        for (ty, method, expected) in [
            (scalar("int"), "round", "math::round"),
            (scalar("float"), "abs", "math::abs"),
            (scalar("datetime"), "year", "time::year"),
            (scalar("geometry"), "area", "geo::area"),
            (scalar("point"), "centroid", "geo::centroid"),
        ] {
            let resolved = resolve(&ty, method).unwrap_or_else(|| panic!("{ty}.{method}()"));
            assert_eq!(resolved.function, expected, "{ty}.{method}()");
        }
    }

    #[test]
    fn the_identity_receivers_still_resolve() {
        for (ty, method, expected) in [
            (scalar("string"), "len", "string::len"),
            (scalar("duration"), "days", "duration::days"),
            (scalar("bytes"), "len", "bytes::len"),
            (
                TypeExpr::Array(Box::new(scalar("int"))),
                "len",
                "array::len",
            ),
            (TypeExpr::Set(Box::new(scalar("int"))), "len", "set::len"),
            (TypeExpr::Object(Vec::new()), "values", "object::values"),
            (
                TypeExpr::Record(vec!["person".to_string()]),
                "id",
                "record::id",
            ),
        ] {
            let resolved = resolve(&ty, method).unwrap_or_else(|| panic!("{ty}.{method}()"));
            assert_eq!(resolved.function, expected, "{ty}.{method}()");
        }
    }

    #[test]
    fn the_shared_block_reaches_every_receiver() {
        // `to_*`, `is_*`, `chain` and friends are on all 12 tables, and none of
        // them follows the receiver's own namespace.
        for ty in [
            scalar("string"),
            scalar("int"),
            scalar("bool"),
            scalar("uuid"),
            TypeExpr::Array(Box::new(scalar("int"))),
        ] {
            assert_eq!(
                resolve(&ty, "to_string").map(|found| found.function),
                Some("type::string"),
                "{ty}.to_string()"
            );
            assert_eq!(
                resolve(&ty, "is_number").map(|found| found.function),
                Some("type::is_number"),
                "{ty}.is_number()"
            );
        }
    }

    #[test]
    fn the_string_table_shadows_the_shared_block() {
        // `String` overrides four common arms and drops one. Getting this wrong
        // means the wrong arity: `string::is_datetime` takes an optional format
        // argument and `type::is_datetime` does not.
        let string = scalar("string");
        assert_eq!(
            resolve(&string, "repeat").map(|found| found.function),
            Some("string::repeat")
        );
        assert_eq!(
            resolve(&string, "is_datetime").map(|found| found.function),
            Some("string::is_datetime")
        );
        // Absent from `String`, present everywhere else.
        assert!(resolve(&string, "is_set").is_none());
        assert!(resolve(&scalar("int"), "is_set").is_some());
    }

    #[test]
    fn the_catch_all_serves_the_untabled_receivers() {
        // Without this table `true.to_string()` would report an unknown method.
        for name in ["bool", "uuid", "regex", "range", "none", "null"] {
            assert_eq!(receiver_kind(&scalar(name)), Some(""), "{name}");
            assert!(resolve(&scalar(name), "to_string").is_some(), "{name}");
        }
    }

    #[test]
    fn a_geojson_object_literal_reaches_both_tables() {
        // The engine turns `{ type: "Point", coordinates: […] }` into a
        // `Value::Geometry`, but the lattice can only see an object. Six calls in
        // SurrealDB's own `method_syntax.surql` are written exactly this way.
        let geojson = TypeExpr::Object(vec![
            ("type".to_string(), scalar("string")),
            (
                "coordinates".to_string(),
                TypeExpr::Array(Box::new(scalar("int"))),
            ),
        ]);
        assert_eq!(
            resolve(&geojson, "area").map(|found| found.function),
            Some("geo::area")
        );
        // And a genuine object method still resolves on the same value.
        assert_eq!(
            resolve(&geojson, "values").map(|found| found.function),
            Some("object::values")
        );
        // A plain object gets the object table only.
        let plain = TypeExpr::Object(vec![("a".to_string(), scalar("int"))]);
        assert!(resolve(&plain, "area").is_none());
        assert!(resolve(&plain, "values").is_some());
    }

    #[test]
    fn the_gate_refuses_what_it_cannot_prove() {
        // A `Some` here is a false positive waiting to happen: the `String` table
        // and the catch-all disagree on arity, so a wrong guess is worse than
        // none.
        for ty in [
            TypeExpr::Unknown,
            TypeExpr::Other("weird".to_string()),
            TypeExpr::Option(Box::new(scalar("string"))),
            TypeExpr::Union(vec![scalar("int"), scalar("string")]),
            scalar("any"),
            scalar("value"),
            scalar("quaternion"),
        ] {
            assert_eq!(receiver_kind(&ty), None, "{ty}");
            assert!(resolve(&ty, "len").is_none(), "{ty}");
        }
    }

    #[test]
    fn a_literal_receiver_behaves_as_its_family() {
        assert_eq!(
            resolve(&TypeExpr::Literal("'x'".to_string()), "len").map(|found| found.function),
            Some("string::len")
        );
    }

    #[test]
    fn an_alias_names_the_implementation() {
        // `every` and `all` are one function. Both must resolve, and to the same
        // place, because the parameters are the same by construction.
        let array = TypeExpr::Array(Box::new(scalar("int")));
        assert_eq!(
            resolve(&array, "every").map(|found| found.function),
            Some("array::all")
        );
        assert_eq!(
            resolve(&array, "all").map(|found| found.function),
            Some("array::all")
        );
    }

    #[test]
    fn a_file_method_is_marked_experimental() {
        let file = resolve(&scalar("file"), "get").expect("file::get");
        assert_eq!(file.experimental, Some("Files"));
        // And nothing else is.
        let string = resolve(&scalar("string"), "len").expect("string::len");
        assert_eq!(string.experimental, None);
    }

    #[test]
    fn the_return_types_that_were_hand_written_are_still_right() {
        // These ten were three const tables in this module until the generator
        // learned to read the engine's registry. The engine declares every one
        // of them, so the answers must not have moved.
        for (function, expected) in [
            ("type::is_record", "bool"),
            ("type::is_number", "bool"),
            ("type::float", "float"),
            ("type::int", "int"),
            ("type::string_lossy", "string"),
            ("math::round", "number"),
            ("math::abs", "number"),
            ("duration::days", "int"),
            ("time::year", "int"),
            ("time::is_leap_year", "bool"),
        ] {
            assert_eq!(
                return_type(function),
                Some(TypeExpr::Scalar(expected.to_string())),
                "{function}"
            );
        }
    }

    #[test]
    fn a_namespace_the_hand_written_tables_never_had_now_answers() {
        // The gap those tables left. Each of these typed as unknown before the
        // engine's registry was read, in call form and in method form alike.
        for (function, expected) in [
            ("geo::area", "float"),
            ("vector::dot", "float"),
            ("crypto::sha256", "string"),
            ("rand::uuid::v4", "uuid"),
            ("time::now", "datetime"),
            ("array::len", "int"),
        ] {
            assert_eq!(
                return_type(function),
                Some(TypeExpr::Scalar(expected.to_string())),
                "{function}"
            );
        }
    }

    #[test]
    fn a_return_type_that_follows_an_argument_stays_unknown() {
        // Honest about the limit, and the limit is now the right one: these
        // return whatever they were given, so no single type is correct and a
        // guess would report against valid SurrealQL. The engine says `Any` for
        // each, and the overlay deliberately leaves them alone.
        for function in [
            "array::group",
            "array::first",
            "object::values",
            "object::entries",
            "array::at",
        ] {
            assert_eq!(return_type(function), None, "{function}");
        }
    }
}
