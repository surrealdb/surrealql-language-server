//! Which operand pairs SurrealQL's arithmetic operators accept.
//!
//! # Why this is transcribed and not derived
//!
//! The documentation does not answer this. It never states a rule for
//! `string + int`, and it never states a rule for numeric promotion in
//! arithmetic — its operators page shows same-type examples only. So every
//! table here is read from the engine at revision `9d9a5b069`, the revision
//! [`crate::grammar_generated::SURREALDB_REVISION`] already pins, and each one
//! cites the file and lines it came from.
//!
//! The tables are also irregular enough that no shared rule would capture them:
//! `+` concatenates two strings but rejects a string and an int; `*` accepts
//! `duration * int` but *not* `int * duration`; `/` never fails at all. Read the
//! engine, not your intuition.
//!
//! # The silence rule
//!
//! Everything here is subordinate to the same invariant as
//! [`crate::semantic::infer`]: a diagnostic fires only when the failure is
//! provable. [`value_kind`] is the gate. It answers `None` for every type that
//! is not certainly one concrete engine kind, and a `None` on either side means
//! the checker says nothing. Widening what `value_kind` recognises is the only
//! way this module can produce a false positive, so a new arm there must be
//! correct rather than merely plausible.

use crate::semantic::type_expr::TypeExpr;

/// The `Value` variants the engine's arithmetic tables distinguish.
///
/// [`Self::Number`] is the static type `number`, which the engine has no
/// runtime variant for — every real value is an int, a float, or a decimal. It
/// is kept so that a declared `number` still types an operation, at the cost of
/// a coarser result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Int,
    Float,
    Decimal,
    /// Some number, but not known which.
    Number,
    String,
    Datetime,
    Duration,
    Array,
    Set,
    Object,
    /// A kind that is certainly concrete and appears in no arm of any table.
    ///
    /// `record:one + 1` and `true + 1` both fail, and we can prove it. This is
    /// the variant that lets the checker report rather than shrug.
    Other,
}

impl ValueKind {
    /// True for every kind the engine wraps in `Value::Number`.
    fn is_number(self) -> bool {
        matches!(self, Self::Int | Self::Float | Self::Decimal | Self::Number)
    }

    /// The type this kind reports as.
    ///
    /// A collection loses its element type: `[1] + [2]` answers `array`, not
    /// `array<int>`. The engine concatenates, so the element types would have to
    /// be unioned, and `array` is the honest coarse answer.
    pub fn as_type(self) -> TypeExpr {
        let name = match self {
            Self::Int => "int",
            Self::Float => "float",
            Self::Decimal => "decimal",
            Self::Number => "number",
            Self::String => "string",
            Self::Datetime => "datetime",
            Self::Duration => "duration",
            Self::Array => "array",
            Self::Set => "set",
            Self::Object => "object",
            // No arm produces `Other`, so this is unreachable in practice.
            Self::Other => return TypeExpr::Unknown,
        };
        TypeExpr::Scalar(name.to_string())
    }
}

/// The arithmetic operators, as the engine names their failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
}

impl ArithOp {
    /// The operator's spelling, or `None` when it is not arithmetic.
    ///
    /// `×` and `÷` are the documented Unicode spellings of `*` and `/`
    /// (`grammar.js`, `_binary_op_token`). Note `*=` is *all equal* in
    /// SurrealQL, not multiply-assign, so it is deliberately absent.
    pub fn parse(spelling: &str) -> Option<Self> {
        Some(match spelling {
            "+" => Self::Add,
            "-" => Self::Sub,
            "*" | "×" => Self::Mul,
            "/" | "÷" => Self::Div,
            "**" => Self::Pow,
            _ => return None,
        })
    }

    /// True when a failure of this operator is worth reporting.
    ///
    /// Division is the exception. `fnc::operate::div` is
    /// `Ok(a.try_div(b).unwrap_or(f64::NAN.into()))` (`fnc/operate.rs:31-33`),
    /// so `[1,2,3] / 1` evaluates to `NaN` rather than failing. There is no
    /// error to surface.
    pub fn can_fail(self) -> bool {
        !matches!(self, Self::Div)
    }

