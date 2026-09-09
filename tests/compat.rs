//! Backwards-compatibility tripwires. Every assertion here pins an
//! observable surface (LSP wire shape, config parsing, diagnostic
//! identity) — a failure means a change is client-visible and must be
//! a reviewed, deliberate decision, not an accident.

mod common;

use serde_json::json;
use surrealql_language_server::config::ServerSettings;
use surrealql_language_server::semantic::analyzer::analyze_document;
use surrealql_language_server::semantic::types::SymbolOrigin;
use tower_lsp_server::ls_types::NumberOrString;

/// Exact-equality golden for the advertised capabilities. Additions
/// are allowed but must be made here consciously — capability drift is
/// how a wasm host and a native editor end up seeing different
/// servers.
#[test]
fn server_capabilities_golden() {
    let capabilities =
        serde_json::to_value(common::TestCore::server_capabilities()).expect("serializable");
    let expected = json!({
        "textDocumentSync": 1,
        "hoverProvider": true,
        "completionProvider": {
            "resolveProvider": true,
            // `>` was added deliberately, so `->` opens the completion list
            // the way `<-` already did through `<`.
            "triggerCharacters": [".", ":", "<", ">", "$", "("],
        },
        "signatureHelpProvider": {
            "triggerCharacters": ["(", ","],
            "retriggerCharacters": [","],
        },
        "definitionProvider": true,
        "referencesProvider": true,
        "documentHighlightProvider": true,
        "documentSymbolProvider": true,
        "workspaceSymbolProvider": true,
        "codeActionProvider": true,
        "renameProvider": { "prepareProvider": true },
        "workspace": {
            "workspaceFolders": {
                "supported": true,
                "changeNotifications": true,
            },
        },
        "callHierarchyProvider": true,
        "semanticTokensProvider": {
            "legend": {
                "tokenTypes": [
                    "keyword", "function", "parameter", "type",
                    "string", "number", "comment", "variable",
                ],
                "tokenModifiers": ["declaration", "defaultLibrary"],
            },
            "range": true,
            "full": true,
        },
        "inlayHintProvider": { "resolveProvider": false },
    });
    assert_eq!(
        capabilities, expected,
        "advertised capabilities changed — update this golden only for a deliberate, reviewed addition"
    );
}

/// Both ERROR and MISSING parse diagnostics carry the same stable
/// identity fields.
#[test]
fn parse_diagnostic_identity_covers_error_and_missing_nodes() {
    // "SELECT * FROM (..." produces a MISSING `)`; "@@@" produces ERROR nodes.
    for text in [
        "DEFINE TABLE @@@invalid@@@;",
        "SELECT * FROM (SELECT * FROM person;",
    ] {
        let uri = "file:///compat.surql".parse().unwrap();
        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");
        assert!(!analysis.syntax_diagnostics.is_empty());
        for diagnostic in &analysis.syntax_diagnostics {
            assert_eq!(
                diagnostic.code,
                Some(NumberOrString::String("parse".to_string())),
                "syntax diagnostics keep code `parse`: {diagnostic:?}"
            );
            assert_eq!(
                diagnostic.source.as_deref(),
                Some("surreal-language-server")
            );
        }
    }
}

/// `builtins.json` is a release artifact consumed outside this repository,
/// so its shape is wire compat: additions are fine, renames and re-encodings
/// are not. One function entry pinned exactly, plus the document's key set.
#[test]
fn builtins_json_entry_shape_golden() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("builtins.json");
    let text = std::fs::read_to_string(path).expect("builtins.json is committed");
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");

    let mut keys: Vec<&str> = value
        .as_object()
        .expect("one JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "constants",
            "functions",
            "meta",
            "namespaces",
            "receivers",
            "renames"
        ],
        "top-level keys changed — additions must be deliberate, removals are breaking"
    );
    assert_eq!(value["meta"]["schemaVersion"], 1);

    let entry = value["functions"]
        .as_array()
        .expect("functions is an array")
        .iter()
        .find(|function| function["name"] == "array::len")
        .expect("array::len is a stable builtin");
    assert_eq!(
        entry,
        &json!({
            "name": "array::len",
            "params": [{ "name": "array", "type": "array", "form": "required" }],
            "isAsync": false,
            "notCallable": false,
            "returns": "int",
        }),
        "a function entry changed shape — update this golden only for a deliberate, reviewed change"
    );
}

