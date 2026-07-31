//! Conformance of the argument checks against SurrealDB itself.
//!
//! The unit tests prove the checks fire where they should. These prove the far
//! more important half: that they stay **silent** on code the engine accepts. A
//! wrong diagnostic costs more than a missing one
//! (`src/semantic/assign.rs:1-16`), and an argument check runs at every call
//! site in every document, so a single bad catalogue entry squiggles working
//! queries.
//!
//! Two layers, because they fail in different situations:
//!
//! * [`valid_builtin_calls_stay_silent`] reads a committed fixture, so it runs
//!   everywhere — including continuous integration, which has no SurrealDB
//!   checkout.
//! * [`the_surrealdb_corpus_produces_only_expected_diagnostics`] sweeps all
//!   ~1,900 files of `language-tests/` and asserts the *exact* set of
//!   diagnostics, so a new false positive fails the build rather than hiding in
//!   a count. It skips without a checkout.
//!
//! To refresh the fixture, extract one call per distinct function from files
//! that declare no expected error:
//!
//! ```text
//! cd $SURREALDB/language-tests/tests
//! # keep lines ending in `;` that call a namespaced builtin, from files with
//! # no `error =` / `parsing-error`, skipping clause fragments and `api::`
//! ```

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use surrealql_language_server::config::ServerSettings;
use surrealql_language_server::semantic::analyzer::analyze_document;
use surrealql_language_server::semantic::types::{
    MergedSemanticModel, SymbolOrigin, WorkspaceIndex,
};
use tower_lsp_server::ls_types::{Diagnostic, NumberOrString, Uri};

fn uri(path: &str) -> Uri {
    format!("file:///workspace/{path}").parse().expect("uri")
}

/// Every diagnostic the pipeline publishes for one document.
fn diagnostics_for(source: &str) -> Vec<Diagnostic> {
    let Some(analysis) = analyze_document(uri("q.surql"), source, SymbolOrigin::Local) else {
        return Vec::new();
    };
    let mut workspace = WorkspaceIndex::default();
    workspace
        .documents
        .insert(uri("q.surql"), std::sync::Arc::new(analysis.clone()));
    let model = MergedSemanticModel::build(&workspace, &Default::default());
    model.semantic_diagnostics(&analysis, &ServerSettings::default())
}

