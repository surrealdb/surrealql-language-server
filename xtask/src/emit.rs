//! Render the catalogue as Rust source.

use std::collections::{BTreeMap, BTreeSet};

use crate::engine_tables::PathEntry;
use crate::kinds::ParamForm;
use crate::methods::Receiver;
use crate::signatures::Implementation;

pub struct Catalogue {
    pub revision: String,
    pub functions: Vec<CatalogueEntry>,
    pub constants: Vec<String>,
    pub renames: Vec<(String, String)>,
    pub not_callable: Vec<String>,
    pub namespaces: BTreeSet<String>,
    /// The method tables, one per receiver `Value` variant plus the catch-all.
    pub receivers: Vec<ReceiverEntry>,
}

pub struct ReceiverEntry {
    /// The engine `Value` variant, or empty for the catch-all table.
    pub kind: String,
    /// `(method, function, experimental target)`, sorted by method.
    pub methods: Vec<(String, String, Option<String>)>,
}

pub struct CatalogueEntry {
    pub name: String,
    pub params: Vec<crate::kinds::Param>,
    pub is_async: bool,
    /// True when the name is in `PATHS` but no dispatch arm implements it in
    /// call form, so the parser accepts it and the engine then refuses it.
    pub not_callable: bool,
    /// True when the generator read this function's implementation.
    ///
    /// Load-bearing for the argument checks. Without it an unreadable signature
    /// and a genuinely zero-argument function are both `params: &[]`, and the
    /// checker would report "expects 0 arguments" for every call to the former.
    pub signature_known: bool,
    /// The SurrealQL type this function returns, or `any`.
    ///
    /// From the engine's registry (see [`crate::returns`]), with [`OVERLAY`]
    /// filling in the kinds the registry's macros cannot spell. `any` silences
    /// every check that would read it, which is the right answer whenever the
    /// return type follows an argument's type.
    pub returns: String,
}

/// Return types the engine declares as `Kind::Any` and a reader can be sure of.
///
/// The registry's macros take the return kind as a bare identifier, so a kind
/// carrying a payload cannot be written and arrives as `Any`
/// (`exec/function/builtin/array.rs:3`). Some of those are genuinely unknowable
/// and must stay `any`: `array::first` returns whatever the array holds. The
/// rest are certain, and this table states them.
///
/// Two rules keep this honest. An entry applies only where the engine said
/// `Any`, so it can never contradict a declaration. And an entry names a type
/// this crate can defend from the engine's implementation, quoted per group
/// below — a guess here reports a diagnostic against valid SurrealQL, which
/// costs more than the silence it replaces.
const OVERLAY: &[(&str, &str)] = &[
    // `object::keys` collects `object.keys().map(..)`, and the engine's object
    // keys are strings (`fnc/object.rs:79`). `string::split` and `string::words`
    // collect `str::split` and `str::split_whitespace` (`fnc/string.rs`).
    ("object::keys", "array<string>"),
    ("string::split", "array<string>"),
    ("string::words", "array<string>"),
    // The element type follows the argument, so these must stay silent:
    // `object::values`, `object::entries`, `array::first`, `array::group`,
    // `array::flatten`, `set::first` and every other collection accessor. They
    // are deliberately absent rather than mapped to `array`.
    //
    // The vector functions take and return a numeric vector
    // (`fnc/vector.rs`), and the engine rejects a non-numeric element before
    // any of them runs.
    ("vector::add", "array<number>"),
    ("vector::cross", "array<number>"),
    ("vector::divide", "array<number>"),
    ("vector::multiply", "array<number>"),
    ("vector::normalize", "array<number>"),
    ("vector::project", "array<number>"),
    ("vector::scale", "array<number>"),
    ("vector::subtract", "array<number>"),
    // A `type::` constructor returns the type it names, which is the rule a
    // `<type>` cast follows. Each one is `val.cast_to::<T>()` in
    // `fnc/type.rs`, so the target type is written in the implementation.
    ("type::array", "array"),
    ("type::bytes", "bytes"),
    ("type::file", "file"),
    ("type::geometry", "geometry"),
    // `point` rather than `geometry`, because the lattice has both and the
    // curated table already names this one `point` (`assign.rs:43` lists it as a
    // primitive). Two names for one type would make the two tables disagree.
    ("type::point", "point"),
    ("type::range", "range"),
    ("type::record", "record"),
    ("type::set", "set"),
    ("type::table", "table"),
    // `encoding::base64::decode` returns `Value::Bytes` (`fnc/encoding.rs`).
    ("encoding::base64::decode", "bytes"),
];