/// Every historical settings shape keeps parsing: nested vs flat
/// roots, camelCase vs snake_case aliases.
#[test]
fn config_accepts_all_historical_shapes() {
    let cases = [
        // Nested root, camelCase.
        json!({
            "surrealql": {
                "connection": { "endpoint": "ws://a:8000/rpc", "namespace": "ns", "database": "db",
                                 "username": "u", "password": "p", "token": "t", "access": "acc" },
                "metadata": { "mode": "workspace+db", "enableLiveMetadata": false, "refreshOnSave": false },
                "analysis": { "enablePermissionAnalysis": false, "enableAggressiveSchemaInference": false, "enableCodeActions": false },
                "authContexts": [{ "name": "admin", "roles": ["admin"], "authRecord": "user:admin" }],
                "activeAuthContext": "admin",
            }
        }),
        // Nested root, snake_case aliases.
        json!({
            "surrealql": {
                "connection": { "endpoint": "ws://a:8000/rpc", "namespace": "ns", "database": "db",
                                 "username": "u", "password": "p", "token": "t", "access": "acc" },
                "metadata": { "mode": "workspace+db", "enable_live_metadata": false, "refresh_on_save": false },
                "analysis": { "enable_permission_analysis": false, "enable_aggressive_schema_inference": false, "enable_code_actions": false },
                "auth_contexts": [{ "name": "admin", "roles": ["admin"], "auth_record": "user:admin" }],
                "active_auth_context": "admin",
            }
        }),
        // Flat root (no `surrealql` wrapper).
        json!({
            "connection": { "endpoint": "ws://a:8000/rpc", "namespace": "ns", "database": "db",
                             "username": "u", "password": "p", "token": "t", "access": "acc" },
            "metadata": { "mode": "workspace+db", "enableLiveMetadata": false, "refreshOnSave": false },
            "analysis": { "enablePermissionAnalysis": false, "enableAggressiveSchemaInference": false, "enableCodeActions": false },
            "authContexts": [{ "name": "admin", "roles": ["admin"], "authRecord": "user:admin" }],
            "activeAuthContext": "admin",
        }),
    ];

    for (index, case) in cases.iter().enumerate() {
        let (settings, warnings) = ServerSettings::from_sources_with_warnings(Some(case), None);
        assert_eq!(warnings, Vec::<String>::new(), "case {index} must not warn");
        assert_eq!(
            settings.connection.endpoint.as_deref(),
            Some("ws://a:8000/rpc")
        );
        assert_eq!(settings.connection.namespace.as_deref(), Some("ns"));
        assert_eq!(settings.connection.database.as_deref(), Some("db"));
        assert_eq!(settings.connection.username.as_deref(), Some("u"));
        assert_eq!(settings.connection.password.as_deref(), Some("p"));
        assert_eq!(settings.connection.token.as_deref(), Some("t"));
        assert_eq!(settings.connection.access.as_deref(), Some("acc"));
        assert!(!settings.metadata.enable_live_metadata, "case {index}");
        assert!(!settings.metadata.refresh_on_save, "case {index}");
        assert!(
            !settings.analysis.enable_permission_analysis,
            "case {index}"
        );
        assert!(
            !settings.analysis.enable_aggressive_schema_inference,
            "case {index}"
        );
        assert!(!settings.analysis.enable_code_actions, "case {index}");
        assert_eq!(settings.auth_contexts[0].name, "admin", "case {index}");
        assert_eq!(
            settings.auth_contexts[0].auth_record.as_deref(),
            Some("user:admin"),
            "case {index}"
        );
        assert_eq!(settings.active_auth_context.as_deref(), Some("admin"));
    }
}

