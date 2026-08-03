//! Check the catalogue's return types by running the functions.
//!
//! ```text
//! cargo run -p xtask --features probe -- verify-returns --surrealdb <path>
//! ```
//!
//! # Why this exists
//!
//! The return types in the catalogue come from the engine's registry
//! (`exec/function/builtin/`), and the engine never reads that registry.
//! `ScalarFunction::signature` carries `#[allow(unused)]` and no engine test
//! asserts against it, so a wrong declaration there would compile, ship, and
//! then make this language server report a diagnostic against working
//! SurrealQL. Reading the source twice does not fix that. Running the function
//! does.
//!
//! So this boots an in-memory datastore, calls every function with arguments
//! built from the parameter types the catalogue already holds, and asks the
//! engine what type came back.
//!
//! # How a disagreement is judged
//!
//! Not every difference is a fault, and the rules below are what keep this from
//! reporting noise:
//!
//! * A recorded `any` accepts anything. It is the absence of a claim.
//! * A recorded type wider than the answer passes: `number` covers an `int`.
//!   Narrowing is silent in the checker, so a wide record costs nothing.
//! * Two probes with different arguments must agree with each other. When they
//!   do not, the return type follows the input — `array::first` gives an `int`
//!   for `[1]` and a `string` for `['a']` — and the only correct record is
//!   `any`. A recorded type here is a fault worth reporting.
//! * A `NONE` answer proves nothing. Many functions return `NONE` on input they
//!   cannot use, and the declaration describes the case that works.
//! * An error answer proves nothing either. The arguments are synthesised, and
//!   the engine is entitled to reject them.

use std::collections::BTreeMap;

use surrealdb_core::dbs::Session;
use surrealdb_core::kvs::Datastore;
use surrealdb_types::Value;

use crate::emit::CatalogueEntry;
use crate::kinds::ParamForm;

/// Namespaces the probe must not call.
///
/// Each one either reaches outside the query or needs a context this probe
/// cannot build. `http::` would make a network request, `sleep` would stall the
/// run, and the rest answer only inside a request, a live index or a defined
/// resource.
const SKIPPED_NAMESPACES: &[&str] = &[
    "api::",
    "eval::",
    "file::",
    "http::",
    "schema::",
    "search::",
    "sequence::",
    "session::",
];

/// Bare names the probe must not call, for the same reasons.
const SKIPPED_NAMES: &[&str] = &["sleep", "rand::enum"];

/// One disagreement worth a human's attention.
pub struct Disagreement {
    pub name: String,
    pub recorded: String,
    pub observed: String,
    pub reason: String,
}

/// Run every function and compare the answer with what the catalogue records.
pub fn verify(entries: &[CatalogueEntry]) -> Result<Vec<Disagreement>, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start a tokio runtime: {error}"))?;
    runtime.block_on(verify_async(entries))
}