/// The overlay type for a name, when the engine declared nothing usable.
fn overlay(name: &str, declared: &str) -> Option<&'static str> {
    if declared != "any" {
        return None;
    }
    OVERLAY
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, ty)| *ty)
}

/// Declarations the engine gets wrong, and what the engine actually returns.
///
/// Unlike [`OVERLAY`], an entry here *replaces* a declaration the engine made.
/// That is a strong claim, so the bar is a demonstration rather than an argument:
/// every entry was produced by the `verify-returns` task, which calls the
/// function in a real engine and reads the type of the answer. The reason the
/// registry can be wrong at all is that SurrealDB never reads it —
/// `ScalarFunction::signature` carries `#[allow(unused)]` and no engine test
/// asserts on it, so a wrong kind there compiles and ships.
///
/// Keep this table empty if you can. A growing list means the engine's registry
/// is drifting from its implementations, and the fix belongs upstream.
const CORRECTIONS: &[(&str, &str, &str)] = &[
    // Declared `(value: Any) -> String` at `exec/function/builtin/crypto.rs:10`,
    // but `fnc/crypto.rs:12` is `Ok(joaat::hash_bytes(..).into())` over a `u32`,
    // which reaches SurrealQL as an `int`. Believing the declaration would
    // report `math::abs(crypto::joaat($s))` as a type error on working code.
    ("crypto::joaat", "string", "int"),
];

/// Correct a declaration the probe proved wrong.
///
/// The declared value is matched as well as the name, so a fix upstream retires
/// the entry loudly rather than quietly: once the engine says `int` too, the
/// pair stops matching and [`unmatched_corrections`] reports it.
fn correction(name: &str, declared: &str) -> Option<&'static str> {
    CORRECTIONS
        .iter()
        .find(|(candidate, wrong, _)| *candidate == name && *wrong == declared)
        .map(|(_, _, right)| *right)
}

/// Corrections that no longer correct anything.
///
/// A correction overrides the engine, so an entry that has stopped applying is
/// the most dangerous kind of stale data: either SurrealDB fixed the
/// declaration, in which case the entry is noise, or the function was renamed,
/// in which case a wrong type is back and unguarded. Either way a human should
/// look, so the generator refuses to run.
pub fn unmatched_corrections(declared_returns: &BTreeMap<String, String>) -> Vec<&'static str> {
    CORRECTIONS
        .iter()
        .filter(|(name, wrong, _)| declared_returns.get(*name).map(String::as_str) != Some(wrong))
        .map(|(name, _, _)| *name)
        .collect()
}

/// Namespaces whose leading arguments the runtime supplies, so the Rust
/// signature is wider than what an author writes.
///
/// `api::` holds middleware. `api::timeout` reads
/// `(req, next, timeout): (Value, Box<Closure>, Duration)`, but the API runtime
/// supplies `req` and `next`; the author writes `api::timeout(30s)`. Nothing in
/// the types marks the first two as injected — `Box<Closure>` is an ordinary
/// author-supplied argument in `array::map` and `value::chain` — so the
/// convention cannot be detected, only recorded.
///
/// These are reported with an unknown signature, which means silence. Checking
/// them would report a wrong count on every valid middleware clause; SurrealDB's
/// own `language-tests/tests/api/` corpus proved it.
const RUNTIME_SUPPLIED_NAMESPACES: &[&str] = &["api::"];

fn has_runtime_supplied_arguments(name: &str) -> bool {
    RUNTIME_SUPPLIED_NAMESPACES
        .iter()
        .any(|namespace| name.starts_with(namespace))
}

