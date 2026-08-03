//! Guards on the generated builtin catalogue (`src/grammar_generated.rs`).
//!
//! Two jobs. The freshness check proves the committed file still matches what
//! the generator produces from the SurrealDB checkout, so the catalogue cannot
//! drift silently. The shape checks hold the invariants the argument checks
//! depend on, and they run everywhere — including CI, which has no SurrealDB
//! checkout.

use std::path::PathBuf;
use std::process::Command;

use surrealql_language_server::grammar::ParamForm;
use surrealql_language_server::grammar_generated::{
    GENERATED_CONSTANTS, GENERATED_FUNCTIONS, GENERATED_NAMESPACES, PARSES_BUT_NOT_CALLABLE,
    RENAMED_FUNCTIONS, SURREALDB_REVISION,
};
use surrealql_language_server::semantic::type_expr::TypeExpr;

/// The SurrealDB checkout, when this machine has one.
///
/// `SURREALDB_DIR` first, then the sibling layout the grammar already uses
/// (`../surrealdb` beside this repository). Returns `None` when neither holds a
/// checkout, which is the normal case in continuous integration — the tests
/// that need one skip rather than fail.
fn surrealdb_dir() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("SURREALDB_DIR").map(PathBuf::from),
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../surrealdb")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|path| path.join("surrealdb/core/src/fnc/mod.rs").is_file())
}