    /// The engine's own wording for a failure of this operator.
    ///
    /// `Error::TryAdd` and friends live at `err/mod.rs:647-673`. Note that
    /// `**` has its own sentence rather than a "perform" phrase.
    pub fn failure_message(self, lhs: &TypeExpr, rhs: &TypeExpr) -> String {
        let verb = match self {
            Self::Add => "addition",
            Self::Sub => "subtraction",
            Self::Mul => "multiplication",
            Self::Div => "division",
            Self::Pow => {
                return format!("Cannot raise the value `{lhs}` with `{rhs}`.");
            }
        };
        format!("Cannot perform {verb} with `{lhs}` and `{rhs}`.")
    }
}

/// The kind `op` produces from these operands, or `None` when the engine fails.
///
/// Transcribed from the `TryAdd`/`TrySub`/`TryMul`/`TryDiv`/`TryPow` impls for
/// `Value`:
///
/// * addition — `val/mod.rs:648-677`, ten arms
/// * subtraction — `val/mod.rs:686-712`, nine arms
/// * multiplication — `val/mod.rs:721-773`, four arms
/// * division — `val/mod.rs:782-830`, four arms
/// * power — `val/mod.rs:873-881`, one arm
///
/// Anything absent from those impls reaches the engine's catch-all `bail!`, so
/// `None` here means "the query fails at run time".
pub fn arith_result(op: ArithOp, lhs: ValueKind, rhs: ValueKind) -> Option<ValueKind> {
    use ArithOp::{Add, Div, Mul, Pow, Sub};
    use ValueKind::{Array, Datetime, Duration, Object, Set, String};

    // Every operator has a `(Number, Number)` arm, and only that arm promotes.
    if lhs.is_number() && rhs.is_number() {
        return Some(number_result(lhs, rhs));
    }

    Some(match (op, lhs, rhs) {
        // `String + String` concatenates. No other operator takes a string —
        // `"8" % "3"` and `"a" - "b"` both fail.
        (Add, String, String) => String,

        (Add, Datetime, Duration) | (Add, Duration, Datetime) => Datetime,
        (Add, Duration, Duration) => Duration,
        (Add, Array, Array) | (Add, Array, Set) => Array,
        (Add, Set, Set) | (Add, Set, Array) => Set,
        (Add, Object, Object) => Object,

        // `datetime - datetime` is the only arm that changes category.
        (Sub, Datetime, Datetime) => Duration,
        (Sub, Datetime, Duration) | (Sub, Duration, Datetime) => Datetime,
        (Sub, Duration, Duration) => Duration,
        (Sub, Array, Array) | (Sub, Array, Set) => Array,
        (Sub, Set, Set) | (Sub, Set, Array) => Set,

        // Scaling a duration, and deliberately one-directional: the engine has
        // no `(Number, Duration)` arm, so `1s * 2` works and `2 * 1s` fails.
        (Mul, Duration, right) if right.is_number() => Duration,
        (Div, Duration, right) if right.is_number() => Duration,

        // Division never fails. A pair with no arm yields `NaN`, which is a
        // float — so the expression still has a type, and nothing is reported.
        (Div, _, _) => ValueKind::Float,

        (Pow, _, _) => return None,

        _ => return None,
    })
}

/// The kind the engine promotes a numeric pair to.
///
/// Transcribed from the `impl_simple_try_op!` macro at
/// `val/number.rs:925-955`, whose catch-all arm converts both sides to a
/// decimal. `Number` is not a runtime variant, so a pair involving it can only
/// be answered coarsely.
fn number_result(lhs: ValueKind, rhs: ValueKind) -> ValueKind {
    use ValueKind::{Decimal, Float, Int, Number};
    match (lhs, rhs) {
        // Nothing is known about which number this is.
        (Number, _) | (_, Number) => Number,
        (Int, Int) => Int,
        (Float, Float) | (Int, Float) | (Float, Int) => Float,
        // `(Decimal, Decimal)`, and every remaining mixed pair, goes to decimal.
        _ => Decimal,
    }
}