/// Join the engine's tables into one catalogue.
pub fn build(
    paths: &[PathEntry],
    dispatch: &BTreeMap<String, String>,
    implementations: &BTreeMap<String, Implementation>,
    declared_returns: &BTreeMap<String, String>,
    revision: String,
    namespaces: BTreeSet<String>,
    receivers: &[Receiver],
) -> Catalogue {
    let mut functions = Vec::new();
    let mut constants = Vec::new();
    let mut renames = Vec::new();
    let mut not_callable = Vec::new();

    for entry in paths {
        if let Some(previous) = &entry.previous_name {
            renames.push((previous.clone(), entry.name.clone()));
        }
        if !entry.is_function {
            constants.push(entry.name.clone());
            continue;
        }

        let implementation = dispatch
            .get(&entry.name)
            .and_then(|path| implementations.get(path));
        let missing = !dispatch.contains_key(&entry.name);
        if missing {
            not_callable.push(entry.name.clone());
        }
        let runtime_supplied = has_runtime_supplied_arguments(&entry.name);

        // A name the registry does not declare gets `any`, which is silence. The
        // nine names that parse but cannot be called are the bulk of those, and
        // a return type for a function the engine refuses to run means nothing.
        let declared = declared_returns
            .get(&entry.name)
            .map(String::as_str)
            .unwrap_or("any");
        let returns = correction(&entry.name, declared)
            .or_else(|| overlay(&entry.name, declared))
            .unwrap_or(declared);

        functions.push(CatalogueEntry {
            name: entry.name.clone(),
            params: if runtime_supplied {
                Vec::new()
            } else {
                implementation
                    .map(|found| found.params.clone())
                    .unwrap_or_default()
            },
            is_async: implementation.is_some_and(|found| found.is_async),
            not_callable: missing,
            signature_known: implementation.is_some() && !runtime_supplied,
            returns: returns.to_string(),
        });
    }

    renames.sort();
    renames.dedup();

    // Pair each method with the name an author would write for the same
    // implementation. A path with no dispatch entry is method-only, and keeps
    // its Rust path so the reader still learns where it lives.
    let names = crate::methods::surrealql_names(dispatch);
    let receivers = receivers
        .iter()
        .map(|receiver| {
            let mut methods: Vec<(String, String, Option<String>)> = receiver
                .methods
                .iter()
                .map(|arm| {
                    let function = names
                        .get(&arm.path)
                        .cloned()
                        .unwrap_or_else(|| arm.path.clone());
                    (arm.method.clone(), function, arm.experimental.clone())
                })
                .collect();
            methods.sort();
            methods.dedup();
            ReceiverEntry {
                kind: receiver.kind.clone(),
                methods,
            }
        })
        .collect();

    Catalogue {
        revision,
        functions,
        constants,
        renames,
        not_callable,
        namespaces,
        receivers,
    }
}

