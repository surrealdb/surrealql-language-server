//! Is a value of type `actual` acceptable where `expected` is declared?
//!
//! This relation exists to drive an ERROR-severity diagnostic, which sets
//! the bar: **a wrong answer is far more costly than no answer.** A noisy
//! type checker gets switched off, and then it protects nobody.
//!
//! So the relation is three-valued, not two. [`Verdict::Incompatible`] is
//! reserved for cases where SurrealDB genuinely cannot accept the value —
//! passing a string literal to a `record<user>` parameter. Everything
//! else, including everything we merely failed to work out, is
//! [`Verdict::Unknown`], and callers must stay silent on it.
//!
//! Where SurrealDB's own coercion rules are subtle (string → datetime,
//! `option<T>` → `T`), the rules below deliberately answer `Unknown`
//! rather than guess. Being right about those is worth less than never
//! being wrong about them.

use crate::semantic::type_expr::TypeExpr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The value fits, or SurrealDB will coerce it silently.
    Compatible,
    /// The value cannot fit. **Only this warrants a diagnostic.**
    Incompatible,
    /// Not enough information, or a coercion too subtle to call.
    Unknown,
}

impl Verdict {
    pub fn is_incompatible(self) -> bool {
        matches!(self, Self::Incompatible)
    }
}

/// SurrealQL's primitive type names.
///
/// Deliberately a lookup over an open `&str` rather than a closed enum:
/// an unrecognised name must degrade to [`Verdict::Unknown`], not to a
/// parse failure or a false mismatch. New SurrealDB releases add type
/// names, and this checker must not start flagging valid code the day one
/// ships.
const PRIMITIVES: &[&str] = &[
    "any", "array", "bool", "bytes", "datetime", "decimal", "duration", "file", "float",
    "function", "geometry", "int", "none", "null", "number", "object", "point", "range", "record",
    "regex", "set", "string", "uuid", "value",
];

fn is_primitive(name: &str) -> bool {
    PRIMITIVES
        .iter()
        .any(|known| known.eq_ignore_ascii_case(name))
}

/// `any` and `value` accept anything and go anywhere.
fn is_top(name: &str) -> bool {
    name.eq_ignore_ascii_case("any") || name.eq_ignore_ascii_case("value")
}

fn is_nullish(name: &str) -> bool {
    name.eq_ignore_ascii_case("none") || name.eq_ignore_ascii_case("null")
}

/// Position in the numeric widening chain `int → float → decimal → number`.
fn numeric_rank(name: &str) -> Option<u8> {
    match name.to_ascii_lowercase().as_str() {
        "int" => Some(0),
        "float" => Some(1),
        "decimal" => Some(2),
        "number" => Some(3),
        _ => None,
    }
}

/// The primitive a literal type widens to: `'x'` → `string`, `42` → `int`,
/// `1h` → `duration`.
fn literal_family(raw: &str) -> Option<&'static str> {
    let text = raw.trim();
    let first = text.chars().next()?;
    if matches!(first, '\'' | '"') {
        return Some("string");
    }
    if text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("false") {
        return Some("bool");
    }
    if first.is_ascii_digit() || matches!(first, '-' | '+') {
        // `1h`, `2w`, `500ms` are durations; bare digits are numbers.
        if text.ends_with(|ch: char| ch.is_ascii_alphabetic())
            && !text.ends_with("f")
            && !text.ends_with("dec")
        {
            return Some("duration");
        }
        if text.ends_with("dec") {
            return Some("decimal");
        }
        if text.ends_with('f') || text.contains('.') {
            return Some("float");
        }
        return Some("int");
    }
    None
}

/// Widen a type one step for comparison: a literal becomes its family.
fn widen(ty: &TypeExpr) -> Option<TypeExpr> {
    match ty {
        TypeExpr::Literal(raw) => {
            literal_family(raw).map(|name| TypeExpr::Scalar(name.to_string()))
        }
        _ => None,
    }
}

