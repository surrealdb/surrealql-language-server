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
use surrealql_language_server::semantic::pipeline::diagnostics_for_document;
use surrealql_language_server::semantic::types::{
    MergedSemanticModel, SymbolOrigin, WorkspaceIndex,
};
use tower_lsp_server::ls_types::{Diagnostic, NumberOrString, Uri};

fn uri(path: &str) -> Uri {
    format!("file:///workspace/{path}").parse().expect("uri")
}

/// Every diagnostic this sweep judges for one document.
///
/// The semantic diagnostics, plus the one syntax diagnostic this crate reasons
/// about rather than merely relays: `unknown-type`. The other syntax codes are
/// left out on purpose — `parse` reports whatever the tree-sitter grammar cannot
/// read, and it cannot read a fair amount of valid SurrealQL: 603 of the 1,897
/// corpus files carry a `parse` diagnostic, across at least seven systematic
/// shapes (`EXPLAIN` alone accounts for 390 occurrences). Those are grammar
/// defects tracked in `docs/grammar-gaps.md`, and pulling them in would bury a
/// real regression in known noise.
///
/// CAUTION: the corollary is that this sweep is blind to grammar false
/// positives. It cannot tell you that valid SurrealQL stopped parsing, only that
/// a semantic rule changed its mind. The permission-group comma was a false
/// `parse` error on a form SurrealDB's own tests use throughout, and this test
/// passed for its whole life. Measure the syntax pass directly for that.
fn diagnostics_for(source: &str) -> Vec<Diagnostic> {
    let Some(analysis) = analyze_document(uri("q.surql"), source, SymbolOrigin::Local) else {
        return Vec::new();
    };
    let mut workspace = WorkspaceIndex::default();
    workspace
        .documents
        .insert(uri("q.surql"), std::sync::Arc::new(analysis.clone()));
    let model = MergedSemanticModel::build(&workspace, &Default::default());
    let mut diagnostics = diagnostics_for_document(&analysis, &model, &ServerSettings::default());
    // `parse` is dropped *after* the pipeline rather than never collected, so
    // this sweep judges exactly what the server emits, minus the one code it
    // deliberately does not reason about.
    diagnostics
        .retain(|diagnostic| diagnostic.code != Some(NumberOrString::String("parse".to_string())));
    diagnostics
}

