//! Marker tests: one file per scenario, source and expectations together.
//!
//! Borrowed from gopls, which packs the source, the settings and the expected
//! output into a single archive per case. The point is that adding a test means
//! adding a data file, not writing another harness — which is what makes
//! rust-analyzer's and gopls's suites cheap to extend.
//!
//! Format. A file in `tests/marker/` is SurrealQL, with two kinds of line:
//!
//! ```text
//! --@ settings: { "analysis": { "schemalessDiagnostics": "strict" } }
//! DEFINE TABLE person SCHEMAFULL;
//! SELECT prson FROM person;
//! --@ diagnostic: 2 unknown-field
//! ```
//!
//! `--@ settings:` takes one JSON object, merged over the defaults. `--@
//! diagnostic:` takes a zero-based line number and a rule id, and asserts that
//! the rule reports on that line. The assertions together are exhaustive: a
//! diagnostic the file does not claim fails the test, which is what stops a
//! case from quietly asserting less than it looks like it does.
//!
//! Marker lines are comments, so a case file is also valid SurrealQL and can be
//! opened in an editor to see what the server does with it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use surrealql_language_server::config::ServerSettings;
use surrealql_language_server::semantic::analyzer::analyze_document;
use surrealql_language_server::semantic::pipeline::diagnostics_for_document;
use surrealql_language_server::semantic::types::{
    MergedSemanticModel, SymbolOrigin, WorkspaceIndex,
};
use tower_lsp_server::ls_types::{NumberOrString, Uri};

const MARKER: &str = "--@";

struct Case {
    name: String,
    source: String,
    settings: ServerSettings,
    expected: BTreeSet<(u32, String)>,
}

fn parse_case(path: &Path) -> Case {
    let text = std::fs::read_to_string(path).expect("read case");
    let mut settings_json: Option<serde_json::Value> = None;
    let mut expected = BTreeSet::new();

    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix(MARKER) else {
            continue;
        };
        let rest = rest.trim();
        if let Some(body) = rest.strip_prefix("settings:") {
            settings_json =
                Some(serde_json::from_str(body.trim()).unwrap_or_else(|error| {
                    panic!("{}: bad settings JSON: {error}", path.display())
                }));
        } else if let Some(body) = rest.strip_prefix("diagnostic:") {
            let mut parts = body.split_whitespace();
            let line_number: u32 = parts
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| panic!("{}: diagnostic needs a line number", path.display()));
            let rule = parts
                .next()
                .unwrap_or_else(|| panic!("{}: diagnostic needs a rule id", path.display()));
            expected.insert((line_number, rule.to_string()));
        } else {
            panic!("{}: unknown marker `{rest}`", path.display());
        }
    }

    let settings = match settings_json {
        Some(value) => {
            ServerSettings::from_sources(Some(&serde_json::json!({ "surrealql": value })), None)
        }
        None => ServerSettings::default(),
    };

    Case {
        name: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("?")
            .to_string(),
        source: text,
        settings,
        expected,
    }
}

fn actual(case: &Case) -> BTreeSet<(u32, String)> {
    let uri: Uri = "file:///marker.surql".parse().expect("uri");
    let Some(analysis) = analyze_document(uri.clone(), &case.source, SymbolOrigin::Local) else {
        return BTreeSet::new();
    };
    let mut workspace = WorkspaceIndex::default();
    workspace
        .documents
        .insert(uri, std::sync::Arc::new(analysis.clone()));
    let model = MergedSemanticModel::build(&workspace, &Default::default());
    diagnostics_for_document(&analysis, &model, &case.settings)
        .into_iter()
        .filter_map(|diagnostic| match diagnostic.code {
            Some(NumberOrString::String(code)) => Some((diagnostic.range.start.line, code)),
            _ => None,
        })
        .collect()
}

fn cases() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/marker");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("surql"))
        .collect();
    found.sort();
    found
}

#[test]
fn every_marker_case_matches() {
    let cases = cases();
    assert!(!cases.is_empty(), "no marker cases found");
    for path in cases {
        let case = parse_case(&path);
        let actual = actual(&case);
        assert_eq!(
            actual, case.expected,
            "{}: diagnostics differ\n  expected {:?}\n  actual   {:?}",
            case.name, case.expected, actual
        );
    }
}