/// Can a value of type `actual` be passed where `expected` is declared?
///
/// The rule order matters — earlier rules are the escape hatches that
/// keep this quiet, and they must run before any comparison that could
/// return [`Verdict::Incompatible`].
pub fn assignable(actual: &TypeExpr, expected: &TypeExpr) -> Verdict {
    use TypeExpr::*;

    // 1. No information on either side, or a shape we chose not to model.
    if matches!(actual, Unknown | Other(_)) || matches!(expected, Unknown | Other(_)) {
        return Verdict::Unknown;
    }

    // Identical types always fit. This has to run before literal widening
    // below, or `'Started'` against `'Started'` would widen to `string`,
    // fail the literal comparison, and come back Unknown.
    if actual == expected {
        return Verdict::Compatible;
    }

    // 2. `any`/`value` on either side accepts everything.
    if let Scalar(name) = actual
        && is_top(name)
    {
        return Verdict::Compatible;
    }
    if let Scalar(name) = expected
        && is_top(name)
    {
        return Verdict::Compatible;
    }

    // An unrecognised scalar name is not a mismatch, it is ignorance.
    if let Scalar(name) = actual
        && !is_primitive(name)
    {
        return Verdict::Unknown;
    }
    if let Scalar(name) = expected
        && !is_primitive(name)
    {
        return Verdict::Unknown;
    }

    // 3. Optionality.
    if let Option(inner) = expected {
        if let Scalar(name) = actual
            && is_nullish(name)
        {
            return Verdict::Compatible;
        }
        return assignable(actual, inner);
    }
    if matches!(actual, Option(_)) {
        // `option<T>` flowing into a non-optional slot. SurrealDB decides
        // at runtime, and real schemas do this constantly — flagging it
        // would be the single largest source of false positives.
        return Verdict::Unknown;
    }
    if let Scalar(name) = actual
        && is_nullish(name)
    {
        // NONE into a non-optional slot: a runtime concern, not ours.
        return Verdict::Unknown;
    }

    // 5. Unions — before literal widening, so that an exact literal member
    // (`'Started'` in `'Started' | 'Cancelled'`) matches on its own terms
    // rather than being widened to `string` first and losing.
    if let Union(members) = expected {
        let verdicts: Vec<_> = members
            .iter()
            .map(|member| assignable(actual, member))
            .collect();
        if verdicts.contains(&Verdict::Compatible) {
            return Verdict::Compatible;
        }
        return if verdicts.iter().all(|v| *v == Verdict::Incompatible) {
            Verdict::Incompatible
        } else {
            Verdict::Unknown
        };
    }
    if let Union(members) = actual {
        // Every branch must fit, or we cannot promise anything.
        let verdicts: Vec<_> = members
            .iter()
            .map(|member| assignable(member, expected))
            .collect();
        return if verdicts.iter().all(|v| *v == Verdict::Compatible) {
            Verdict::Compatible
        } else {
            Verdict::Unknown
        };
    }

    // 4/6. A literal widens to its family. Exact literal equality was
    // already settled above.
    if let Some(widened) = widen(actual) {
        return match assignable(&widened, expected) {
            // `string` is not assignable to the literal type `'open'`, but
            // a *specific* string might be — a runtime ASSERT decides.
            Verdict::Incompatible if matches!(expected, Literal(_)) => Verdict::Unknown,
            verdict => verdict,
        };
    }
    if matches!(expected, Literal(_)) {
        // A non-literal flowing into a literal slot: an ASSERT question.
        return Verdict::Unknown;
    }

    match (actual, expected) {
        // 7. Records: compatible unless both name tables and they are disjoint.
        (Record(from), Record(to)) => {
            // An empty table list is a bare `record`, which matches any
            // table; otherwise the two sets must overlap.
            let untabled = from.is_empty() || to.is_empty();
            if untabled || from.iter().any(|table| to.contains(table)) {
                Verdict::Compatible
            } else {
                Verdict::Incompatible
            }
        }

        // Collections against their bare primitive names.
        (Array(_) | Tuple(_), Scalar(name)) if name.eq_ignore_ascii_case("array") => {
            Verdict::Compatible
        }
        (Set(_), Scalar(name)) if name.eq_ignore_ascii_case("set") => Verdict::Compatible,
        (Object(_), Scalar(name)) if name.eq_ignore_ascii_case("object") => Verdict::Compatible,
        (Record(_), Scalar(name)) if name.eq_ignore_ascii_case("record") => Verdict::Compatible,
        (Scalar(name), Array(_) | Tuple(_)) if name.eq_ignore_ascii_case("array") => {
            Verdict::Unknown
        }
        (Scalar(name), Object(_)) if name.eq_ignore_ascii_case("object") => Verdict::Unknown,
        (Scalar(name), Record(_)) if name.eq_ignore_ascii_case("record") => Verdict::Unknown,

        (Array(from), Array(to)) | (Set(from), Set(to)) => match assignable(from, to) {
            // An empty `[]` types as `array<any>`; never punish it.
            Verdict::Incompatible => Verdict::Incompatible,
            _ => Verdict::Compatible,
        },
        (Tuple(from), Array(to)) => {
            if from
                .iter()
                .any(|item| assignable(item, to).is_incompatible())
            {
                Verdict::Incompatible
            } else {
                Verdict::Compatible
            }
        }
        (Array(_), Tuple(_)) => Verdict::Unknown,
        (Tuple(from), Tuple(to)) => {
            if from.len() != to.len() {
                return Verdict::Incompatible;
            }
            if from
                .iter()
                .zip(to)
                .any(|(a, b)| assignable(a, b).is_incompatible())
            {
                Verdict::Incompatible
            } else {
                Verdict::Compatible
            }
        }

        (Object(from), Object(to)) => object_verdict(from, to),

        // 8/9. Primitive against primitive.
        (Scalar(from), Scalar(to)) => {
            if from.eq_ignore_ascii_case(to) {
                return Verdict::Compatible;
            }
            match (numeric_rank(from), numeric_rank(to)) {
                // Widening is silent; narrowing is a runtime question.
                (Some(a), Some(b)) => {
                    if a <= b {
                        Verdict::Compatible
                    } else {
                        Verdict::Unknown
                    }
                }
                // A string may or may not coerce into these depending on
                // its contents — SurrealDB decides at runtime.
                _ if from.eq_ignore_ascii_case("string")
                    && matches!(
                        to.to_ascii_lowercase().as_str(),
                        "datetime" | "duration" | "uuid" | "bytes" | "regex" | "file"
                    ) =>
                {
                    Verdict::Unknown
                }
                _ => Verdict::Incompatible,
            }
        }

        // Structurally different shapes with nothing in common.
        (Record(_), Array(_) | Tuple(_) | Set(_) | Object(_))
        | (Array(_) | Tuple(_) | Set(_) | Object(_), Record(_))
        | (Object(_), Array(_) | Tuple(_) | Set(_))
        | (Array(_) | Tuple(_) | Set(_), Object(_))
        | (Set(_), Array(_) | Tuple(_))
        | (Array(_) | Tuple(_), Set(_)) => Verdict::Incompatible,

        // A scalar flowing into a collection slot is a runtime question, not an
        // error. An aggregate supplies the whole group where the source text
        // names one field, so `math::sum(<float> usage)` inside
        // `AS SELECT … GROUP BY …` is valid SurrealQL even though the argument
        // reads as a single float. Modelling that context is out of reach, and
        // reporting it flagged SurrealDB's own view tests.
        (Scalar(_), Set(_) | Array(_) | Tuple(_)) => Verdict::Unknown,
        // The reverse stays an error: a collection where a scalar is wanted is
        // wrong in any context, and `array::at([1, 2], [1, 2])` depends on it.
        (Set(_), Scalar(_)) | (Array(_) | Tuple(_), Scalar(_)) => Verdict::Incompatible,
        (Scalar(_), Object(_)) | (Object(_), Scalar(_)) => Verdict::Incompatible,
        (Scalar(_), Record(_)) | (Record(_), Scalar(_)) => Verdict::Incompatible,

        _ => Verdict::Unknown,
    }
}