/// The six accepted `metadata.mode` strings and their effect on the
/// two schema sources — observable behavior clients depend on.
#[test]
fn metadata_mode_truth_table_is_stable() {
    let table = [
        ("both", true, true),
        ("workspace+db", true, true),
        ("filesystem", true, false),
        ("workspace", true, false),
        ("db", false, true),
        ("remote", false, true),
    ];
    for (mode, filesystem, db) in table {
        let value = json!({ "surrealql": { "metadata": { "mode": mode } } });
        let (settings, warnings) = ServerSettings::from_sources_with_warnings(Some(&value), None);
        assert_eq!(
            warnings,
            Vec::<String>::new(),
            "mode `{mode}` must not warn"
        );
        assert_eq!(
            settings.metadata.filesystem_enabled(),
            filesystem,
            "filesystem_enabled for `{mode}`"
        );
        assert_eq!(
            settings.metadata.db_enabled(),
            db,
            "db_enabled for `{mode}`"
        );
    }
}

/// The `show_message` default impl must forward to `log_message` so
/// pre-0.3 `LspNotifier` implementors (which don't know the method)
/// still surface toast content somewhere.
#[tokio::test]
async fn show_message_default_impl_falls_back_to_log_message() {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use surrealql_language_server::core::LspNotifier;
    use tower_lsp_server::ls_types::{Diagnostic, MessageType, Uri};

    /// A minimal pre-0.3-style implementor: no `show_message` override.
    #[derive(Default)]
    struct LegacyNotifier {
        logs: Arc<Mutex<Vec<(MessageType, String)>>>,
    }

    #[async_trait]
    impl LspNotifier for LegacyNotifier {
        async fn publish_diagnostics(&self, _uri: Uri, _diagnostics: Vec<Diagnostic>) {}
        async fn log_message(&self, level: MessageType, message: String) {
            self.logs.lock().unwrap().push((level, message));
        }
        async fn request_configuration(&self) -> Option<serde_json::Value> {
            None
        }
    }

    let notifier = LegacyNotifier::default();
    notifier
        .show_message(MessageType::WARNING, "toast content".to_string())
        .await;
    assert_eq!(
        notifier.logs.lock().unwrap().as_slice(),
        &[(MessageType::WARNING, "toast content".to_string())],
        "default show_message must route through log_message"
    );
}

/// Defaults are wire-observable (they decide behavior when a client
/// sends nothing).
#[test]
fn default_settings_are_stable() {
    let settings = ServerSettings::default();
    assert_eq!(settings.metadata.mode, "workspace+db");
    assert!(settings.metadata.enable_live_metadata);
    assert!(settings.metadata.refresh_on_save);
    assert!(settings.analysis.enable_permission_analysis);
    assert!(settings.analysis.enable_aggressive_schema_inference);
    assert!(settings.analysis.enable_code_actions);
    assert!(settings.analysis.enable_type_checking);
    assert_eq!(settings.analysis.schemaless_diagnostics, "quiet");
    assert_eq!(settings.analysis.max_syntax_diagnostics, 2000);
    assert_eq!(settings.active_auth_context.as_deref(), Some("viewer"));
    assert_eq!(settings.auth_contexts.len(), 1);
    assert_eq!(settings.auth_contexts[0].name, "viewer");
    assert_eq!(settings.auth_contexts[0].roles, vec!["viewer".to_string()]);
    assert!(settings.connection.endpoint.is_none());
}

// ---------------------------------------------------------------------------
// The `check` subcommand surface (machine-readable output, exit codes, flags)
// ---------------------------------------------------------------------------

