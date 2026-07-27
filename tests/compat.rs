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
            "resolveProvider": false,
            "triggerCharacters": [".", ":", "<", "$", "("],
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
    assert_eq!(settings.active_auth_context.as_deref(), Some("viewer"));
    assert_eq!(settings.auth_contexts.len(), 1);
    assert_eq!(settings.auth_contexts[0].name, "viewer");
    assert_eq!(settings.auth_contexts[0].roles, vec!["viewer".to_string()]);
    assert!(settings.connection.endpoint.is_none());
}
