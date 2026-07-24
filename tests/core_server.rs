//! End-to-end tests driving [`LanguageServerCore`] through its public
//! API with recording mocks — the same pipeline real clients exercise
//! (didOpen → analysis → merged model → published diagnostics).

mod common;

use common::{core_with, uri};
use serde_json::json;
use tower_lsp_server::ls_types::{
    DiagnosticSeverity, DidChangeConfigurationParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, MessageType, NumberOrString,
    TextDocumentIdentifier, TextDocumentItem,
};

fn text_document(path: &str, text: &str) -> TextDocumentItem {
    TextDocumentItem {
        uri: uri(path),
        language_id: "surrealql".to_string(),
        version: 1,
        text: text.to_string(),
    }
}

async fn open(core: &common::TestCore, path: &str, text: &str) {
    core.did_open(DidOpenTextDocumentParams {
        text_document: text_document(path, text),
    })
    .await;
}

#[tokio::test]
async fn did_open_publishes_syntax_diagnostics_for_broken_document() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());

    open(&core, "bad.surql", "DEFINE TABLE @@@invalid@@@;").await;

    let diagnostics = notifier
        .last_published_for(&uri("bad.surql"))
        .expect("diagnostics published for the opened document");
    assert!(
        !diagnostics.is_empty(),
        "broken surql must produce diagnostics"
    );
    for diagnostic in &diagnostics {
        assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            diagnostic.code,
            Some(NumberOrString::String("parse".to_string()))
        );
        assert_eq!(
            diagnostic.source.as_deref(),
            Some("surreal-language-server")
        );
    }
}

#[tokio::test]
async fn did_open_clean_document_publishes_empty_diagnostics() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());

    open(
        &core,
        "clean.surql",
        "DEFINE TABLE person SCHEMAFULL PERMISSIONS FOR select FULL;\n\
         DEFINE FIELD name ON TABLE person TYPE string;",
    )
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("clean.surql"))
        .expect("diagnostics published for the opened document");
    assert_eq!(
        diagnostics,
        Vec::new(),
        "clean document must publish an empty set"
    );
}

#[tokio::test]
async fn did_close_clears_diagnostics() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());

    open(&core, "bad.surql", "DEFINE TABLE @@@invalid@@@;").await;
    core.did_close(DidCloseTextDocumentParams {
        text_document: TextDocumentIdentifier {
            uri: uri("bad.surql"),
        },
    })
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("bad.surql"))
        .expect("close must publish");
    assert_eq!(diagnostics, Vec::new(), "close must clear diagnostics");
}

#[tokio::test]
async fn initialized_pulls_configuration_and_logs_ready() {
    let (core, notifier, metadata) = core_with(Default::default(), Default::default());
    *notifier.configuration.lock().unwrap() = Some(json!({
        "surrealql": { "connection": { "endpoint": "ws://from-pull:8000/rpc" } }
    }));

    core.initialize(InitializeParams::default()).await;
    core.initialized().await;

    let logs = notifier.logs();
    assert!(
        logs.iter().any(|(level, message)| {
            *level == MessageType::INFO && message == "SurrealQL semantic language server ready"
        }),
        "ready log missing: {logs:?}"
    );
    let settings = metadata
        .last_settings
        .lock()
        .unwrap()
        .clone()
        .expect("initialized must trigger a metadata fetch");
    assert_eq!(
        settings.connection.endpoint.as_deref(),
        Some("ws://from-pull:8000/rpc"),
        "pulled configuration must reach the metadata provider"
    );
}

