//! Latency benchmarks for the operations an editor drives on every edit.
//!
//! Seven operations, each measured at three input sizes so a quadratic term
//! shows up as a rising cost-per-line rather than a single slow number. The
//! targets in `docs/perf-plan.md` are asserted at the end: the harness exits
//! non-zero when one regresses, so CI can gate on it.
//!
//! Run with `cargo bench`. There is no `criterion` dependency on purpose —
//! the dependency graph is shared with the `wasm32` target and the published
//! crate, and a wall-clock loop is enough to separate 20 ms from 3800 ms.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ls_types::{Position, Range, Uri};
use surrealql_language_server::config::ServerSettings;
use surrealql_language_server::semantic::analyzer::analyze_document;
use surrealql_language_server::semantic::highlight;
use surrealql_language_server::semantic::text::{LineIndex, token_at, word_range};
use surrealql_language_server::semantic::types::{
    MergedSemanticModel, SymbolOrigin, WorkspaceIndex,
};

/// Line sizes every document-shaped benchmark runs at. The ratios matter more
/// than the absolute numbers: 200 → 800 → 3200 is two clean doublings-of-four,
/// so a linear pass holds its cost-per-line and a quadratic one quadruples it.
const LINE_SIZES: [usize; 3] = [200, 800, 3200];

/// Workspace sizes for the model-shaped benchmarks.
const DOC_COUNTS: [usize; 3] = [10, 50, 200];

fn uri(n: usize) -> Uri {
    format!("file:///bench/doc{n}.surql")
        .parse()
        .expect("valid uri")
}

/// One SELECT per line: the shape that produces one query fact per line, which
/// is what drives the per-symbol position conversions.
fn query_doc(lines: usize) -> String {
    "SELECT name, email, age FROM person WHERE age > 21;\n".repeat(lines)
}

/// A schema file: four tables with five fields each per group.
fn schema_doc(index: usize, tables: usize) -> String {
    let mut out = String::new();
    for t in 0..tables {
        out.push_str(&format!(
            "DEFINE TABLE t{index}_{t} SCHEMAFULL;\n\
             DEFINE FIELD name ON t{index}_{t} TYPE string;\n\
             DEFINE FIELD email ON t{index}_{t} TYPE string;\n\
             DEFINE FIELD age ON t{index}_{t} TYPE int;\n\
             DEFINE FIELD tags ON t{index}_{t} TYPE array<string>;\n\
             SELECT name, age FROM t{index}_{t} WHERE age > 21;\n"
        ));
    }
    out
}

/// A schema document: one `DEFINE FIELD` per line with a `TYPE` clause and no
/// `COMMENT` clause.
///
/// This is the shape the all-SELECT rows miss entirely, and it drives two paths
/// they never reach: the leading-comment lookup (which runs for every DEFINE
/// without a `COMMENT` clause) and the `TypeName` check in the syntax pass.
fn schema_only_doc(lines: usize) -> String {
    let mut out = String::from("DEFINE TABLE person SCHEMAFULL;\n");
    for i in 0..lines {
        out.push_str(&format!("DEFINE FIELD f{i} ON person TYPE string;\n"));
    }
    out
}

fn workspace(docs: usize, tables_per_doc: usize) -> WorkspaceIndex {
    let mut index = WorkspaceIndex::default();
    for d in 0..docs {
        let text = schema_doc(d, tables_per_doc);
        let analysis = analyze_document(uri(d), &text, SymbolOrigin::Local).expect("parses");
        index.documents.insert(uri(d), Arc::new(analysis));
    }
    index
}