/// The type checks this crate owns: argument counts, argument types, declared
/// function return types, `LET` annotations, arithmetic operands, and type names.
///
/// `let-type` and `operator-type` are here because a false positive in either
/// would otherwise be structurally invisible to this sweep — the one test that
/// reads real-world SurrealQL at scale. `unknown-type` is here for the same
/// reason, and it needs the sweep more than most: it fires on a closed keyword
/// list, so a name SurrealDB adds in a later release shows up here first.
fn argument_diagnostics(source: &str) -> Vec<(String, String)> {
    diagnostics_for(source)
        .into_iter()
        .filter_map(|diagnostic| match &diagnostic.code {
            Some(NumberOrString::String(code))
                if code.starts_with("argument-")
                    || code == "return-type"
                    || code == "let-type"
                    || code == "operator-type"
                    || code == "unknown-method"
                    || code == "unknown-type"
                    || code == "field-type" =>
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
    // `math::top([[], {}], 2)` / `math::bottom(…)` against `array<number>`. Both
    // files declare it: "Expected `number` but found `[]` when coercing element
    // at index 0 of `array<number>`".
    ("language/functions/math/top.surql", "argument-type"),
    ("language/functions/math/bottom.surql", "argument-type"),
    // `DEFINE FIELD … TYPE T DEFAULT/VALUE <value>` where the value cannot
    // coerce to `T`. Each file declares the engine's own refusal:
    //   `TYPE string DEFAULT 0`   -> "Expected `string` but found `0`"
    //   `TYPE int DEFAULT 'notanint'` -> "Expected `int` but found `'notanint'`"
    //   `TYPE record<layout> VALUE type::string($value)`
    //       -> "Expected `record<layout>` but found `'layout:one'`"
    // The last is a bug reproduction whose own comment says it "should error at
    // write time" — the server now catches it before the write.
    (
        "language/statements/define/field/default_value_does_not_match_type.surql",
        "field-type",
    ),
    (
        "language/statements/define/field/id_default.surql",
        "field-type",
    ),
    (
        "reproductions/value_clause_type_validation.surql",
        "field-type",
    ),
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

// ---------------------------------------------------------------------------
// Formatter
// ---------------------------------------------------------------------------

use surrealql_language_server::format;

/// Whether the document holds a `parse` diagnostic — i.e. the formatter would
/// refuse it.
fn diagnostics_for_with_parse(source: &str) -> bool {
    let Some(analysis) = analyze_document(uri("q.surql"), source, SymbolOrigin::Local) else {
        return true;
    };
    analysis.tree.root_node().has_error()
}

/// Count the comment characters, as a proxy for "no comment was lost".
fn comment_bytes(source: &str) -> usize {
    source
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            ["--", "//", "#"]
                .iter()
                .find_map(|opener| trimmed.strip_prefix(opener))
        })
        .map(|body| body.trim().len())
        .sum()
}

/// Format each committed fixture and check the three properties that make a
/// formatter safe to run on save: it does not change meaning, it does not lose
/// a comment, and running it twice changes nothing.
#[test]
fn the_formatter_is_safe_on_the_committed_fixtures() {
    for name in [
        "builtin_calls_valid.surql",
        "method_syntax.surql",
        "adversarial.surql",
    ] {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let source = std::fs::read_to_string(&path).expect("fixture");
        let formatted = format::format(&source);

        // `adversarial.surql` deliberately contains constructs the pinned
        // grammar cannot parse, and the formatter refuses those whole. That is
        // the designed behaviour, so an unchanged result is a pass.
        if formatted == source {
            continue;
        }

        assert_eq!(
            format::format(&formatted),
            formatted,
            "{name}: formatting is not idempotent"
        );
        assert!(
            comment_bytes(&formatted) >= comment_bytes(&source),
            "{name}: comment text was lost"
        );

        let before = diagnostics_for(&source).len();
        let after = diagnostics_for(&formatted).len();
        assert_eq!(
            after, before,
            "{name}: formatting changed the diagnostic count"
        );
    }
}

/// The same three properties across SurrealDB's whole corpus. Ignored for the
/// same reason the diagnostic sweep is: it needs a SurrealDB checkout and takes
/// a few seconds.
#[test]
#[ignore = "sweeps the whole SurrealDB corpus"]
fn the_formatter_is_safe_on_the_surrealdb_corpus() {
    let Some(root) = corpus_dir() else {
        eprintln!("no SurrealDB checkout; skipping");
        return;
    };
    let mut files = Vec::new();
    surql_files(&root, &mut files);
    let mut checked = 0usize;
    let mut refused = 0usize;
    let mut already = 0usize;
    for path in files {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let formatted = format::format(&source);
        if formatted == source {
            // Either the file is already in canonical form, or the formatter
            // refused it because it does not parse. The two are worth telling
            // apart: a high refusal count means the grammar, not the formatter,
            // is the limit.
            if diagnostics_for_with_parse(&source) {
                refused += 1;
            } else {
                already += 1;
            }
            continue;
        }
        checked += 1;
        assert_eq!(
            format::format(&formatted),
            formatted,
            "{}: not idempotent",
            path.display()
        );
        assert_eq!(
            diagnostics_for(&formatted).len(),
            diagnostics_for(&source).len(),
            "{}: formatting changed the diagnostic count",
            path.display()
        );
    }
    println!("formatted {checked}; already canonical {already}; refused (unparseable) {refused}");
    assert!(checked > 100, "the sweep must actually reach files");
}