#[tokio::test]
async fn did_change_configuration_preserves_connection_from_initialize() {
    let (core, _notifier, metadata) = core_with(Default::default(), Default::default());

    core.initialize(InitializeParams {
        initialization_options: Some(json!({
            "surrealql": { "connection": { "endpoint": "ws://from-init:8000/rpc" } }
        })),
        ..InitializeParams::default()
    })
    .await;

    // A partial payload that says nothing about the connection.
    core.did_change_configuration(DidChangeConfigurationParams {
        settings: json!({ "surrealql": { "metadata": { "refreshOnSave": false } } }),
    })
    .await;

    let settings = metadata
        .last_settings
        .lock()
        .unwrap()
        .clone()
        .expect("configuration change must trigger a metadata fetch");
    // A partial payload must merge over the in-flight settings, not
    // replace them: the endpoint from initializationOptions survives
    // while the pushed metadata flag takes effect.
    assert_eq!(
        settings.connection.endpoint.as_deref(),
        Some("ws://from-init:8000/rpc")
    );
    assert!(!settings.metadata.refresh_on_save);
}

#[tokio::test]
async fn metadata_errors_surface_once_and_log_recovery() {
    use surrealql_language_server::semantic::types::LiveMetadataSnapshot;

    let failing = LiveMetadataSnapshot {
        documents: Default::default(),
        errors: vec![
            "failed to connect to SurrealDB: connection refused".to_string(),
            "INFO FOR DB returned an error: not permitted".to_string(),
        ],
    };
    let (core, notifier, metadata) = core_with(Default::default(), failing.clone());

    core.initialize(InitializeParams::default()).await;
    core.initialized().await;

    let shows = notifier.shows();
    assert_eq!(
        shows.len(),
        1,
        "one toast per distinct failure set: {shows:?}"
    );
    assert_eq!(shows[0].0, MessageType::WARNING);
    assert!(shows[0].1.contains("live schema metadata unavailable"));
    assert!(shows[0].1.contains("connection refused"));
    assert!(shows[0].1.contains("+1 more"));
    let warning_logs: Vec<_> = notifier
        .logs()
        .into_iter()
        .filter(|(_, message)| message.starts_with("SurrealQL metadata:"))
        .collect();
    assert_eq!(warning_logs.len(), 2, "each error gets its own log line");

    // Same failure set again (e.g. a save with refreshOnSave): no new toast.
    core.did_change_configuration(DidChangeConfigurationParams {
        settings: json!({ "surrealql": {} }),
    })
    .await;
    assert_eq!(
        notifier.shows().len(),
        1,
        "unchanged failures must not re-toast"
    );

    // Recovery: fetch comes back clean → INFO log, still no new toast.
    *metadata.snapshot.lock().unwrap() = LiveMetadataSnapshot::default();
    core.did_change_configuration(DidChangeConfigurationParams {
        settings: json!({ "surrealql": {} }),
    })
    .await;
    assert_eq!(notifier.shows().len(), 1);
    assert!(
        notifier.logs().iter().any(|(level, message)| {
            *level == MessageType::INFO && message.contains("available again")
        }),
        "recovery must be logged"
    );
}

#[tokio::test]
async fn malformed_settings_payload_logs_a_warning_and_keeps_going() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());

    core.initialize(InitializeParams::default()).await;
    core.did_change_configuration(DidChangeConfigurationParams {
        // endpoint must be a string — this typo'd payload used to be
        // silently replaced with all-default settings.
        settings: json!({ "surrealql": { "connection": { "endpoint": 42 } } }),
    })
    .await;

    assert!(
        notifier.logs().iter().any(|(level, message)| {
            *level == MessageType::WARNING
                && message.starts_with("SurrealQL settings:")
                && message.contains("invalid `surrealql` settings")
        }),
        "malformed settings must be reported: {:?}",
        notifier.logs()
    );
}