async fn verify_async(entries: &[CatalogueEntry]) -> Result<Vec<Disagreement>, String> {
    let datastore = Datastore::new("memory")
        .await
        .map_err(|error| format!("cannot open an in-memory datastore: {error}"))?;
    let session = Session::owner().with_ns("probe").with_db("probe");

    // `type::table` and the record functions need a table to point at, and a
    // record id that resolves to nothing behaves differently from one that does.
    let setup = "CREATE person:tobie SET name = 'Tobie';";
    datastore
        .execute(setup, &session, None)
        .await
        .map_err(|error| format!("cannot prepare the probe database: {error}"))?;

    let mut disagreements = Vec::new();
    let mut probed = 0usize;
    let mut inconclusive = 0usize;

    for entry in entries {
        if skipped(&entry.name) || entry.not_callable {
            continue;
        }

        let first = probe(&datastore, &session, entry, ArgumentStyle::Numeric).await;
        let second = probe(&datastore, &session, entry, ArgumentStyle::Textual).await;

        let (Some(first), Some(second)) = (first, second) else {
            inconclusive += 1;
            continue;
        };
        probed += 1;

        if first != second {
            // Two numeric answers are not a fault. The numeric types form a
            // widening chain, so one name covers both: `math::abs` gives an
            // `int` for an `int` and a `float` for a `float`, and `number` is
            // honest about all of it. The record only has to be at least as wide
            // as the widest answer.
            if let (Some(first_rank), Some(second_rank)) =
                (numeric_rank(&first), numeric_rank(&second))
            {
                let widest = if first_rank > second_rank {
                    &first
                } else {
                    &second
                };
                if let Some(reason) = fault(&entry.returns, widest) {
                    disagreements.push(Disagreement {
                        name: entry.name.clone(),
                        recorded: entry.returns.clone(),
                        observed: format!("{first} then {second}"),
                        reason,
                    });
                }
                continue;
            }

            // Answers from different families mean the type follows the input,
            // and `any` is the only honest record.
            if entry.returns != "any" {
                disagreements.push(Disagreement {
                    name: entry.name.clone(),
                    recorded: entry.returns.clone(),
                    observed: format!("{first} then {second}"),
                    reason: "the return type follows the argument, so it cannot be one type"
                        .to_string(),
                });
            }
            continue;
        }

        if let Some(reason) = fault(&entry.returns, &first) {
            disagreements.push(Disagreement {
                name: entry.name.clone(),
                recorded: entry.returns.clone(),
                observed: first,
                reason,
            });
        }
    }

    eprintln!(
        "  probed {probed} functions, {inconclusive} gave no usable answer, \
         {} disagreements",
        disagreements.len()
    );
    Ok(disagreements)
}

fn skipped(name: &str) -> bool {
    SKIPPED_NAMESPACES
        .iter()
        .any(|namespace| name.starts_with(namespace))
        || SKIPPED_NAMES.contains(&name)
}

/// Why the recorded type is wrong for this answer, or `None` when it is fine.
fn fault(recorded: &str, observed: &str) -> Option<String> {
    // No claim to check.
    if recorded == "any" {
        return None;
    }
    if recorded == observed {
        return None;
    }
    // `geometry<point>` answers a `point` or a `geometry` record. The engine
    // narrows further than the lattice does here, and both names are right.
    if observed.starts_with("geometry") && matches!(recorded, "geometry" | "point") {
        return None;
    }
    // A record may be wider than the answer. Narrowing is silent in the
    // checker, so a wide record loses precision and never invents a diagnostic.
    if let (Some(recorded_rank), Some(observed_rank)) =
        (numeric_rank(recorded), numeric_rank(observed))
        && recorded_rank > observed_rank
    {
        return None;
    }
    // `array<string>` recorded against a plain `array` answer: the engine's
    // `type::of` never reports an element type, so this is as close as the probe
    // can get to agreement.
    if let Some((outer, _)) = recorded.split_once('<')
        && outer == observed
    {
        return None;
    }
    Some(format!("the engine answered `{observed}`"))
}

/// Position in the numeric widening chain `int → float → decimal → number`.
fn numeric_rank(name: &str) -> Option<u8> {
    match name {
        "int" => Some(0),
        "float" => Some(1),
        "decimal" => Some(2),
        "number" => Some(3),
        _ => None,
    }
}

/// Which of the two argument sets to build.
///
/// The two must differ in the *type* of what they carry, not only the value.
/// That difference is what exposes a function whose return type follows its
/// input.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ArgumentStyle {
    Numeric,
    Textual,
}

/// Call one function and return the type of the answer, when there is one.
async fn probe(
    datastore: &Datastore,
    session: &Session,
    entry: &CatalogueEntry,
    style: ArgumentStyle,
) -> Option<String> {
    let arguments = arguments_for(entry, style)?;
    // `type::of` is the engine's own answer to "what is this", and it narrows an
    // `int` from a `float` where the public `Value::kind()` reports only
    // `number`.
    let query = format!("RETURN type::of({}({}));", entry.name, arguments.join(", "));

    let mut responses = datastore.execute(&query, session, None).await.ok()?;
    if responses.len() != 1 {
        return None;
    }
    let value = responses.remove(0).result.ok()?;
    match value {
        Value::String(observed) if observed != "none" && observed != "null" => Some(observed),
        // `NONE` is what a function answers for input it cannot use. The
        // declaration describes the case that works, so this proves nothing.
        _ => None,
    }
}