/// The engine kind a type certainly denotes, or `None`.
///
/// This is the gate for the whole check. `None` means "not provably one
/// concrete kind", and the caller must then stay silent.
///
/// The three groups below are deliberate:
///
/// * a name that maps to an engine variant the tables mention;
/// * a name that maps to a variant no arm accepts, which becomes
///   [`ValueKind::Other`] so that `true + 1` is *reported* rather than ignored;
/// * everything else, which is `None`.
pub fn value_kind(ty: &TypeExpr) -> Option<ValueKind> {
    match ty {
        TypeExpr::Scalar(name) => scalar_kind(name),

        // A tuple type (`[string, string]`) is an array at run time.
        TypeExpr::Array(_) | TypeExpr::Tuple(_) => Some(ValueKind::Array),
        TypeExpr::Set(_) => Some(ValueKind::Set),
        TypeExpr::Object(_) => Some(ValueKind::Object),

        // A record id takes part in no arithmetic arm.
        TypeExpr::Record(_) => Some(ValueKind::Other),

        // `'x'` behaves as its family does. Reuse the one widening step the
        // assignability relation already defines, so the two cannot disagree.
        TypeExpr::Literal(_) => crate::semantic::assign::widen(ty)
            .as_ref()
            .and_then(value_kind),

        // `Unknown` and `Other` are absence of information. `Option` may hold a
        // NONE *or* a number, so nothing about it is provable — the same
        // position `assignable` takes for an optional on the value side. A
        // `Union` is the same argument.
        TypeExpr::Unknown | TypeExpr::Other(_) | TypeExpr::Option(_) | TypeExpr::Union(_) => None,
    }
}

/// The engine kind a primitive type name denotes.
///
/// The `Other` list is the load-bearing one: every name in it is a `Value`
/// variant that appears in no arm of any table, so a failure involving it is
/// provable. A name absent from both lists answers `None`, which covers `any`,
/// `value`, and anything a future SurrealDB release adds.
fn scalar_kind(name: &str) -> Option<ValueKind> {
    Some(match name.to_ascii_lowercase().as_str() {
        "int" => ValueKind::Int,
        "float" => ValueKind::Float,
        "decimal" => ValueKind::Decimal,
        "number" => ValueKind::Number,
        "string" => ValueKind::String,
        "datetime" => ValueKind::Datetime,
        "duration" => ValueKind::Duration,
        "array" => ValueKind::Array,
        "set" => ValueKind::Set,
        "object" => ValueKind::Object,

        // Concrete, and accepted nowhere.
        //
        // `point` is a `Value::Geometry`, as is `geometry`. Note the reverse
        // direction is *not* covered: an object literal that the engine reads as
        // a geometry types here as an object, so `{…} + {…}` stays silent where
        // the engine would fail. That is a missed report, which is the safe way
        // round.
        "bool" | "bytes" | "uuid" | "regex" | "geometry" | "point" | "file" | "range"
        | "function" | "record" | "table" | "none" | "null" => ValueKind::Other,

        // `any`, `value`, and every name this mapper has not been taught.
        _ => return None,
    })
}

/// The precedence rank of a binary operator, as the engine ranks it.
///
/// The engine's `BindingPower` (`sql/operator.rs:535-549`) runs
/// `Nullish < Or < And < Equality < Relation < AddSub < MulDiv < Power`, mapped
/// from spellings at `syn/parser/expression.rs:78-140`. Every one is
/// left-associative (`syn/parser/expression.rs:77`).
///
/// Only the three arithmetic ranks are distinguished. Everything else collapses
/// to zero, and that is sufficient rather than lazy: a non-arithmetic operator
/// is never *checked*, so its rank matters only in that it must bind looser than
/// `+`. Collapsing also makes the unrecognisable spellings safe — a KNN
/// operator (`<|2|>`), a `@1@` match, and a `+=` assignment all land at zero,
/// which lets the arithmetic parts of their chain still group correctly.
///
/// NOTE: The published documentation puts `??`/`?:` *above* `**`. That is wrong.
/// The engine's own `language/expression/operators/precedence.surql` asserts
/// `2 + 1 ?: true + 1` is `3`, which holds only when `?:` binds loosest.
pub fn binding_power(spelling: &str) -> u8 {
    match spelling {
        "**" => 3,
        "*" | "×" | "/" | "÷" => 2,
        "+" | "-" => 1,
        _ => 0,
    }
}

/// True when this operator may leave its right-hand side unevaluated.
///
/// `??` returns its left side when that side is not nullish, `?:` when the left
/// side is truthy, and `&&`/`||` are the usual short circuits. SurrealDB's own
/// `language/expression/operators/precedence.surql` depends on it: it asserts
/// `2 + 1 ?: true + 1` is `3`, so `true + 1` — which would fail — never runs.
///
/// An arithmetic failure on the right of one of these is therefore not provable,
/// and must not be reported.
pub fn short_circuits(spelling: &str) -> bool {
    matches!(
        spelling.to_ascii_uppercase().as_str(),
        "??" | "?:" | "&&" | "||" | "AND" | "OR"
    )
}