pub fn render(catalogue: &Catalogue) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "//! The SurrealDB builtin function catalogue.\n\
         //!\n\
         //! @generated by `cargo xtask generate-builtins` — do not edit by hand.\n\
         //! Generated from the SurrealDB checkout at revision `{revision}`, out of\n\
         //! `syn/parser/builtin.rs` (the names), `fnc/mod.rs` (the dispatch tables),\n\
         //! the `pub fn` signatures under `fnc/` (the argument types) and the registry\n\
         //! under `exec/function/builtin/` (the return types).\n\
         //!\n\
         //! A parameter the generator could not read is typed `any`, which silences the\n\
         //! argument check for that position. A return type reads `any` under the same\n\
         //! rule. That is deliberate: a wrong type here would invent a diagnostic\n\
         //! against valid SurrealQL.\n\
         \n\
         use crate::grammar::{{GeneratedFunction, GeneratedMethod, GeneratedParam, GeneratedReceiver, ParamForm}};\n\
         \n\
         /// The SurrealDB revision this catalogue was generated from.\n\
         pub const SURREALDB_REVISION: &str = \"{revision}\";\n\n",
        revision = catalogue.revision
    ));

    out.push_str(&format!(
        "/// Every function name the SurrealDB parser accepts, with the argument types\n\
         /// its implementation declares and the return type the engine's registry\n\
         /// declares. {count} entries.\n\
         pub const GENERATED_FUNCTIONS: &[GeneratedFunction] = &[\n",
        count = catalogue.functions.len()
    ));
    for entry in &catalogue.functions {
        out.push_str(&render_entry(entry));
    }
    out.push_str("];\n\n");

    out.push_str(&format!(
        "/// The namespace prefixes that really exist, derived from the names above.\n\
         /// {count} entries.\n\
         pub const GENERATED_NAMESPACES: &[&str] = &[\n",
        count = catalogue.namespaces.len()
    ));
    for namespace in &catalogue.namespaces {
        out.push_str(&format!("    \"{namespace}\",\n"));
    }
    out.push_str("];\n\n");

    out.push_str(&format!(
        "/// Constants such as `math::PI`. They take no arguments and are not called.\n\
         /// {count} entries.\n\
         pub const GENERATED_CONSTANTS: &[&str] = &[\n",
        count = catalogue.constants.len()
    ));
    for constant in &catalogue.constants {
        out.push_str(&format!("    \"{constant}\",\n"));
    }
    out.push_str("];\n\n");

    out.push_str(&format!(
        "/// Renamed functions, as `(previous spelling, current spelling)`. The engine\n\
         /// keeps these to suggest a replacement, so they make a quick fix. {count} pairs.\n\
         pub const RENAMED_FUNCTIONS: &[(&str, &str)] = &[\n",
        count = catalogue.renames.len()
    ));
    for (previous, current) in &catalogue.renames {
        out.push_str(&format!("    (\"{previous}\", \"{current}\"),\n"));
    }
    out.push_str("];\n\n");

    out.push_str(&format!(
        "/// Names the parser accepts that no dispatch arm implements in call form. A\n\
         /// query using one parses and then fails at run time. {count} entries.\n\
         pub const PARSES_BUT_NOT_CALLABLE: &[&str] = &[\n",
        count = catalogue.not_callable.len()
    ));
    for name in &catalogue.not_callable {
        out.push_str(&format!("    \"{name}\",\n"));
    }
    out.push_str("];\n");

    let arms: usize = catalogue
        .receivers
        .iter()
        .map(|receiver| receiver.methods.len())
        .sum();
    out.push_str(&format!(
        "/// Which function a `value.method()` call dispatches to, per receiver.\n\
         ///\n\
         /// Read from `fnc::idiom` in `fnc/mod.rs`. The mapping is **not**\n\
         /// `<receiver>::<method>`: `Number` dispatches into `math::`, `Geometry` into\n\
         /// `geo::` and `Datetime` into `time::`, and 52 names flatten a path, so that\n\
         /// `is_alphanum` is `string::is::alphanum`.\n\
         ///\n\
         /// Each receiver's list is complete rather than layered over a shared block.\n\
         /// `String` shadows four of the common arms with different arities and drops\n\
         /// `is_set` altogether, so a default-plus-overrides model would be wrong.\n\
         ///\n\
         /// {receivers} receivers, {arms} arms.\n\
         pub const GENERATED_RECEIVERS: &[GeneratedReceiver] = &[\n",
        receivers = catalogue.receivers.len(),
        arms = arms
    ));
    for receiver in &catalogue.receivers {
        out.push_str("    GeneratedReceiver {\n");
        out.push_str(&format!("        kind: \"{}\",\n", receiver.kind));
        out.push_str("        methods: &[\n");
        for (method, function, experimental) in &receiver.methods {
            let experimental = match experimental {
                Some(target) => format!("Some(\"{target}\")"),
                None => "None".to_string(),
            };
            out.push_str(&format!(
                "            GeneratedMethod {{ method: \"{method}\", function: \"{function}\", experimental: {experimental} }},\n"
            ));
        }
        out.push_str("        ],\n");
        out.push_str("    },\n");
    }
    out.push_str("];\n\n");

    out
}

fn render_entry(entry: &CatalogueEntry) -> String {
    let mut out = String::new();
    out.push_str("    GeneratedFunction {\n");
    out.push_str(&format!("        name: \"{}\",\n", entry.name));
    if entry.params.is_empty() {
        out.push_str("        params: &[],\n");
    } else {
        out.push_str("        params: &[\n");
        for param in &entry.params {
            out.push_str(&format!(
                "            GeneratedParam {{ name: \"{}\", ty: \"{}\", form: ParamForm::{} }},\n",
                param.name,
                param.ty,
                form_name(param.form)
            ));
        }
        out.push_str("        ],\n");
    }
    out.push_str(&format!("        is_async: {},\n", entry.is_async));
    out.push_str(&format!("        not_callable: {},\n", entry.not_callable));
    out.push_str(&format!(
        "        signature_known: {},\n",
        entry.signature_known
    ));
    out.push_str(&format!("        returns: \"{}\",\n", entry.returns));
    out.push_str("    },\n");
    out
}

fn form_name(form: ParamForm) -> &'static str {
    match form {
        ParamForm::Required => "Required",
        ParamForm::Optional => "Optional",
        ParamForm::Variadic => "Variadic",
    }
}