/// A workspace where one document declares the schema and the rest only query
/// it, so most merge candidates lose.
///
/// `workspace` gives every document its own table names, so every candidate
/// wins the merge and the clone-on-win path is never exercised. Repeating an
/// identical document would not help either: `should_replace_table` compares
/// with `>=`, so a tie still replaces.
///
/// A candidate only loses when it scores strictly lower, and the ordinary way
/// that happens is provenance. A query over an undeclared name *infers* a
/// table and its fields, and an inferred definition (score 206) loses to the
/// explicit one (score 1410). One schema file plus the queries that use it is
/// the common workspace shape, and it is also what `INFO FOR DB` produces when
/// it mirrors local names back from the engine.
fn overlapping_workspace(docs: usize) -> WorkspaceIndex {
    let mut index = WorkspaceIndex::default();
    // One document declares four tables with five fields each.
    let schema = schema_doc(0, 4);
    let analysis = analyze_document(uri(0), &schema, SymbolOrigin::Local).expect("parses");
    index.documents.insert(uri(0), Arc::new(analysis));

    // Every other document only reads those tables. Each one contributes an
    // inferred table and three inferred fields per table, and all of them lose.
    let mut queries = String::new();
    for t in 0..4 {
        queries.push_str(&format!(
            "SELECT name, email, age FROM t0_{t} WHERE age > 21;\n"
        ));
    }
    for d in 1..docs {
        let analysis = analyze_document(uri(d), &queries, SymbolOrigin::Local).expect("parses");
        index.documents.insert(uri(d), Arc::new(analysis));
    }
    index
}

/// A document whose queries target tables that no `DEFINE TABLE` declares.
///
/// This shape is what exercises the workspace-wide scans in the diagnostics
/// pass: an undeclared target is *inferred*, and the inferred branch calls
/// `target_usage_count` over every query fact in the workspace and then runs
/// `jaro_winkler` against every table. A document that declares the tables it
/// queries never reaches that branch, so a benchmark built from `schema_doc`
/// alone reports a diagnostics cost that looks flat when it is not.
fn undeclared_target_doc(index: usize) -> String {
    let mut out = String::new();
    for t in 0..20 {
        out.push_str(&format!(
            "SELECT name FROM undeclared_d{index}_t{t} WHERE age > 1;\n"
        ));
    }
    // Some explicit tables, so the near-miss candidate set is not empty.
    for t in 0..5 {
        out.push_str(&format!("DEFINE TABLE real_d{index}_t{t} SCHEMAFULL;\n"));
    }
    out
}

fn undeclared_workspace(docs: usize) -> WorkspaceIndex {
    let mut index = WorkspaceIndex::default();
    for d in 0..docs {
        let text = undeclared_target_doc(d);
        let analysis = analyze_document(uri(d), &text, SymbolOrigin::Local).expect("parses");
        index.documents.insert(uri(d), Arc::new(analysis));
    }
    index
}

/// Time `body` enough times to get a stable number without a long wall-clock.
/// Returns the mean milliseconds per iteration.
fn time<F: FnMut()>(mut body: F) -> f64 {
    // One warm-up pass so a first-call allocation does not land in the mean.
    body();
    let budget = Duration::from_millis(400);
    let start = Instant::now();
    let mut runs = 0u32;
    // The cap has to be high enough that a sub-microsecond operation still
    // accumulates a measurable total: the cursor helpers went from 2.4 ms to
    // well under 1 us, and 200 runs of that is below the clock's resolution.
    while start.elapsed() < budget && runs < 200_000 {
        body();
        runs += 1;
    }
    if runs == 0 {
        return start.elapsed().as_secs_f64() * 1000.0;
    }
    start.elapsed().as_secs_f64() * 1000.0 / f64::from(runs)
}

/// A recorded result plus the target it must meet.
struct Measured {
    name: &'static str,
    scale: String,
    ms: f64,
    target_ms: Option<f64>,
}