/// What is wrong with one property of an object literal.
///
/// Returned separately from the [`Verdict`] so the diagnostic layer can
/// name the property and point at its value rather than reporting
/// "argument 2 is wrong" for a six-field object.
#[derive(Debug, Clone, PartialEq)]
pub enum ObjectFault {
    /// A property present on both sides whose type cannot fit.
    Property {
        key: String,
        expected: TypeExpr,
        actual: TypeExpr,
    },
    /// A required (non-`option<T>`) property the literal does not supply.
    Missing { key: String },
}

/// Compare an object against a declared object type.
///
/// Three deliberate asymmetries:
///
/// * **Shared keys are checked.** A key present on both sides is an
///   ordinary type comparison — nothing uncertain about it.
/// * **Missing keys are checked only when required.** A declared
///   `option<T>` property may be omitted; anything else may not.
/// * **Extra keys are ignored.** It is not established that SurrealQL
///   rejects properties a declared object type doesn't mention, and a
///   false positive there would land on valid code.
pub fn object_faults(
    actual: &[(String, TypeExpr)],
    expected: &[(String, TypeExpr)],
) -> Vec<ObjectFault> {
    let mut faults = Vec::new();
    for (key, want) in expected {
        match actual.iter().find(|(name, _)| name == key) {
            Some((_, have)) => {
                if assignable(have, want).is_incompatible() {
                    faults.push(ObjectFault::Property {
                        key: key.clone(),
                        expected: want.clone(),
                        actual: have.clone(),
                    });
                }
            }
            None if !matches!(want, TypeExpr::Option(_)) => {
                faults.push(ObjectFault::Missing { key: key.clone() })
            }
            None => {}
        }
    }
    faults
}

