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
use surrealql_language_server::semantic::text::{token_at, word_range};
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

fn workspace(docs: usize, tables_per_doc: usize) -> WorkspaceIndex {
    let mut index = WorkspaceIndex::default();
    for d in 0..docs {
        let text = schema_doc(d, tables_per_doc);
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
    while start.elapsed() < budget && runs < 200 {
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
        let ms = time(|| {
            std::hint::black_box(analyze_document(uri(0), &text, SymbolOrigin::Local));
        });
        report(lines, text.len(), ms);
        results.push(Measured {
            name: "analyze_document",
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
        let ms = time(|| {
            std::hint::black_box(token_at(&text, pos));
            std::hint::black_box(word_range(&text, pos));
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
        println!("  {docs:>4} docs ({tables:>4} tables): {ms:>9.3} ms");
        results.push(Measured {
            name: "table_completion_items",
            scale: format!("{tables} tables"),
            ms,
            target_ms: (docs == 200).then_some(2.0),
        });
    }

    // ── 6. Semantic diagnostics for one document ────────────────────────
    println!("\n== semantic_diagnostics (one document) ==");
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

    summarize(&results);
}

fn report(lines: usize, bytes: usize, ms: f64) {
    let per_line = ms * 1000.0 / lines as f64;
    println!("  {lines:>5} lines ({bytes:>7} B): {ms:>9.3} ms  ({per_line:>7.2} us/line)");
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