fn main() {
    let mut results: Vec<Measured> = Vec::new();

    // ── 1. Full-document semantic tokens ────────────────────────────────
    println!("\n== semantic_tokens_full ==");
    for lines in LINE_SIZES {
        let text = query_doc(lines);
        let analysis = analyze_document(uri(0), &text, SymbolOrigin::Local).expect("parses");
        let ms = time(|| {
            std::hint::black_box(highlight::collect_semantic_tokens(
                &analysis.tree,
                &analysis.text,
                &analysis.line_index,
            ));
        });
        report(lines, text.len(), ms);
        results.push(Measured {
            name: "semantic_tokens_full",
            scale: format!("{lines} lines"),
            ms,
            // The plan's target is stated for 4000 lines; 3200 is the
            // benchmark size, so scale the budget down proportionally.
            target_ms: (lines == 3200).then_some(16.0),
        });
    }

    // ── 2. analyze_document ─────────────────────────────────────────────
    println!("\n== analyze_document ==");
    for lines in LINE_SIZES {
        let text = query_doc(lines);
        // Constructed outside the loop: `uri()` parses a URI, which is not
        // part of what this row measures.
        let doc_uri = uri(0);
        let ms = time(|| {
            std::hint::black_box(analyze_document(
                doc_uri.clone(),
                &text,
                SymbolOrigin::Local,
            ));
        });
        report(lines, text.len(), ms);
        results.push(Measured {
            name: "analyze_document",
            scale: format!("{lines} lines"),
            ms,
            // Raised from 60 ms, deliberately, when field checking grew to
            // cover projections and `WHERE` clauses. That check is what makes
            // `SELECT prson_name FROM person` report at all, and its cost is
            // one extra traversal of each statement — about 8 ms on this
            // document, which is 3200 consecutive SELECTs with four field
            // references each, the worst case for the feature by construction.
            //
            // Everything accidental was removed first, measured at each step:
            // a duplicate walk for assignment names, a quadratic outline
            // nesting pass, an eager 12,801-entry reference index that only
            // user-initiated requests read, and two unconditional tree walks
            // now guarded by a substring test. That took the figure from
            // 129 ms to 67 ms. Reaching 60 again needs the field-reference
            // collection folded into `collect_statements` so there is one
            // traversal rather than two — a real change, not a tuning pass.
            //
            // The schema-shaped document below still clears 60 ms.
            target_ms: (lines == 3200).then_some(75.0),
        });
    }

    // ── 2b. analyze_document on a DEFINE-heavy document ─────────────────
    println!("\n== analyze_document (schema shape: one DEFINE FIELD per line) ==");
    for lines in LINE_SIZES {
        let text = schema_only_doc(lines);
        let doc_uri = uri(0);
        let ms = time(|| {
            std::hint::black_box(analyze_document(
                doc_uri.clone(),
                &text,
                SymbolOrigin::Local,
            ));
        });
        report(lines, text.len(), ms);
        results.push(Measured {
            name: "analyze_document/schema",
            scale: format!("{lines} lines"),
            ms,
            target_ms: (lines == 3200).then_some(60.0),
        });
    }

    // ── 3. Ranged semantic tokens (a 40-line viewport) ──────────────────
    println!("\n== semantic_tokens_range (40-line viewport) ==");
    for lines in LINE_SIZES {
        let text = query_doc(lines);
        let analysis = analyze_document(uri(0), &text, SymbolOrigin::Local).expect("parses");
        let range = Range::new(Position::new(0, 0), Position::new(40, 0));
        let ms = time(|| {
            std::hint::black_box(highlight::collect_semantic_tokens_range(
                &analysis.tree,
                &analysis.text,
                &analysis.line_index,
                range,
            ));
        });
        report(lines, text.len(), ms);
        results.push(Measured {
            name: "semantic_tokens_range",
            scale: format!("{lines} lines"),
            ms,
            target_ms: (lines == 3200).then_some(5.0),
        });
    }

    // ── 4. Cursor helpers at the end of the document ────────────────────
    println!("\n== token_at + word_range (cursor at end of file) ==");
    for lines in LINE_SIZES {
        let text = query_doc(lines);
        let pos = Position::new(lines as u32 - 1, 9);
        // Built outside the timed loop because the server caches it on the
        // document analysis: a request pays the lookup, not the construction.
        let index = LineIndex::new(&text);
        let ms = time(|| {
            std::hint::black_box(token_at(&text, &index, pos));
            std::hint::black_box(word_range(&text, &index, pos));
        });
        report(lines, text.len(), ms);
        results.push(Measured {
            name: "token_at + word_range",
            scale: format!("{lines} lines"),
            ms,
            target_ms: (lines == 3200).then_some(1.0),
        });
    }

    // ── 5. Table completion items ───────────────────────────────────────
    println!("\n== table_completion_items(\"\") ==");
    for docs in DOC_COUNTS {
        let ws = workspace(docs, 4);
        let model = MergedSemanticModel::build(&ws, &Default::default());
        let tables = model.table_completion_items("", None).len();
        let ms = time(|| {
            std::hint::black_box(model.table_completion_items("", None));
        });
        println!("  {docs:>4} docs ({tables:>4} tables): {ms:>9.4} ms");
        results.push(Measured {
            name: "table_completion_items",
            scale: format!("{tables} tables"),
            ms,
            target_ms: (docs == 200).then_some(2.0),
        });
    }

    // ── 6. Semantic diagnostics for one document ────────────────────────
    // Two corpora. The declared one is the common case. The undeclared one
    // reaches the inferred-target branch, which is the only path that scans
    // the whole workspace per fact — see `undeclared_target_doc`.
    println!("\n== semantic_diagnostics (one document, declared targets) ==");
    for docs in DOC_COUNTS {
        let ws = workspace(docs, 4);
        let model = MergedSemanticModel::build(&ws, &Default::default());
        let settings = ServerSettings::default();
        let analysis = Arc::clone(ws.documents.get(&uri(0)).expect("present"));
        let ms = time(|| {
            std::hint::black_box(model.semantic_diagnostics(analysis.as_ref(), &settings));
        });
        println!("  {docs:>4} docs: {ms:>9.3} ms");
        results.push(Measured {
            name: "semantic_diagnostics",
            scale: format!("{docs} docs"),
            ms,
            target_ms: (docs == 200).then_some(1.0),
        });
    }

    println!("\n== semantic_diagnostics (one document, undeclared targets) ==");
    for docs in DOC_COUNTS {
        let ws = undeclared_workspace(docs);
        let model = MergedSemanticModel::build(&ws, &Default::default());
        let settings = ServerSettings::default();
        let analysis = Arc::clone(ws.documents.get(&uri(0)).expect("present"));
        let ms = time(|| {
            std::hint::black_box(model.semantic_diagnostics(analysis.as_ref(), &settings));
        });
        println!("  {:>4} docs ({:>5} facts): {ms:>9.3} ms", docs, docs * 20);
        results.push(Measured {
            name: "semantic_diagnostics/inferred",
            scale: format!("{docs} docs"),
            ms,
            target_ms: (docs == 200).then_some(1.0),
        });
    }

    // ── 7. Merged model rebuild (the per-keystroke cost) ────────────────
    println!("\n== MergedSemanticModel::build (per keystroke today) ==");
    for docs in DOC_COUNTS {
        let ws = workspace(docs, 4);
        let ms = time(|| {
            std::hint::black_box(MergedSemanticModel::build(&ws, &Default::default()));
        });
        println!("  {docs:>4} docs: {ms:>9.3} ms");
        results.push(Measured {
            name: "model_build",
            scale: format!("{docs} docs"),
            ms,
            target_ms: None,
        });
    }

    // ── 8. Merged model rebuild over overlapping definitions ────────────
    // The row above gives every document its own tables, so every candidate
    // wins the merge. This one repeats one document's definitions, so all but
    // the first copy lose — the case where cloning before the merge decides
    // is pure waste. See `overlapping_workspace`.
    println!("\n== MergedSemanticModel::build (overlapping definitions) ==");
    for docs in DOC_COUNTS {
        let ws = overlapping_workspace(docs);
        let ms = time(|| {
            std::hint::black_box(MergedSemanticModel::build(&ws, &Default::default()));
        });
        println!("  {docs:>4} docs: {ms:>9.3} ms");
        results.push(Measured {
            name: "model_build/overlap",
            scale: format!("{docs} docs"),
            ms,
            target_ms: None,
        });
    }

    summarize(&results);
}

fn report(lines: usize, bytes: usize, ms: f64) {
    let per_line = ms * 1000.0 / lines as f64;
    println!("  {lines:>5} lines ({bytes:>7} B): {ms:>9.4} ms  ({per_line:>7.2} us/line)");
}

/// Print the target table and exit non-zero when a target is missed, so the
/// benchmark can gate a pull request.
fn summarize(results: &[Measured]) {
    println!("\n== targets ==");
    let mut failed = 0;
    for r in results {
        let Some(target) = r.target_ms else { continue };
        let ok = r.ms <= target;
        if !ok {
            failed += 1;
        }
        println!(
            "  [{}] {:<24} {:>12}  {:>9.3} ms  (target {:.1} ms)",
            if ok { "PASS" } else { "FAIL" },
            r.name,
            r.scale,
            r.ms,
            target
        );
    }
    if failed > 0 {
        println!("\n{failed} target(s) missed.");
        std::process::exit(1);
    }
    println!("\nAll targets met.");
}