fn object_verdict(actual: &[(String, TypeExpr)], expected: &[(String, TypeExpr)]) -> Verdict {
    if !object_faults(actual, expected).is_empty() {
        return Verdict::Incompatible;
    }
    // No fault found, but a shared key we could not decide leaves the
    // whole object undecided.
    let uncertain = expected.iter().any(|(key, want)| {
        actual
            .iter()
            .find(|(name, _)| name == key)
            .is_some_and(|(_, have)| assignable(have, want) == Verdict::Unknown)
    });
    if uncertain {
        Verdict::Unknown
    } else {
        Verdict::Compatible
    }
}

#[cfg(test)]
mod tests {
    use super::{ObjectFault, Verdict, assignable, object_faults};
    use crate::semantic::type_expr::TypeExpr;

    fn s(name: &str) -> TypeExpr {
        TypeExpr::Scalar(name.to_string())
    }
    fn rec(tables: &[&str]) -> TypeExpr {
        TypeExpr::Record(tables.iter().map(|t| t.to_string()).collect())
    }
    fn arr(inner: TypeExpr) -> TypeExpr {
        TypeExpr::Array(Box::new(inner))
    }
    fn opt(inner: TypeExpr) -> TypeExpr {
        TypeExpr::Option(Box::new(inner))
    }
    fn lit(raw: &str) -> TypeExpr {
        TypeExpr::Literal(raw.to_string())
    }

    #[test]
    fn reports_the_users_actual_mistake() {
        // A string literal passed where a `record<user>` is declared.
        assert_eq!(
            assignable(&s("string"), &rec(&["user"])),
            Verdict::Incompatible
        );
        assert_eq!(
            assignable(&s("int"), &rec(&["user"])),
            Verdict::Incompatible
        );
    }

    #[test]
    fn plain_primitive_mismatches_are_reported() {
        for (from, to) in [
            ("string", "int"),
            ("int", "string"),
            ("bool", "string"),
            ("string", "bool"),
            ("object", "string"),
        ] {
            assert_eq!(
                assignable(&s(from), &s(to)),
                Verdict::Incompatible,
                "{from} -> {to}"
            );
        }
    }

    #[test]
    fn identical_and_widening_numerics_are_compatible() {
        assert_eq!(assignable(&s("string"), &s("string")), Verdict::Compatible);
        assert_eq!(assignable(&s("int"), &s("number")), Verdict::Compatible);
        assert_eq!(assignable(&s("int"), &s("decimal")), Verdict::Compatible);
        assert_eq!(assignable(&s("float"), &s("decimal")), Verdict::Compatible);
        // Narrowing is a runtime question, not an error.
        assert_eq!(assignable(&s("number"), &s("int")), Verdict::Unknown);
        assert_eq!(assignable(&s("decimal"), &s("float")), Verdict::Unknown);
    }

    #[test]
    fn unknown_and_other_are_always_silent() {
        for other in [TypeExpr::Unknown, TypeExpr::Other("weird<x>".into())] {
            assert_eq!(assignable(&other, &s("int")), Verdict::Unknown);
            assert_eq!(assignable(&s("int"), &other), Verdict::Unknown);
        }
    }

    #[test]
    fn any_and_value_accept_everything() {
        for top in ["any", "value"] {
            assert_eq!(assignable(&s("string"), &s(top)), Verdict::Compatible);
            assert_eq!(assignable(&s(top), &rec(&["user"])), Verdict::Compatible);
        }
    }