#[test]
fn the_committed_catalogue_matches_the_generator() {
    let Some(surrealdb) = surrealdb_dir() else {
        eprintln!("skipping: no SurrealDB checkout. Set SURREALDB_DIR to run this check.");
        return;
    };

    let output = Command::new(env!("CARGO"))
        .args(["xtask", "generate-builtins", "--surrealdb"])
        .arg(&surrealdb)
        .arg("--check")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo xtask must run");

    assert!(
        output.status.success(),
        "the committed catalogue is stale:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_catalogue_records_which_revision_it_came_from() {
    assert!(
        !SURREALDB_REVISION.is_empty() && SURREALDB_REVISION != "unknown",
        "a catalogue with no provenance cannot be audited"
    );
}

#[test]
fn the_catalogue_covers_the_whole_engine_surface() {
    // The engine's own counts, verified against
    // `syn/parser/builtin.rs` (434 PathKind::Function + 27 PathKind::Constant).
    assert_eq!(GENERATED_FUNCTIONS.len(), 434);
    assert_eq!(GENERATED_CONSTANTS.len(), 27);
    assert_eq!(RENAMED_FUNCTIONS.len(), 62);
}

#[test]
fn only_the_known_exceptions_have_an_unreadable_signature() {
    // An unread signature is silent, so this is a coverage guard, not a
    // correctness one. Every entry below is a deliberate exclusion with a
    // reason, and the list must not grow by accident.
    //
    // `api::*` is middleware: the API runtime supplies `(request, next)`, a
    // convention the Rust types do not express.
    // `rand::float/int/time` take `NoneOrRange<T>`, whose `FromArg` declares
    // zero-or-two arguments — an arity this catalogue cannot represent.
    let mut unread: Vec<&str> = GENERATED_FUNCTIONS
        .iter()
        .filter(|function| !function.not_callable && !function.signature_known)
        .map(|function| function.name)
        .collect();
    unread.sort_unstable();
    assert_eq!(
        unread,
        vec![
            "api::invoke",
            "api::req::body",
            "api::res::body",
            "api::res::header",
            "api::res::headers",
            "api::res::status",
            "api::timeout",
            "rand::float",
            "rand::int",
            "rand::time",
        ],
        "the set of deliberately-unread signatures changed"
    );
}

#[test]
fn the_catalogue_reads_almost_every_callable_signature() {
    let known = GENERATED_FUNCTIONS
        .iter()
        .filter(|function| function.signature_known)
        .count();
    let callable = GENERATED_FUNCTIONS
        .iter()
        .filter(|function| !function.not_callable)
        .count();
    assert!(
        known * 100 >= callable * 97,
        "only {known} of {callable} callable signatures were read"
    );
}

#[test]
fn an_unknown_signature_never_looks_like_zero_arity() {
    // The trap this invariant exists to prevent: `params: &[]` means "takes no
    // arguments" only when the signature was read. Otherwise an arity check
    // would report "expects 0 arguments" for every call.
    for function in GENERATED_FUNCTIONS {
        if !function.signature_known {
            assert!(
                function.params.is_empty(),
                "`{}` has parameters but claims an unknown signature",
                function.name
            );
        }
    }
}

#[test]
fn the_names_that_parse_but_cannot_be_called_are_the_known_nine() {
    let mut found: Vec<&str> = PARSES_BUT_NOT_CALLABLE.to_vec();
    found.sort_unstable();
    assert_eq!(
        found,
        vec![
            "duration::set_day",
            "duration::set_hour",
            "duration::set_minute",
            "duration::set_month",
            "duration::set_nanosecond",
            "duration::set_second",
            "duration::set_year",
            "object::matches",
            "value::chain",
        ],
        "the set of parse-but-not-callable names changed"
    );
}

#[test]
fn the_namespaces_exclude_the_bare_functions() {
    // The hand-written list advertises `not::` and `sleep::`, but `not`,
    // `sleep`, `count` and `rand` are bare functions — there is no such
    // namespace, so offering the prefix is a wrong answer.
    for wrong in ["not::", "sleep::", "count::"] {
        assert!(
            !GENERATED_NAMESPACES.contains(&wrong),
            "`{wrong}` is not a SurrealDB namespace"
        );
    }
    for real in ["array::", "string::", "math::", "type::", "vector::"] {
        assert!(GENERATED_NAMESPACES.contains(&real), "`{real}` is missing");
    }
}

#[test]
fn a_variadic_parameter_is_always_last() {
    // The arity model assumes it: a variadic absorbs every remaining argument,
    // so a parameter after one could never be reached.
    for function in GENERATED_FUNCTIONS {
        if let Some(position) = function
            .params
            .iter()
            .position(|param| param.form == ParamForm::Variadic)
        {
            assert_eq!(
                position,
                function.params.len() - 1,
                "`{}` has a parameter after a variadic",
                function.name
            );
        }
    }
}

#[test]
fn a_required_parameter_never_follows_an_optional_one() {
    // The arity model computes one lower bound, so optionals must form a
    // trailing run.
    for function in GENERATED_FUNCTIONS {
        let mut seen_optional = false;
        for param in function.params {
            match param.form {
                ParamForm::Optional | ParamForm::Variadic => seen_optional = true,
                ParamForm::Required => assert!(
                    !seen_optional,
                    "`{}` has a required parameter after an optional one",
                    function.name
                ),
            }
        }
    }
}

#[test]
fn every_parameter_type_is_a_name_the_checker_understands() {
    // A type the language server's lattice cannot place degrades to silence, so
    // this is not a correctness risk — but a typo would silence a whole
    // parameter, so it should be visible.
    for function in GENERATED_FUNCTIONS {
        for param in function.params {
            let base = param
                .ty
                .split_once('<')
                .map_or(param.ty, |(outer, _)| outer);
            assert!(
                KNOWN_TYPES.contains(&base),
                "`{}` parameter `{}` has type `{}`, which the checker does not know",
                function.name,
                param.name,
                param.ty
            );
        }
    }
}

#[test]
fn a_rename_never_maps_a_name_onto_itself() {
    for (previous, current) in RENAMED_FUNCTIONS {
        assert_ne!(previous, current, "`{previous}` renames to itself");
    }
}

// --- Return types -----------------------------------------------------------

/// The type names the checker's lattice can place.
///
/// Shared by the parameter and return-type guards: both feed `TypeExpr::parse`,
/// and a name outside this list degrades to silence there.
/// Kept in step with `PRIMITIVES` in `src/semantic/assign.rs`, which is the list
/// the assignability rules actually consult. `point` is on that list and appears
/// only as a return type, never as a parameter.
const KNOWN_TYPES: &[&str] = &[
    "any", "array", "bool", "bytes", "datetime", "decimal", "duration", "file", "float",
    "function", "geometry", "int", "none", "null", "number", "object", "point", "range", "record",
    "regex", "set", "string", "table", "uuid",
];

#[test]
fn every_return_type_is_a_name_the_checker_understands() {
    for function in GENERATED_FUNCTIONS {
        let base = function
            .returns
            .split_once('<')
            .map_or(function.returns, |(outer, _)| outer);
        assert!(
            KNOWN_TYPES.contains(&base),
            "`{}` returns `{}`, which the checker does not know",
            function.name,
            function.returns
        );
    }
}

#[test]
fn the_catalogue_types_the_majority_of_callable_returns() {
    // A floor, not a target. The ceiling is set by the engine: its registry
    // declares the return kind as a bare identifier, so no kind carrying a
    // payload can be written and about a third arrive as `Any`. Some of those
    // are right — `array::first` returns whatever the array holds.
    let typed = GENERATED_FUNCTIONS
        .iter()
        .filter(|function| !function.not_callable && function.returns != "any")
        .count();
    let callable = GENERATED_FUNCTIONS
        .iter()
        .filter(|function| !function.not_callable)
        .count();
    assert!(
        typed * 100 >= callable * 55,
        "only {typed} of {callable} callable functions have a return type"
    );
}

#[test]
fn a_name_that_cannot_be_called_is_never_given_a_return_type() {
    // The engine refuses to run these nine, so a return type for one of them
    // would describe a call that always fails.
    for function in GENERATED_FUNCTIONS {
        if function.not_callable {
            assert_eq!(
                function.returns, "any",
                "`{}` cannot be called but declares a return type",
                function.name
            );
        }
    }
}

#[test]
fn the_two_return_type_tables_never_contradict_each_other() {
    // The guard that matters most in this file. The curated table was written by
    // hand against the documentation; the generated one is read out of the
    // engine. Where both are specific they must agree, because one of them is
    // wrong otherwise — and a wrong return type reports against valid SurrealQL.
    //
    // Widening is allowed in one direction only: the curated table may name a
    // wider numeric type than the engine (`string::len` is `number` there and
    // `int` in the registry), because the checker treats narrowing as unknown
    // and stays silent. The reverse would be a claim the engine does not make.
    fn numeric_rank(name: &str) -> Option<u8> {
        match name {
            "int" => Some(0),
            "float" => Some(1),
            "decimal" => Some(2),
            "number" => Some(3),
            _ => None,
        }
    }

    let mut compared = 0usize;
    for curated in surrealql_language_server::grammar::BUILTIN_FUNCTIONS {
        let Some((_, declared)) = curated.signature.rsplit_once("->") else {
            continue;
        };
        let declared = declared.trim();
        let Some(generated) = GENERATED_FUNCTIONS
            .iter()
            .find(|function| function.name == curated.name)
        else {
            panic!("curated `{}` is not in the catalogue", curated.name);
        };
        if generated.returns == "any" || declared == "any" {
            // The engine could not spell it, or the curated table would not
            // commit. Either way there is nothing to contradict.
            continue;
        }
        // A curated type the lattice cannot model — `range<record>`,
        // `array<field>` — carries no information and loses to the engine inside
        // `builtin_return_type`, so it cannot contradict anything either. This
        // mirrors the resolver's own rule rather than comparing raw text.
        if matches!(
            TypeExpr::parse(declared),
            TypeExpr::Unknown | TypeExpr::Other(_)
        ) {
            continue;
        }
        compared += 1;
        if declared == generated.returns {
            continue;
        }
        match (numeric_rank(declared), numeric_rank(generated.returns)) {
            (Some(curated_rank), Some(engine_rank)) if curated_rank > engine_rank => {}
            _ => panic!(
                "`{}` returns `{}` in the curated table and `{}` in the engine's registry",
                curated.name, declared, generated.returns
            ),
        }
    }
    assert!(
        compared >= 60,
        "only {compared} entries were comparable, so this guard proved little"
    );
}