/// The type checks this crate owns: argument counts, argument types, declared
/// function return types, `LET` annotations, and arithmetic operands.
///
/// `let-type` and `operator-type` are here because a false positive in either
/// would otherwise be structurally invisible to this sweep — the one test that
/// reads real-world SurrealQL at scale.
fn argument_diagnostics(source: &str) -> Vec<(String, String)> {
    diagnostics_for(source)
        .into_iter()
        .filter_map(|diagnostic| match &diagnostic.code {
            Some(NumberOrString::String(code))
                if code.starts_with("argument-")
                    || code == "return-type"
                    || code == "let-type"
                    || code == "operator-type"
                    || code == "unknown-method" =>
            {
                Some((code.clone(), diagnostic.message.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn valid_builtin_calls_stay_silent() {
    let fixture = include_str!("fixtures/builtin_calls_valid.surql");

    // One statement at a time, so a failure names the offending call rather
    // than the whole file.
    let mut offenders = Vec::new();
    for line in fixture.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("--") {
            continue;
        }
        for (code, message) in argument_diagnostics(line) {
            offenders.push(format!("{line}\n    → {code}: {message}"));
        }
    }

    assert!(
        offenders.is_empty(),
        "{} of SurrealDB's own valid calls were flagged:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}

#[test]
fn the_fixture_still_covers_the_breadth_it_was_built_for() {
    // Guards against the fixture being trimmed until it proves nothing.
    let fixture = include_str!("fixtures/builtin_calls_valid.surql");
    let calls = fixture
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with("--")
        })
        .count();
    let namespaces: BTreeSet<&str> = fixture
        .lines()
        .filter_map(|line| line.trim().strip_prefix("-- "))
        .filter_map(|label| label.strip_suffix("::"))
        .collect();

    assert!(calls >= 120, "only {calls} calls left in the fixture");
    assert!(
        namespaces.len() >= 18,
        "only {} namespaces left: {namespaces:?}",
        namespaces.len()
    );
}

/// The SurrealDB corpus, when this machine has a checkout.
///
/// `SURREALDB_DIR` first, then the sibling layout the grammar already uses.
/// `None` in continuous integration, where the sweep skips.
fn corpus_dir() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("SURREALDB_DIR").map(PathBuf::from),
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../surrealdb")),
    ];
    candidates
        .into_iter()
        .flatten()
        .map(|path| path.join("language-tests/tests"))
        .find(|tests| tests.is_dir())
}

fn surql_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            surql_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("surql") {
            out.push(path);
        }
    }
}

/// Every `(file, code)` pair the sweep is allowed to report.
///
/// Each one is a call SurrealDB itself rejects: every file listed declares an
/// `error =` expectation in its own front matter, and the wording matches —
/// `array::add`'s reads `Incorrect arguments for function array::add(). Expected
/// 2 arguments`, the same defect in the same words.
///
/// The assertion is on the exact set rather than a count, so a new false
/// positive fails here instead of blending into a total. Every file *not* listed
/// is code SurrealDB accepts and the checks must stay silent on.
const EXPECTED: &[(&str, &str)] = &[
    ("language/coerce/regex.surql", "argument-type"),
    ("language/functions/array/add.surql", "argument-count"),
    ("language/functions/array/add.surql", "argument-type"),
    ("language/functions/array/any.surql", "argument-type"),
    ("language/functions/array/append.surql", "argument-type"),
    ("language/functions/array/at.surql", "argument-type"),
    ("language/functions/array/combine.surql", "argument-type"),
    (
        "language/functions/array/complement.surql",
        "argument-count",
    ),
    ("language/functions/array/complement.surql", "argument-type"),
    ("language/functions/array/concat.surql", "argument-type"),
    (
        "language/functions/array/difference.surql",
        "argument-count",
    ),
    ("language/functions/array/difference.surql", "argument-type"),
    ("language/functions/array/distinct.surql", "argument-type"),
    ("language/functions/array/first.surql", "argument-type"),
    ("language/functions/array/flatten.surql", "argument-type"),
    ("language/functions/array/group.surql", "argument-type"),
    ("language/functions/array/insert.surql", "argument-count"),
    ("language/functions/array/intersect.surql", "argument-count"),
    ("language/functions/array/intersect.surql", "argument-type"),
    ("language/functions/array/is_empty.surql", "argument-type"),
    ("language/functions/array/join.surql", "argument-count"),
    ("language/functions/array/len.surql", "argument-type"),
    ("language/functions/array/max.surql", "argument-type"),
    ("language/functions/array/min.surql", "argument-type"),
    ("language/functions/array/prepend.surql", "argument-type"),
    ("language/functions/array/push.surql", "argument-type"),
    ("language/functions/array/reverse.surql", "argument-type"),
    ("language/functions/array/shuffle.surql", "argument-type"),
    ("language/functions/array/slice.surql", "argument-type"),
    ("language/functions/array/sort.surql", "argument-type"),
    ("language/functions/array/sort_asc.surql", "argument-count"),
    ("language/functions/array/sort_desc.surql", "argument-count"),
    ("language/functions/array/union.surql", "argument-type"),
    ("language/functions/bytes/len.surql", "argument-type"),
    ("language/functions/object/entries.surql", "argument-type"),
    ("language/functions/object/extend.surql", "argument-type"),
    ("language/functions/parse/url/domain.surql", "argument-type"),
    ("language/functions/parse/url/host.surql", "argument-type"),
    ("language/functions/parse/url/path.surql", "argument-type"),
    ("language/functions/parse/url/scheme.surql", "argument-type"),
    ("language/functions/set/add.surql", "argument-type"),
    ("language/functions/set/any.surql", "argument-type"),
    ("language/functions/set/complement.surql", "argument-type"),
    (
        "language/functions/set/complex_values.surql",
        "argument-type",
    ),
    ("language/functions/set/contains.surql", "argument-type"),
    ("language/functions/set/difference.surql", "argument-type"),
    ("language/functions/set/intersect.surql", "argument-type"),
    ("language/functions/set/is_empty.surql", "argument-type"),
    ("language/functions/set/len.surql", "argument-type"),
    ("language/functions/set/remove.surql", "argument-type"),
    ("language/functions/set/union.surql", "argument-type"),
    (
        "language/statements/define/function/custom_optional_args.surql",
        "argument-count",
    ),
    // Both declare `error = "Tried to set `$bar`, but couldn't coerce value:
    // Expected `int` but found `'hello'`"`, which is this check in the engine's
    // own words.
    ("language/statements/let/typed.surql", "let-type"),
    (
        "language/statements/let/typed_let_in_block.surql",
        "let-type",
    ),
    // Arithmetic on an operand pair the engine has no arm for. Every one of
    // these files declares the matching `error = "Cannot perform …"` or
    // `error = "Cannot raise …"` in its own front matter.
    (
        "language/primitive/array/arithmic_operations.surql",
        "operator-type",
    ),
    (
        "language/primitive/duration/arithmatic_operations.surql",
        "operator-type",
    ),
    (
        "language/primitive/set/set_array_common_behaviour.surql",
        "operator-type",
    ),
    // `1 + "1"`, declared as `error = true`.
    ("self_tests/multi_line.surql", "operator-type"),
];

/// The exhaustive oracle. Ignored by default because it re-analyses ~1,900
/// documents and takes about two minutes, which does not belong in a suite that
/// otherwise finishes in under a second.
///
/// Run it whenever the catalogue, the arity model, or an argument check changes:
///
/// ```bash
/// cargo test --test conformance -- --ignored --nocapture
/// ```
#[test]
#[ignore = "sweeps the whole SurrealDB corpus; ~2 minutes"]
fn the_surrealdb_corpus_produces_only_expected_diagnostics() {
    let Some(corpus) = corpus_dir() else {
        eprintln!("skipping: no SurrealDB checkout. Set SURREALDB_DIR to run this sweep.");
        return;
    };

    let mut files = Vec::new();
    surql_files(&corpus, &mut files);
    files.sort();
    assert!(
        files.len() > 1500,
        "expected the full corpus, found {} files",
        files.len()
    );

    let mut found: BTreeSet<(String, String)> = BTreeSet::new();
    let mut detail: Vec<String> = Vec::new();
    for file in &files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        // The harness keeps its expectations in a leading TOML block comment.
        let body = match source.split_once("*/") {
            Some((head, tail)) if head.trim_start().starts_with("/**") => tail,
            _ => source.as_str(),
        };
        let relative = file
            .strip_prefix(&corpus)
            .unwrap_or(file)
            .to_string_lossy()
            .to_string();
        for (code, message) in argument_diagnostics(body) {
            found.insert((relative.clone(), code.clone()));
            detail.push(format!("{relative} :: {code} :: {message}"));
        }
    }

    let expected: BTreeSet<(String, String)> = EXPECTED
        .iter()
        .map(|(file, code)| ((*file).to_string(), (*code).to_string()))
        .collect();

    let unexpected: Vec<&(String, String)> = found.difference(&expected).collect();
    assert!(
        unexpected.is_empty(),
        "the checks fired on {} file(s) not in the expected set:\n{}",
        unexpected.len(),
        detail
            .iter()
            .filter(|line| unexpected
                .iter()
                .any(|(file, code)| line.starts_with(&format!("{file} :: {code}"))))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );

    // The other direction: a check that silently stops working should also fail.
    let missing: Vec<&(String, String)> = expected.difference(&found).collect();
    assert!(
        missing.is_empty(),
        "these known-bad calls are no longer reported: {missing:?}"
    );
}