/// One SurrealQL literal per parameter, or `None` when the probe cannot build a
/// call.
fn arguments_for(entry: &CatalogueEntry, style: ArgumentStyle) -> Option<Vec<String>> {
    // An unreadable signature means the parameter list is empty because nothing
    // was read, not because the function takes nothing. Calling it with no
    // arguments is still worth trying: `rand::int()` is valid.
    if !entry.signature_known {
        return Some(Vec::new());
    }

    let mut arguments = Vec::new();
    for param in &entry.params {
        // A variadic or optional parameter can be left out, and leaving it out
        // is the call an author is most likely to write.
        if param.form != ParamForm::Required {
            continue;
        }
        arguments.push(literal(&param.ty, style)?);
    }
    Some(arguments)
}

/// A SurrealQL literal of the given type.
fn literal(ty: &str, style: ArgumentStyle) -> Option<String> {
    let textual = style == ArgumentStyle::Textual;
    let rendered = match ty {
        "any" => {
            if textual {
                "'a'"
            } else {
                "1"
            }
        }
        "bool" => {
            if textual {
                "false"
            } else {
                "true"
            }
        }
        "bytes" => "<bytes>'a'",
        "datetime" => {
            if textual {
                "d'2025-06-01T12:00:00Z'"
            } else {
                "d'2024-01-01T00:00:00Z'"
            }
        }
        "decimal" => "1dec",
        "duration" => {
            if textual {
                "2h"
            } else {
                "1h"
            }
        }
        "float" => "1.5f",
        "int" => {
            if textual {
                "2"
            } else {
                "1"
            }
        }
        "number" => {
            if textual {
                "2.5f"
            } else {
                "1"
            }
        }
        "object" => {
            if textual {
                "{ a: 'x' }"
            } else {
                "{ a: 1 }"
            }
        }
        "range" => "1..3",
        "record" => "person:tobie",
        "regex" => "/a/",
        "string" => {
            if textual {
                "'bcd'"
            } else {
                "'a'"
            }
        }
        "table" => "type::table('person')",
        "uuid" => "u'019535d9-3df7-79fb-b466-fa907fa17f9e'",
        "geometry" | "point" => "(-0.118, 51.509)",
        "function" => "|$v| { RETURN $v; }",
        "file" => "f'bucket:/key'",
        // The element type is what makes the two styles differ, which is what
        // exposes `array::first` and its neighbours.
        "array" | "set" => {
            let inner = if textual { "['a', 'b']" } else { "[1, 2]" };
            return Some(if ty == "set" {
                format!("<set>{inner}")
            } else {
                inner.to_string()
            });
        }
        other => {
            // `array<number>`, `array<string>`, `set<int>` and friends.
            let (outer, element) = other.split_once('<')?;
            let element = element.trim_end_matches('>');
            let one = literal(element, style)?;
            let two = literal(element, style)?;
            let inner = format!("[{one}, {two}]");
            return Some(if outer == "set" {
                format!("<set>{inner}")
            } else {
                inner
            });
        }
    };
    Some(rendered.to_string())
}

/// Group the disagreements by namespace, for a report a human can act on.
pub fn report(disagreements: &[Disagreement]) -> String {
    let mut by_namespace: BTreeMap<&str, Vec<&Disagreement>> = BTreeMap::new();
    for found in disagreements {
        let namespace = found
            .name
            .split_once("::")
            .map(|(namespace, _)| namespace)
            .unwrap_or("(bare)");
        by_namespace.entry(namespace).or_default().push(found);
    }

    let mut out = String::new();
    for (namespace, found) in by_namespace {
        out.push_str(&format!("{namespace}::\n"));
        for one in found {
            out.push_str(&format!(
                "  {} records `{}` — {} ({})\n",
                one.name, one.recorded, one.reason, one.observed
            ));
        }
    }
    out
}