/// Exact-equality golden for the `--format json` report shape. The
/// diagnostics inside are LSP wire objects verbatim (0-based lines, UTF-16
/// columns, camelCase keys, integer severity, string code) — agents key
/// repairs on these fields, so changes must be additive and deliberate.
#[test]
fn check_json_report_shape_golden() {
    use surrealql_language_server::native::check::{
        CheckReport, FileReport, ScanReport, Summary, render_json,
    };
    use tower_lsp_server::ls_types::{Diagnostic, DiagnosticSeverity, Position, Range};

    let report = CheckReport {
        version: "test".to_string(),
        files: vec![FileReport {
            path: "queries/feed.surql".to_string(),
            diagnostics: vec![Diagnostic {
                range: Range {
                    start: Position {
                        line: 1,
                        character: 14,
                    },
                    end: Position {
                        line: 1,
                        character: 19,
                    },
                },
                severity: Some(DiagnosticSeverity::WARNING),
                code: Some(NumberOrString::String("unknown-table".to_string())),
                source: Some("surreal-language-server".to_string()),
                message: "Unknown table `persn`. Did you mean `person`?".to_string(),
                data: Some(json!({ "suggestion": "person", "table": "persn" })),
                ..Diagnostic::default()
            }],
        }],
        summary: Summary {
            files_checked: 1,
            errors: 0,
            warnings: 1,
            information: 0,
            hints: 0,
        },
        scan: ScanReport::default(),
        config_warnings: vec![],
        exit_code: 0,
    };
    let value: serde_json::Value =
        serde_json::from_str(&render_json(&report)).expect("render_json emits one JSON object");
    let expected = json!({
        "version": "test",
        "files": [{
            "path": "queries/feed.surql",
            "diagnostics": [{
                "range": {
                    "start": { "line": 1, "character": 14 },
                    "end": { "line": 1, "character": 19 },
                },
                "severity": 2,
                "code": "unknown-table",
                "source": "surreal-language-server",
                "message": "Unknown table `persn`. Did you mean `person`?",
                "data": { "suggestion": "person", "table": "persn" },
            }],
        }],
        "summary": {
            "filesChecked": 1,
            "errors": 0,
            "warnings": 1,
            "information": 0,
            "hints": 0,
        },
        "scan": {
            "walkErrors": 0,
            "skippedOversize": 0,
            "skippedUnreadable": 0,
            "fileCapHit": false,
        },
        "configWarnings": [],
        "exitCode": 0,
    });
    assert_eq!(
        value, expected,
        "the check JSON report changed shape — update this golden only for a deliberate, reviewed addition"
    );
}

/// The exit-code contract agents and CI key on: 0 clean, 1 diagnostics at
/// or above `--fail-on`, 2 when check could not do what was asked. Also
/// pins that `unknown-table` stays a warning (the default threshold lets
/// it pass) and that every documented flag keeps parsing.
#[test]
fn check_exit_codes_and_flags_are_stable() {
    use std::process::Command;

    let binary = env!("CARGO_BIN_EXE_surrealql-language-server");
    let dir = std::env::temp_dir().join(format!("surql-compat-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("schema")).expect("scratch dir");
    std::fs::write(dir.join("clean.surql"), "DEFINE TABLE person SCHEMALESS;\n").expect("fixture");
    std::fs::write(dir.join("broken.surql"), "SELEC * FRM person\n").expect("fixture");
    std::fs::write(
        dir.join("typo.surql"),
        "DEFINE TABLE person SCHEMALESS;\nSELECT * FROM persn;\n",
    )
    .expect("fixture");
    std::fs::write(dir.join("config.json"), "{}").expect("fixture");

    let run = |args: &[&str]| {
        Command::new(binary)
            .arg("check")
            .args(args)
            .current_dir(&dir)
            .output()
            .expect("spawn check")
            .status
            .code()
            .expect("exit code")
    };

    assert_eq!(run(&["clean.surql"]), 0, "clean file");
    assert_eq!(run(&["broken.surql"]), 1, "parse error is an error");
    assert_eq!(run(&["typo.surql"]), 0, "unknown-table stays a warning");
    assert_eq!(
        run(&["typo.surql", "--fail-on", "warning"]),
        1,
        "--fail-on raises it"
    );
    assert_eq!(run(&["absent.surql"]), 2, "missing file");
    assert_eq!(run(&["clean.surql", "--frobnicate"]), 2, "unknown flag");

    // Every documented flag parses; renaming one breaks callers.
    assert_eq!(
        run(&[
            "clean.surql",
            "--workspace",
            "schema",
            "--format",
            "json",
            "--config",
            "config.json",
            "--param",
            "id",
            "--fail-on",
            "error",
        ]),
        0,
        "the documented flag surface"
    );
}