/// Collapse an operator's source text to a single comparable spelling.
///
/// An `Operator` node can span several tokens — `NOT IN`, `IS NOT`,
/// `<|2, COSINE|>` — and `k::text_of` trims only the outside, so the interior
/// whitespace survives. Nothing here depends on the keyword forms, but they must
/// not accidentally match an arithmetic spelling.
pub fn normalize_operator(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(name: &str) -> TypeExpr {
        TypeExpr::Scalar(name.to_string())
    }

    fn kind(name: &str) -> ValueKind {
        value_kind(&scalar(name)).expect("a known kind")
    }

    #[test]
    fn the_reported_case_is_a_failure() {
        // `RETURN "" + "222" + 3;` — the inner pair concatenates, and the outer
        // pair is the one SurrealDB rejects.
        assert_eq!(
            arith_result(ArithOp::Add, kind("string"), kind("string")),
            Some(ValueKind::String)
        );
        assert_eq!(
            arith_result(ArithOp::Add, kind("string"), kind("int")),
            None
        );
    }

    #[test]
    fn the_cast_forms_of_the_reported_case_are_accepted() {
        // `<string>3` makes the outer pair two strings.
        assert_eq!(
            arith_result(ArithOp::Add, kind("string"), kind("string")),
            Some(ValueKind::String)
        );
        // `<int>"0" + <int>"222" + 3` is int throughout.
        assert_eq!(
            arith_result(ArithOp::Add, kind("int"), kind("int")),
            Some(ValueKind::Int)
        );
    }

    #[test]
    fn numbers_promote_along_the_engine_chain() {
        for (left, right, expected) in [
            ("int", "int", ValueKind::Int),
            ("int", "float", ValueKind::Float),
            ("float", "int", ValueKind::Float),
            ("float", "float", ValueKind::Float),
            ("decimal", "decimal", ValueKind::Decimal),
            // The catch-all arm converts both sides to a decimal.
            ("int", "decimal", ValueKind::Decimal),
            ("float", "decimal", ValueKind::Decimal),
            // Nothing is known about which number a declared `number` is.
            ("number", "int", ValueKind::Number),
        ] {
            assert_eq!(
                arith_result(ArithOp::Add, kind(left), kind(right)),
                Some(expected),
                "{left} + {right}"
            );
        }
    }

    #[test]
    fn multiplication_by_a_duration_is_one_directional() {
        // The engine has a `(Duration, Number)` arm and no reverse, so `2 * 1s`
        // genuinely fails while `1s * 2` does not.
        assert_eq!(
            arith_result(ArithOp::Mul, kind("duration"), kind("int")),
            Some(ValueKind::Duration)
        );
        assert_eq!(
            arith_result(ArithOp::Mul, kind("int"), kind("duration")),
            None
        );
    }

    #[test]
    fn collections_combine_but_do_not_take_a_scalar() {
        // `array + set` answers array and `set + array` answers set: the engine's
        // result is not symmetric.
        assert_eq!(
            arith_result(ArithOp::Add, kind("array"), kind("set")),
            Some(ValueKind::Array)
        );
        assert_eq!(
            arith_result(ArithOp::Add, kind("set"), kind("array")),
            Some(ValueKind::Set)
        );
        // The corpus pins all four of these as failures.
        for op in [ArithOp::Add, ArithOp::Sub] {
            assert_eq!(arith_result(op, kind("array"), kind("int")), None);
            assert_eq!(arith_result(op, kind("set"), kind("int")), None);
        }
    }

    #[test]
    fn duration_arithmetic_follows_the_corpus() {
        assert_eq!(
            arith_result(ArithOp::Add, kind("duration"), kind("duration")),
            Some(ValueKind::Duration)
        );
        assert_eq!(
            arith_result(ArithOp::Sub, kind("datetime"), kind("datetime")),
            Some(ValueKind::Duration)
        );
        assert_eq!(
            arith_result(ArithOp::Add, kind("datetime"), kind("duration")),
            Some(ValueKind::Datetime)
        );
        // Both pinned by `duration/arithmatic_operations.surql`.
        assert_eq!(
            arith_result(ArithOp::Mul, kind("duration"), kind("duration")),
            None
        );
        assert_eq!(
            arith_result(ArithOp::Pow, kind("duration"), kind("duration")),
            None
        );
    }

    #[test]
    fn division_never_fails() {
        // `[1,2,3] / 1` is `NaN`, not an error, so every pair must answer
        // `Some` — and the operator is excluded from reporting anyway.
        for left in ["array", "string", "object", "bool"] {
            assert!(
                arith_result(ArithOp::Div, kind(left), kind("int")).is_some(),
                "{left} / int must not be reported"
            );
        }
        assert!(!ArithOp::Div.can_fail());
        assert!(ArithOp::Add.can_fail());
    }

    #[test]
    fn a_concrete_kind_with_no_arm_is_provably_wrong() {
        // This is what `ValueKind::Other` buys: a report rather than a shrug.
        for name in ["bool", "bytes", "uuid", "regex", "none", "null", "record"] {
            assert_eq!(value_kind(&scalar(name)), Some(ValueKind::Other), "{name}");
            assert_eq!(
                arith_result(ArithOp::Add, kind(name), kind("int")),
                None,
                "{name} + int"
            );
        }
    }

    #[test]
    fn the_gate_refuses_everything_it_cannot_prove() {
        // Each of these must stay silent. A `Some` here is a false positive.
        let unprovable = [
            TypeExpr::Unknown,
            TypeExpr::Other("weird<thing>".to_string()),
            TypeExpr::Option(Box::new(scalar("int"))),
            TypeExpr::Union(vec![scalar("int"), scalar("string")]),
            scalar("any"),
            scalar("value"),
            // A name no release has taught this mapper.
            scalar("quaternion"),
        ];
        for ty in unprovable {
            assert_eq!(value_kind(&ty), None, "{ty} must not be judged");
        }
    }

    #[test]
    fn a_literal_behaves_as_its_family() {
        assert_eq!(
            value_kind(&TypeExpr::Literal("'x'".to_string())),
            Some(ValueKind::String)
        );
        assert_eq!(
            value_kind(&TypeExpr::Literal("42".to_string())),
            Some(ValueKind::Int)
        );
        assert_eq!(
            value_kind(&TypeExpr::Literal("1h".to_string())),
            Some(ValueKind::Duration)
        );
    }

    #[test]
    fn arithmetic_binds_tighter_than_anything_else() {
        assert!(binding_power("**") > binding_power("*"));
        assert!(binding_power("*") > binding_power("+"));
        assert_eq!(binding_power("+"), binding_power("-"));
        assert_eq!(binding_power("*"), binding_power("×"));
        // Everything non-arithmetic must bind looser than `+`, so an arithmetic
        // run inside a larger chain groups correctly.
        for other in [
            "==", "&&", "??", "?:", "IN", "CONTAINS", "<|2|>", "@1@", "+=",
        ] {
            assert!(
                binding_power(other) < binding_power("+"),
                "{other} must bind looser than `+`"
            );
        }
    }

    #[test]
    fn only_arithmetic_spellings_parse_as_operators() {
        assert_eq!(ArithOp::parse("+"), Some(ArithOp::Add));
        assert_eq!(ArithOp::parse("×"), Some(ArithOp::Mul));
        assert_eq!(ArithOp::parse("÷"), Some(ArithOp::Div));
        assert_eq!(ArithOp::parse("**"), Some(ArithOp::Pow));
        // `*=` is *all equal* in SurrealQL, and `+=`/`-=` go through the looser
        // increment path. None of them is arithmetic here.
        for spelling in ["*=", "+=", "-=", "==", "?=", "IN", "@@"] {
            assert_eq!(ArithOp::parse(spelling), None, "{spelling}");
        }
    }

    #[test]
    fn the_message_matches_the_engine_wording() {
        let string = scalar("string");
        let int = scalar("int");
        assert_eq!(
            ArithOp::Add.failure_message(&string, &int),
            "Cannot perform addition with `string` and `int`."
        );
        // `**` has its own sentence in the engine.
        assert_eq!(
            ArithOp::Pow.failure_message(&string, &int),
            "Cannot raise the value `string` with `int`."
        );
    }

    #[test]
    fn the_short_circuiting_operators_are_recognised() {
        for spelling in ["??", "?:", "&&", "||", "AND", "and", "OR"] {
            assert!(short_circuits(spelling), "{spelling} short-circuits");
        }
        for spelling in ["+", "-", "*", "**", "==", "IN"] {
            assert!(!short_circuits(spelling), "{spelling} does not");
        }
    }

    #[test]
    fn operator_text_collapses_interior_whitespace() {
        assert_eq!(normalize_operator("NOT  IN"), "NOT IN");
        assert_eq!(normalize_operator("IS\n NOT"), "IS NOT");
        assert_eq!(normalize_operator("+"), "+");
    }
}