    #[test]
    fn unrecognised_type_names_never_produce_a_mismatch() {
        // A type SurrealDB adds tomorrow must not start flagging code.
        assert_eq!(assignable(&s("frobnicate"), &s("int")), Verdict::Unknown);
        assert_eq!(assignable(&s("int"), &s("frobnicate")), Verdict::Unknown);
    }

    #[test]
    fn option_handling_is_asymmetric_and_quiet() {
        // Into an optional slot: the inner type, or nothing at all.
        assert_eq!(
            assignable(&s("string"), &opt(s("string"))),
            Verdict::Compatible
        );
        assert_eq!(
            assignable(&s("none"), &opt(s("string"))),
            Verdict::Compatible
        );
        assert_eq!(
            assignable(&s("int"), &opt(s("string"))),
            Verdict::Incompatible
        );
        // Out of an optional slot: never flagged. This is the single
        // biggest false-positive source in real schemas.
        assert_eq!(
            assignable(&opt(s("string")), &s("string")),
            Verdict::Unknown
        );
        assert_eq!(assignable(&opt(s("string")), &s("int")), Verdict::Unknown);
        assert_eq!(assignable(&s("none"), &s("string")), Verdict::Unknown);
    }

    #[test]
    fn record_tables_must_be_disjoint_to_be_a_mismatch() {
        assert_eq!(
            assignable(&rec(&["user"]), &rec(&["user"])),
            Verdict::Compatible
        );
        // `record<orderData | project>` accepts a `record<project>`.
        assert_eq!(
            assignable(&rec(&["project"]), &rec(&["orderData", "project"])),
            Verdict::Compatible
        );
        // A bare `record` matches any table.
        assert_eq!(assignable(&rec(&[]), &rec(&["user"])), Verdict::Compatible);
        assert_eq!(assignable(&rec(&["user"]), &rec(&[])), Verdict::Compatible);
        assert_eq!(
            assignable(&rec(&["user"]), &rec(&["company"])),
            Verdict::Incompatible
        );
    }

    #[test]
    fn literals_widen_to_their_family() {
        assert_eq!(
            assignable(&lit("'open'"), &s("string")),
            Verdict::Compatible
        );
        assert_eq!(assignable(&lit("42"), &s("int")), Verdict::Compatible);
        assert_eq!(assignable(&lit("42"), &s("number")), Verdict::Compatible);
        assert_eq!(assignable(&lit("1h"), &s("duration")), Verdict::Compatible);
        assert_eq!(assignable(&lit("'open'"), &s("int")), Verdict::Incompatible);
    }

    #[test]
    fn widening_into_a_literal_union_stays_silent() {
        // `TYPE 'Started' | 'Cancelled'` fed a `string` — an ASSERT decides.
        let union = TypeExpr::Union(vec![lit("'Started'"), lit("'Cancelled'")]);
        assert_eq!(assignable(&s("string"), &union), Verdict::Unknown);
        assert_eq!(assignable(&lit("'Started'"), &union), Verdict::Compatible);
    }

    #[test]
    fn unions_widen_the_expected_side_and_narrow_the_actual() {
        let expected = TypeExpr::Union(vec![s("string"), s("int")]);
        assert_eq!(assignable(&s("string"), &expected), Verdict::Compatible);
        assert_eq!(assignable(&s("bool"), &expected), Verdict::Incompatible);

        let actual = TypeExpr::Union(vec![s("string"), s("int")]);
        assert_eq!(assignable(&actual, &s("string")), Verdict::Unknown);
        assert_eq!(
            assignable(&actual, &TypeExpr::Union(vec![s("string"), s("int")])),
            Verdict::Compatible
        );
    }

