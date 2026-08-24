//! Backwards-compatibility tripwires. Every assertion here pins an
//! observable surface (LSP wire shape, config parsing, diagnostic
//! identity) — a failure means a change is client-visible and must be
//! a reviewed, deliberate decision, not an accident.

mod common;

use serde_json::json;
use surrealql_language_server::config::ServerSettings;
use surrealql_language_server::semantic::analyzer::analyze_document;
use surrealql_language_server::semantic::pipeline::diagnostics_for_document;
use surrealql_language_server::semantic::rules;
use surrealql_language_server::semantic::types::{
    MergedSemanticModel, SymbolOrigin, WorkspaceIndex,
};
use tower_lsp_server::ls_types::{NumberOrString, Uri};

/// Exact-equality golden for the advertised capabilities. Additions
/// are allowed but must be made here consciously — capability drift is
/// how a wasm host and a native editor end up seeing different
/// servers.
#[test]
fn server_capabilities_golden() {
    let capabilities =
        serde_json::to_value(common::TestCore::server_capabilities()).expect("serializable");
    let expected = json!({
        // Options rather than the bare kind `1`, so `save` is registered and a
        // spec-compliant client actually sends `didSave`. `change: 1` keeps
        // FULL sync — the sync kind itself did not move.
        "positionEncoding": "utf-16",
        "textDocumentSync": {
            "openClose": true,
            "change": 2,
            "save": { "includeText": false },
        },
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
        "diagnosticProvider": {
            "identifier": "surrealql",
            "interFileDependencies": true,
            "workspaceDiagnostics": true,
        },
        "documentFormattingProvider": true,
        "documentRangeFormattingProvider": true,
        "foldingRangeProvider": true,
        "selectionRangeProvider": true,
        "typeDefinitionProvider": true,
        "documentLinkProvider": { "resolveProvider": false },
        "workspaceSymbolProvider": true,
        // The kinds the server actually produces, so a client can decide what
        // to request instead of asking for everything.
        "codeActionProvider": {
            "codeActionKinds": ["quickfix", "refactor.rewrite"],
            "resolveProvider": false,
        },
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
            "full": { "delta": true },
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
// Rule registry
// ---------------------------------------------------------------------------

/// Analyze one document and assemble its full diagnostic set, exactly as
/// `diagnostics_for_document` in `src/core/server.rs` does.
fn all_diagnostics(
    source: &str,
    settings: &ServerSettings,
) -> Vec<tower_lsp_server::ls_types::Diagnostic> {
    let uri: Uri = "file:///rules.surql".parse().unwrap();
    let analysis = analyze_document(uri.clone(), source, SymbolOrigin::Local).expect("analysis");
    let mut workspace = WorkspaceIndex::default();
    workspace
        .documents
        .insert(uri, std::sync::Arc::new(analysis.clone()));
    let model = MergedSemanticModel::build(&workspace, &Default::default());
    diagnostics_for_document(&analysis, &model, settings)
}

/// One source fixture per rule reachable from real SurrealQL.
///
/// These exist so `severities_are_transcribed_not_chosen` can compare the
/// registry against reality rather than against itself. A registry checked only
/// against a hand-written table would prove nothing.
///
fn rule_fixtures() -> Vec<(&'static str, ServerSettings)> {
    let default = ServerSettings::default;
    let strict = || {
        let mut settings = ServerSettings::default();
        settings.analysis.schemaless_diagnostics = "strict".to_string();
        settings
    };

    vec![
        // parse
        ("DEFINE TABLE @@@invalid@@@;", default()),
        // unknown-type
        ("LET $x: xxx = 2;", default()),
        // unknown-table. `persn` rather than `persons`: a plural variant is
        // deliberately not treated as a typo.
        (
            "DEFINE TABLE person SCHEMAFULL;\nSELECT * FROM persn;",
            default(),
        ),
        // unknown-field
        (
            "DEFINE TABLE person SCHEMAFULL;\n\
             DEFINE FIELD name ON person TYPE string;\n\
             CREATE person SET nickname = \"b\";",
            default(),
        ),
        // field-type
        (
            "DEFINE TABLE person SCHEMAFULL;\n\
             DEFINE FIELD age ON person TYPE int DEFAULT \"not a number\";",
            default(),
        ),
        // permission-denied — reachable from source now that the clause parses.
        (
            "DEFINE TABLE locked SCHEMAFULL PERMISSIONS FOR create NONE;\n\
             DEFINE FIELD a ON locked TYPE string;\n\
             CREATE locked SET a = \"x\";",
            default(),
        ),
        // permission-unknown
        (
            "DEFINE TABLE owned SCHEMAFULL PERMISSIONS FOR create WHERE $auth.id = id;\n\
             DEFINE FIELD a ON owned TYPE string;\n\
             CREATE owned SET a = \"x\";",
            strict(),
        ),
        // dynamic-target. A quoted target names no table and is not a
        // parameter or an expression, so it stays `Unresolved`.
        ("SELECT * FROM \"person\";", default()),
        // argument-type
        ("RETURN string::len(123);", default()),
        // argument-count
        ("RETURN string::len(\"a\", \"b\");", default()),
        // let-type
        ("LET $x: int = \"abc\";", default()),
        // return-type
        (
            "DEFINE FUNCTION fn::f() -> int { RETURN \"abc\"; };",
            default(),
        ),
        // operator-type
        ("RETURN \"a\" + 1;", default()),
        // unknown-method
        ("RETURN \"abc\".nonsense();", default()),
        // undefined-variable
        ("RETURN $nope;", default()),
        // renamed-function
        ("RETURN duration::from::days(1);", default()),
        // not-callable
        ("RETURN object::matches({ a: 1 }, { a: 1 });", default()),
        // unknown-function
        ("RETURN fn::does_not_exist();", default()),
        // duplicate-definition
        (
            "DEFINE TABLE person SCHEMAFULL;\nDEFINE TABLE person SCHEMAFULL;",
            default(),
        ),
        // unknown-index-field
        (
            "DEFINE TABLE person SCHEMAFULL;\n\
             DEFINE FIELD email ON person TYPE string;\n\
             DEFINE INDEX by_name ON person FIELDS name;",
            default(),
        ),
        // relation-endpoint
        (
            "DEFINE TABLE person SCHEMALESS;\n\
             DEFINE TABLE company SCHEMALESS;\n\
             DEFINE TABLE works_on TYPE RELATION IN person OUT project ENFORCED;\n\
             RELATE person:1->works_on->company:2;",
            default(),
        ),
        // unused-binding
        ("DEFINE PARAM $unused VALUE 1;", default()),
        // unused-suppression — a directive for a rule that does not report here
        (
            "-- surql-ignore: unknown-table\nDEFINE TABLE person SCHEMAFULL;",
            default(),
        ),
        // unknown-analyzer
        (
            "DEFINE TABLE article SCHEMAFULL;\n\
             DEFINE FIELD body ON article TYPE string;\n\
             DEFINE INDEX ft ON article FIELDS body SEARCH ANALYZER nope BM25;",
            default(),
        ),
    ]
}

/// Every severity in the registry is the severity the emitter actually
/// produces.
///
/// This is the test that makes it safe for a later change to re-stamp
/// `Diagnostic.severity` from `Rule::default_severity`. Without it, one wrong
/// cell in the registry table silently rewrites the wire for every client, and
/// nothing fails.
#[test]
fn severities_are_transcribed_not_chosen() {
    let batches: Vec<Vec<tower_lsp_server::ls_types::Diagnostic>> = rule_fixtures()
        .iter()
        .map(|(source, settings)| all_diagnostics(source, settings))
        .collect();

    for diagnostic in batches.iter().flatten() {
        let Some(NumberOrString::String(code)) = &diagnostic.code else {
            panic!("diagnostic without a string code: {diagnostic:?}");
        };
        let rule = rules::rule(code).unwrap_or_else(|| panic!("`{code}` has no registered rule"));
        assert_eq!(
            diagnostic.severity,
            Some(rule.default_severity),
            "`{code}` is emitted at {:?} but the registry says {:?}",
            diagnostic.severity,
            rule.default_severity
        );
    }
}

/// The fixtures really do reach every rule. A severity check that covers only
/// 12 of 17 codes leaves the other five free to drift.
#[test]
fn the_fixtures_reach_every_rule() {
    let mut seen: Vec<String> = Vec::new();
    let batches: Vec<Vec<tower_lsp_server::ls_types::Diagnostic>> = rule_fixtures()
        .iter()
        .map(|(source, settings)| all_diagnostics(source, settings))
        .collect();

    for diagnostic in batches.iter().flatten() {
        if let Some(NumberOrString::String(code)) = &diagnostic.code
            && !seen.contains(code)
        {
            seen.push(code.clone());
        }
    }
    seen.sort();
    let mut expected: Vec<String> = rules::ids().map(str::to_string).collect();
    expected.sort();
    assert_eq!(
        seen, expected,
        "the rule fixtures no longer cover every registered rule"
    );
}

/// Exact-equality golden over the registry, in the spirit of
/// `server_capabilities_golden`. A rule's id, category, severity and fix
/// applicability all become user-visible once the catalogue is published, so a
/// change to any of them must be deliberate.
#[test]
fn rule_registry_golden() {
    let rendered: Vec<String> = rules::RULES
        .iter()
        .map(|rule| {
            format!(
                "{} | {:?} | {:?} | {}",
                rule.id,
                rule.category,
                rule.default_severity,
                match rule.fix {
                    Some(applicability) => format!("{applicability:?}"),
                    None => "-".to_string(),
                }
            )
        })
        .collect();

    assert_eq!(
        rendered,
        vec![
            "argument-count | Types | Error | -",
            "argument-type | Types | Error | -",
            "duplicate-definition | Schema | Warning | -",
            "dynamic-target | Schema | Warning | -",
            "field-type | Types | Error | -",
            "let-type | Types | Error | -",
            "not-callable | Types | Warning | -",
            "operator-type | Types | Error | -",
            "permission-denied | Permissions | Error | -",
            "permission-unknown | Permissions | Warning | -",
            "renamed-function | Types | Warning | MachineApplicable",
            "return-type | Types | Error | -",
            "undefined-variable | Types | Error | -",
            "unknown-field | Schema | Warning | -",
            "unknown-table | Schema | Warning | Suggestion",
            "unknown-index-field | Schema | Warning | -",
            "unknown-analyzer | Schema | Warning | -",
            "unknown-function | Types | Error | Suggestion",
            "relation-endpoint | Schema | Error | -",
            "unused-binding | Types | Hint | -",
            "unused-suppression | Syntax | Hint | -",
            "unknown-method | Types | Error | -",
            "unknown-type | Syntax | Error | Suggestion",
            "parse | Syntax | Error | -",
        ]
    );
}

/// Every diagnostic carries a link to its rule page. Clients that support
/// `codeDescription` turn the code in the problems panel into a link, which is
/// the difference between seeing `unknown-table` and learning why it fired.
#[test]
fn every_diagnostic_links_to_its_rule_page() {
    for (source, settings) in rule_fixtures() {
        for diagnostic in all_diagnostics(source, &settings) {
            let Some(NumberOrString::String(code)) = &diagnostic.code else {
                continue;
            };
            let description = diagnostic
                .code_description
                .as_ref()
                .unwrap_or_else(|| panic!("`{code}` carries no codeDescription"));
            assert!(
                description.href.as_str().ends_with(&format!("#{code}")),
                "`{code}` links to {}",
                description.href.as_str()
            );
        }
    }
}

/// The grammar revision is written in six places. They drifting apart is how a
/// CI run ends up testing a different grammar from the one a developer built
/// against — and the node-kind constants in `src/semantic/node_kind.rs` are
/// coupled to one specific revision.
#[test]
fn the_grammar_pin_is_the_same_everywhere() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let canonical = {
        let script = std::fs::read_to_string(root.join("scripts/setup-grammar.sh"))
            .expect("setup-grammar.sh");
        let line = script
            .lines()
            .find(|line| line.starts_with("GRAMMAR_REF="))
            .expect("GRAMMAR_REF");
        line.split('-')
            .next_back()
            .and_then(|tail| tail.split('}').next())
            .expect("a sha")
            .to_string()
    };
    assert_eq!(
        canonical.len(),
        40,
        "expected a full sha, got `{canonical}`"
    );

    for relative in [
        ".github/workflows/ci.yml",
        "docs/grammar-gaps.md",
        "docs/perf-plan.md",
    ] {
        let text = std::fs::read_to_string(root.join(relative)).expect(relative);
        // Any 40-character hex run that is not the canonical pin is a stale
        // copy. Action SHAs are pinned the same way, so only lines that also
        // mention the grammar are considered.
        for line in text
            .lines()
            .filter(|line| line.contains("surrealql-tree-sitter") || line.contains("GRAMMAR_REF"))
        {
            for word in line.split(|c: char| !c.is_ascii_alphanumeric()) {
                if word.len() == 40 && word.chars().all(|c| c.is_ascii_hexdigit()) {
                    assert_eq!(
                        word, canonical,
                        "{relative} pins a different grammar revision"
                    );
                }
            }
        }
    }
}

/// A client that sends only `rootUri` — or only the older `rootPath` — still
/// gets a workspace. Reading `workspaceFolders` alone left the list empty, so
/// nothing was walked and no `surrealql.toml` was ever found; both features
/// looked broken when in fact the folder list was.
#[test]
fn a_root_only_client_still_gets_a_workspace() {
    use surrealql_language_server::core::server::resolve_workspace_folders_for_test as resolve;
    use tower_lsp_server::ls_types::{InitializeParams, WorkspaceFolder};

    let folder = |path: &str| WorkspaceFolder {
        uri: format!("file://{path}").parse().expect("uri"),
        name: "w".to_string(),
    };

    // `workspaceFolders` wins when present.
    #[allow(deprecated)]
    let both = InitializeParams {
        workspace_folders: Some(vec![folder("/from/folders")]),
        root_uri: Some("file:///from/root".parse().expect("uri")),
        ..Default::default()
    };
    assert_eq!(
        resolve(&both),
        vec![std::path::PathBuf::from("/from/folders")]
    );

    // `rootUri` when it does not.
    #[allow(deprecated)]
    let root_only = InitializeParams {
        root_uri: Some("file:///from/root".parse().expect("uri")),
        ..Default::default()
    };
    assert_eq!(
        resolve(&root_only),
        vec![std::path::PathBuf::from("/from/root")]
    );

    // And `rootPath`, which predates `rootUri`.
    #[allow(deprecated)]
    let path_only = InitializeParams {
        root_path: Some("/from/path".to_string()),
        ..Default::default()
    };
    assert_eq!(
        resolve(&path_only),
        vec![std::path::PathBuf::from("/from/path")]
    );

    // Nothing at all is still nothing, not a panic.
    assert!(resolve(&InitializeParams::default()).is_empty());
}