/// The audit's headline finding: a typo'd table name used to be
/// auto-inferred by the very statement that misused it, so the
/// unknown-table diagnostic and its quick fix were dead code in the
/// real pipeline. This drives the REAL flow (didOpen → analysis →
/// merged model → semantic diagnostics → code action) end to end.
#[tokio::test]
async fn typo_in_table_name_yields_did_you_mean_diagnostic_and_quick_fix() {
    use tower_lsp_server::ls_types::{CodeActionOrCommand, DiagnosticSeverity};

    let (core, notifier, _) = core_with(Default::default(), Default::default());
    let text = "DEFINE TABLE person SCHEMAFULL;\n\
                DEFINE FIELD email ON person TYPE string;\n\
                CREATE prson SET email = 'x';";
    open(&core, "typo.surql", text).await;

    let diagnostics = notifier
        .last_published_for(&uri("typo.surql"))
        .expect("diagnostics published");
    let unknown: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.code == Some(NumberOrString::String("unknown-table".to_string()))
        })
        .collect();
    assert_eq!(
        unknown.len(),
        1,
        "exactly one unknown-table diagnostic: {diagnostics:?}"
    );
    let diagnostic = unknown[0];
    assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::WARNING));
    assert_eq!(
        diagnostic.message,
        "Unknown table `prson`. Did you mean `person`?"
    );
    // The squiggle covers only the `prson` token on line 2.
    assert_eq!(diagnostic.range.start.line, 2);
    assert_eq!(diagnostic.range.start.character, 7);
    assert_eq!(diagnostic.range.end.line, 2);
    assert_eq!(diagnostic.range.end.character, 12);
    // relatedInformation points at the DEFINE TABLE.
    let related = diagnostic
        .related_information
        .as_ref()
        .expect("related information present");
    assert_eq!(related[0].message, "`person` is defined here.");
    assert_eq!(related[0].location.range.start.line, 0);

    // And the quick fix replaces just the typo'd token.
    let code_actions = core
        .code_action(tower_lsp_server::ls_types::CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri("typo.surql"),
            },
            range: diagnostic.range,
            context: tower_lsp_server::ls_types::CodeActionContext {
                diagnostics: vec![diagnostic.clone()],
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .expect("code actions");
    let quick_fix = code_actions
        .iter()
        .find_map(|action| match action {
            CodeActionOrCommand::CodeAction(action) if action.title.starts_with("Replace") => {
                Some(action)
            }
            _ => None,
        })
        .expect("quick fix offered");
    assert_eq!(quick_fix.title, "Replace `prson` with `person`");
}

#[tokio::test]
async fn usage_only_inferred_tables_stay_silent() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    // No explicit schema anywhere: inference from usage is a feature,
    // not a typo — no unknown-table diagnostics.
    open(
        &core,
        "inferred.surql",
        "CREATE metrics_daily SET count = 1;\nSELECT * FROM metrics_daily;",
    )
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("inferred.surql"))
        .expect("published");
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != Some(NumberOrString::String("unknown-table".to_string()))
        }),
        "usage-only inference must not be flagged: {diagnostics:?}"
    );
}

#[tokio::test]
async fn schemaless_tables_allow_ad_hoc_fields() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "schemaless.surql",
        "DEFINE TABLE log SCHEMALESS;\nCREATE log SET anything_goes = true;",
    )
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("schemaless.surql"))
        .expect("published");
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != Some(NumberOrString::String("unknown-field".to_string()))
        }),
        "SCHEMALESS tables must accept ad-hoc fields: {diagnostics:?}"
    );
}

#[tokio::test]
async fn typo_in_schemafull_field_yields_did_you_mean() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "field-typo.surql",
        "DEFINE TABLE person SCHEMAFULL;\n\
         DEFINE FIELD email ON person TYPE string;\n\
         UPDATE person SET emial = 'x';",
    )
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("field-typo.surql"))
        .expect("published");
    let unknown_field = diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code == Some(NumberOrString::String("unknown-field".to_string()))
        })
        .expect("unknown-field diagnostic must fire on a SCHEMAFULL table");
    assert_eq!(
        unknown_field.message,
        "Unknown field `person.emial`. Did you mean `email`?"
    );
    // Tight range over `emial` on line 2.
    assert_eq!(unknown_field.range.start.line, 2);
    assert_eq!(unknown_field.range.end.line, 2);
    assert!(unknown_field.range.end.character - unknown_field.range.start.character == 5);
}

#[tokio::test]
async fn parameter_targets_do_not_warn() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "param-target.surql",
        "DELETE $record;\nSELECT * FROM $source;",
    )
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("param-target.surql"))
        .expect("published");
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != Some(NumberOrString::String("dynamic-target".to_string()))
        }),
        "$param targets must not produce dynamic-target warnings: {diagnostics:?}"
    );
}