    #[test]
    fn collections_compare_structurally() {
        assert_eq!(
            assignable(&arr(s("string")), &s("array")),
            Verdict::Compatible
        );
        assert_eq!(
            assignable(&arr(s("string")), &arr(s("string"))),
            Verdict::Compatible
        );
        assert_eq!(
            assignable(&arr(s("string")), &arr(s("int"))),
            Verdict::Incompatible
        );
        // An empty array literal types as `array<any>` and must fit anywhere.
        assert_eq!(
            assignable(&arr(s("any")), &arr(rec(&["user"]))),
            Verdict::Compatible
        );
        // A scalar into a collection slot is a runtime question, not an error:
        // an aggregate hands the function the whole group where the source text
        // names a single field, so `math::sum(<float> usage)` in
        // `AS SELECT … GROUP BY …` is valid. Reporting it flagged SurrealDB's
        // own view tests. The reverse direction stays an error.
        assert_eq!(
            assignable(&s("string"), &arr(s("string"))),
            Verdict::Unknown
        );
        assert_eq!(
            assignable(&arr(s("string")), &s("string")),
            Verdict::Incompatible
        );
    }

    #[test]
    fn tuples_check_arity_and_elements() {
        let pair = TypeExpr::Tuple(vec![s("string"), s("string")]);
        assert_eq!(assignable(&pair, &pair.clone()), Verdict::Compatible);
        assert_eq!(assignable(&pair, &arr(s("string"))), Verdict::Compatible);
        assert_eq!(assignable(&pair, &arr(s("int"))), Verdict::Incompatible);
        assert_eq!(
            assignable(&pair, &TypeExpr::Tuple(vec![s("string")])),
            Verdict::Incompatible
        );
    }

    #[test]
    fn object_shared_keys_are_compared() {
        // `{ line: 15102, asset: 2 }` against the declared shape.
        let actual = TypeExpr::Object(vec![("line".into(), s("int")), ("asset".into(), s("int"))]);
        let expected = TypeExpr::Object(vec![
            ("line".into(), rec(&["orderLine"])),
            ("asset".into(), rec(&["asset"])),
        ]);

        assert_eq!(assignable(&actual, &expected), Verdict::Incompatible);
        assert_eq!(
            object_faults(
                &[("line".into(), s("int")), ("asset".into(), s("int"))],
                &[
                    ("line".into(), rec(&["orderLine"])),
                    ("asset".into(), rec(&["asset"]))
                ]
            ),
            vec![
                ObjectFault::Property {
                    key: "line".into(),
                    expected: rec(&["orderLine"]),
                    actual: s("int"),
                },
                ObjectFault::Property {
                    key: "asset".into(),
                    expected: rec(&["asset"]),
                    actual: s("int"),
                },
            ]
        );
    }

    #[test]
    fn object_missing_required_property_is_a_fault() {
        let faults = object_faults(
            &[("line".into(), rec(&["orderLine"]))],
            &[
                ("line".into(), rec(&["orderLine"])),
                ("asset".into(), rec(&["asset"])),
            ],
        );
        assert_eq!(
            faults,
            vec![ObjectFault::Missing {
                key: "asset".into()
            }]
        );
    }

    #[test]
    fn object_optional_property_may_be_omitted() {
        let faults = object_faults(
            &[("line".into(), rec(&["orderLine"]))],
            &[
                ("line".into(), rec(&["orderLine"])),
                ("note".into(), opt(s("string"))),
            ],
        );
        assert!(faults.is_empty(), "an option<T> property is not required");
    }

    #[test]
    fn object_extra_properties_are_ignored() {
        // Not established that SurrealQL seals object types; staying
        // silent keeps valid code clean.
        let faults = object_faults(
            &[
                ("line".into(), rec(&["orderLine"])),
                ("extra".into(), s("string")),
            ],
            &[("line".into(), rec(&["orderLine"]))],
        );
        assert!(faults.is_empty());
    }

    #[test]
    fn object_with_undecidable_property_stays_silent() {
        let actual = TypeExpr::Object(vec![("line".into(), TypeExpr::Unknown)]);
        let expected = TypeExpr::Object(vec![("line".into(), rec(&["orderLine"]))]);
        assert_eq!(assignable(&actual, &expected), Verdict::Unknown);
    }

    #[test]
    fn object_still_fits_the_bare_object_primitive() {
        let a = TypeExpr::Object(vec![("line".into(), rec(&["orderLine"]))]);
        assert_eq!(assignable(&a, &s("object")), Verdict::Compatible);
    }

    #[test]
    fn string_to_stringly_types_stays_silent() {
        for to in ["datetime", "duration", "uuid", "bytes", "regex", "file"] {
            assert_eq!(
                assignable(&s("string"), &s(to)),
                Verdict::Unknown,
                "string -> {to} must not be reported"
            );
        }
    }
}
