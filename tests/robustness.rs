//! The analyzer must not panic on anything.
//!
//! `panic = 'abort'` in the release profile means one panic ends the session
//! with no trace beyond the stderr hook, and every input here is something a
//! user can produce by typing. There is no `cargo-fuzz` target because CI has
//! no fuzzing step and an unrun fuzzer proves nothing; this is a deterministic
//! sweep that runs in the normal suite instead.
//!
//! Deterministic on purpose. A seeded generator that changes between runs turns
//! a real failure into one nobody can reproduce.

use surrealql_language_server::config::ServerSettings;
use surrealql_language_server::format;
use surrealql_language_server::semantic::analyzer::analyze_document;
use surrealql_language_server::semantic::pipeline::diagnostics_for_document;
use surrealql_language_server::semantic::types::{
    MergedSemanticModel, SymbolOrigin, WorkspaceIndex,
};
use tower_lsp_server::ls_types::Uri;

fn uri() -> Uri {
    "file:///fuzz.surql".parse().expect("uri")
}

/// A small deterministic pseudo-random generator. No dependency, no drift.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn pick(&mut self, items: &[&'static str]) -> &'static str {
        items[(self.next() % items.len() as u64) as usize]
    }
}

/// Fragments drawn from real SurrealQL, including several the pinned grammar
/// cannot parse. Recombining them produces text no human would write and the
/// parser has to survive.
const FRAGMENTS: &[&str] = &[
    "DEFINE TABLE person SCHEMAFULL",
    "DEFINE FIELD email ON person TYPE string",
    "DEFINE INDEX by_email ON person FIELDS email",
    "DEFINE FUNCTION fn::f($a: int) -> int",
    "SELECT * FROM person",
    "SELECT name, email AS mail FROM person WHERE age > 18",
    "CREATE person SET name = 'x'",
    "RELATE person:1->knows->person:2",
    "REMOVE TABLE person",
    "LET $x: array<string> = []",
    "RETURN string::concat('a', $x)",
    "{ RETURN 1; }",
    "|person:1..100|",
    "-- surql-ignore: unknown-field",
    "/* block */",
    "'unterminated",
    "@@@",
    "((((",
    "}}}}",
    ";",
    "\n",
    "\t",
    "TYPE RELATION IN a OUT b ENFORCED",
    "PERMISSIONS FOR select FULL",
    "$",
    "::",
    "->",
    "0..",
    "\u{1F600}",
    "récord",
];

fn generated(seed: u64, pieces: usize) -> String {
    let mut rng = Rng(seed | 1);
    let mut out = String::new();
    for _ in 0..pieces {
        out.push_str(rng.pick(FRAGMENTS));
        if rng.next().is_multiple_of(3) {
            out.push(' ');
        }
    }
    out
}

/// The whole pipeline, over 2,000 generated documents.
///
/// Any panic fails the test. So does a diagnostic whose range is reversed or
/// outside the document — an editor given one of those can misbehave in ways
/// that look like its own bug.
#[test]
fn the_pipeline_survives_generated_documents() {
    let settings = ServerSettings::default();
    for seed in 1..=2_000u64 {
        let source = generated(seed, (seed % 12) as usize + 1);
        let Some(analysis) = analyze_document(uri(), &source, SymbolOrigin::Local) else {
            continue;
        };
        let mut workspace = WorkspaceIndex::default();
        workspace
            .documents
            .insert(uri(), std::sync::Arc::new(analysis.clone()));
        let model = MergedSemanticModel::build(&workspace, &Default::default());
        let diagnostics = diagnostics_for_document(&analysis, &model, &settings);

        let lines = source.lines().count().max(1) as u32;
        for diagnostic in &diagnostics {
            let start = (
                diagnostic.range.start.line,
                diagnostic.range.start.character,
            );
            let end = (diagnostic.range.end.line, diagnostic.range.end.character);
            assert!(
                start <= end,
                "seed {seed}: reversed range {start:?}..{end:?} on {:?}",
                diagnostic.code
            );
            assert!(
                diagnostic.range.start.line <= lines,
                "seed {seed}: range past the end of a {lines}-line document"
            );
        }
    }
}

/// The formatter over the same documents. It must never produce text that stops
/// parsing, and must always be idempotent.
#[test]
fn the_formatter_survives_generated_documents() {
    for seed in 1..=2_000u64 {
        let source = generated(seed, (seed % 12) as usize + 1);
        let once = format::format(&source);
        assert_eq!(
            format::format(&once),
            once,
            "seed {seed}: formatting is not idempotent"
        );
        if once != source {
            // A changed document must still parse: the formatter refuses
            // anything it cannot read, so a change is a promise it could.
            let analysis = analyze_document(uri(), &once, SymbolOrigin::Local)
                .expect("formatted text must analyse");
            assert!(
                !analysis.tree.root_node().has_error(),
                "seed {seed}: formatting introduced a parse error"
            );
        }
    }
}

/// Every prefix of a document, as if someone typed it one character at a time.
/// Half-written SurrealQL is the normal case in an editor, not an edge case.
#[test]
fn the_pipeline_survives_every_prefix_of_a_real_document() {
    let full = "DEFINE TABLE person SCHEMAFULL;\n\
                DEFINE FIELD email ON person TYPE string;\n\
                DEFINE FUNCTION fn::greet($name: string) -> string {\n\
                    RETURN string::concat('hi ', $name);\n\
                };\n\
                SELECT email FROM person WHERE email != NONE;";
    let settings = ServerSettings::default();
    for end in 0..=full.len() {
        if !full.is_char_boundary(end) {
            continue;
        }
        let source = &full[..end];
        let Some(analysis) = analyze_document(uri(), source, SymbolOrigin::Local) else {
            continue;
        };
        let mut workspace = WorkspaceIndex::default();
        workspace
            .documents
            .insert(uri(), std::sync::Arc::new(analysis.clone()));
        let model = MergedSemanticModel::build(&workspace, &Default::default());
        let _ = diagnostics_for_document(&analysis, &model, &settings);
        let _ = format::format(source);
    }
}