#[tokio::test]
async fn genuinely_opaque_targets_still_warn() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    // A literal-number target is neither a table name, a $param, nor
    // an expression — the dynamic-target warning must still fire.
    open(&core, "opaque.surql", "UPDATE 42 SET x = 1;").await;

    let diagnostics = notifier
        .last_published_for(&uri("opaque.surql"))
        .expect("published");
    let warning = diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code == Some(NumberOrString::String("dynamic-target".to_string()))
        })
        .expect("dynamic-target warning must fire for opaque targets");
    assert_eq!(
        warning.severity,
        Some(tower_lsp_server::ls_types::DiagnosticSeverity::WARNING)
    );
    assert!(
        warning
            .message
            .contains("target could not be resolved statically")
    );
}

#[tokio::test]
async fn builtin_id_field_is_not_flagged_on_schemafull_tables() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "builtin-id.surql",
        "DEFINE TABLE person SCHEMAFULL;\n\
         DEFINE FIELD name ON person TYPE string;\n\
         CREATE person SET id = 'john', name = 'John';",
    )
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("builtin-id.surql"))
        .expect("published");
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != Some(NumberOrString::String("unknown-field".to_string()))
        }),
        "builtin `id` must not be flagged: {diagnostics:?}"
    );
}

#[tokio::test]
async fn relate_set_fields_are_not_checked_against_subject_tables() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "relate.surql",
        "DEFINE TABLE person SCHEMAFULL;\n\
         DEFINE FIELD name ON person TYPE string;\n\
         DEFINE TABLE likes SCHEMAFULL;\n\
         DEFINE FIELD since ON likes TYPE datetime;\n\
         RELATE person:one->likes->person:two SET since = time::now();",
    )
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("relate.surql"))
        .expect("published");
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != Some(NumberOrString::String("unknown-field".to_string()))
        }),
        "RELATE SET fields belong to the edge table and must not be checked \
         against the subject tables: {diagnostics:?}"
    );
}

#[tokio::test]
async fn workspace_scan_stats_are_reported() {
    use surrealql_language_server::semantic::types::{WorkspaceIndex, WorkspaceScanStats};

    let workspace = WorkspaceIndex {
        documents: Default::default(),
        scan_stats: WorkspaceScanStats {
            walk_errors: 2,
            skipped_oversize: 1,
            skipped_unreadable: 0,
            file_cap_hit: true,
        },
    };
    let (core, notifier, _) = core_with(workspace, Default::default());

    core.initialize(InitializeParams::default()).await;
    core.initialized().await;

    let logs = notifier.logs();
    let summary = logs
        .iter()
        .find(|(level, message)| {
            *level == MessageType::WARNING && message.contains("workspace scan skipped")
        })
        .expect("scan summary log");
    assert!(summary.1.contains("2 unreadable directory entries"));
    assert!(summary.1.contains("1 oversized files"));
    assert!(summary.1.contains("file limit"));
    assert!(
        notifier
            .shows()
            .iter()
            .any(|(_, message)| message.contains("files were not indexed")),
        "hitting the file cap must toast"
    );
}

#[tokio::test]
async fn unknown_metadata_mode_warns_and_repairs_to_default() {
    let (core, notifier, metadata) = core_with(Default::default(), Default::default());

    core.initialize(InitializeParams::default()).await;
    core.did_change_configuration(DidChangeConfigurationParams {
        settings: json!({ "surrealql": { "metadata": { "mode": "workspaceanddb" } } }),
    })
    .await;

    assert!(
        notifier.logs().iter().any(|(level, message)| {
            *level == MessageType::WARNING && message.contains("unknown metadata.mode")
        }),
        "unknown mode must be reported: {:?}",
        notifier.logs()
    );
    let settings = metadata
        .last_settings
        .lock()
        .unwrap()
        .clone()
        .expect("fetch must run");
    assert_eq!(
        settings.metadata.mode, "workspace+db",
        "unknown mode must repair to the default instead of disabling all metadata"
    );
}
