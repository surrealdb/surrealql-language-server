//! End-to-end tests driving [`LanguageServerCore`] through its public
//! API with recording mocks — the same pipeline real clients exercise
//! (didOpen → analysis → merged model → published diagnostics).

mod common;

use common::{core_with, uri};
use serde_json::json;
use surrealql_language_server::config::ServerSettings;
use tower_lsp_server::ls_types::{
    CompletionItem, CompletionParams, CompletionResponse, Diagnostic, DiagnosticSeverity,
    DidChangeConfigurationParams, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse, InitializeParams,
    MessageType, NumberOrString, Position, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams, VersionedTextDocumentIdentifier,
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

/// Drive the real `textDocument/completion` handler at one cursor position.
async fn complete(
    core: &common::TestCore,
    path: &str,
    line: u32,
    character: u32,
) -> Vec<CompletionItem> {
    let response = core
        .completion(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri(path) },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await
        .expect("the handler must answer for an open document");
    match response {
        CompletionResponse::Array(items) => items,
        CompletionResponse::List(list) => list.items,
    }
}

fn labels(items: &[CompletionItem]) -> Vec<&str> {
    items.iter().map(|item| item.label.as_str()).collect()
}

/// A document that registers one table, then leaves the cursor on line 1.
const WITH_TABLE: &str = "DEFINE TABLE person SCHEMAFULL;\n";

// ──────────────────────────────────────────────────────────────────────
// What structured builtin parameters unlock
// ──────────────────────────────────────────────────────────────────────

async fn signature_help_at(
    core: &common::TestCore,
    path: &str,
    line: u32,
    character: u32,
) -> tower_lsp_server::ls_types::SignatureHelp {
    core.signature_help(tower_lsp_server::ls_types::SignatureHelpParams {
        context: None,
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri(path) },
            position: Position { line, character },
        },
        work_done_progress_params: Default::default(),
    })
    .await
    .expect("signature help for a builtin call")
}

#[tokio::test]
async fn signature_help_covers_a_namespace_the_curated_table_never_had() {
    // `math::` was one of the 18 namespaces with no curated entry, so this
    // position used to answer nothing.
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "RETURN math::clamp(";
    open(&core, "a.surql", text).await;

    let help = signature_help_at(&core, "a.surql", 0, text.len() as u32).await;

    let signature = &help.signatures[0];
    let labels: Vec<String> = signature
        .parameters
        .as_ref()
        .expect("parameters")
        .iter()
        .map(|param| match &param.label {
            tower_lsp_server::ls_types::ParameterLabel::Simple(label) => label.clone(),
            tower_lsp_server::ls_types::ParameterLabel::LabelOffsets(_) => String::new(),
        })
        .collect();
    assert_eq!(
        labels,
        vec!["arg: number", "min: number", "max: number"],
        "parameters come from the engine's own implementation"
    );
}

#[tokio::test]
async fn signature_help_reads_a_closure_through_its_variable() {
    // `$double(` has no `DEFINE FUNCTION` and no catalogue entry; the parameters
    // and result come from the type the `LET` binding carries.
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "LET $double = |$x: int| $x * 2;\nRETURN $double(";
    open(&core, "a.surql", text).await;

    let help = signature_help_at(&core, "a.surql", 1, "RETURN $double(".len() as u32).await;

    let signature = &help.signatures[0];
    assert_eq!(signature.label, "$double($x: int) -> int");
    let rendered = format!("{:?}", signature.parameters);
    assert!(rendered.contains("$x: int"), "{rendered}");
}

#[tokio::test]
async fn signature_help_marks_optional_and_variadic_parameters() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "RETURN array::insert(";
    open(&core, "a.surql", text).await;

    let help = signature_help_at(&core, "a.surql", 0, text.len() as u32).await;
    let rendered = format!("{:?}", help.signatures[0].parameters);
    assert!(
        rendered.contains("index?: int"),
        "an omittable parameter carries `?`: {rendered}"
    );
}

#[tokio::test]
async fn signature_help_keeps_the_curated_prose_where_it_exists() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "RETURN string::len(";
    open(&core, "a.surql", text).await;

    let help = signature_help_at(&core, "a.surql", 0, text.len() as u32).await;
    assert!(
        help.signatures[0].documentation.is_some(),
        "one of the 79 curated entries must keep its summary"
    );
}

#[tokio::test]
async fn inlay_hints_name_the_arguments_of_a_multi_parameter_builtin() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "RETURN math::clamp(5, 1, 10);";
    open(&core, "a.surql", text).await;

    let hints = core
        .inlay_hint(tower_lsp_server::ls_types::InlayHintParams {
            text_document: TextDocumentIdentifier {
                uri: uri("a.surql"),
            },
            range: tower_lsp_server::ls_types::Range {
                start: Position::new(0, 0),
                end: Position::new(0, text.len() as u32),
            },
            work_done_progress_params: Default::default(),
        })
        .await;

    let labels: Vec<String> = hints
        .iter()
        .map(|hint| match &hint.label {
            tower_lsp_server::ls_types::InlayHintLabel::String(label) => label.clone(),
            tower_lsp_server::ls_types::InlayHintLabel::LabelParts(_) => String::new(),
        })
        .collect();
    assert_eq!(labels, vec!["arg:", "min:", "max:"]);
}

#[tokio::test]
async fn a_single_parameter_builtin_gets_no_inlay_hint() {
    // `arg:` next to the only argument is noise, and most builtins take one.
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "RETURN string::len('abc');";
    open(&core, "a.surql", text).await;

    let hints = core
        .inlay_hint(tower_lsp_server::ls_types::InlayHintParams {
            text_document: TextDocumentIdentifier {
                uri: uri("a.surql"),
            },
            range: tower_lsp_server::ls_types::Range {
                start: Position::new(0, 0),
                end: Position::new(0, text.len() as u32),
            },
            work_done_progress_params: Default::default(),
        })
        .await;

    assert!(hints.is_empty(), "got {hints:?}");
}

#[tokio::test]
async fn a_renamed_builtin_warns_and_offers_the_current_spelling() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", "RETURN type::thing('person', 'one');").await;

    let diagnostics = notifier
        .last_published_for(&uri("a.surql"))
        .expect("diagnostics");
    let renamed: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.code == Some(NumberOrString::String("renamed-function".to_string()))
        })
        .collect();
    assert_eq!(renamed.len(), 1, "got {diagnostics:?}");
    assert_eq!(
        renamed[0].severity,
        Some(DiagnosticSeverity::WARNING),
        "the engine still accepts the old name"
    );
    assert!(renamed[0].message.contains("renamed to `type::record`"));

    // And the quick fix rewrites it.
    let actions = core
        .code_action(tower_lsp_server::ls_types::CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri("a.surql"),
            },
            range: renamed[0].range,
            context: tower_lsp_server::ls_types::CodeActionContext {
                diagnostics: vec![renamed[0].clone()],
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .expect("code actions");
    let rendered = format!("{actions:?}");
    assert!(
        rendered.contains("Rename `type::thing` to `type::record`"),
        "expected a rename fix, got {rendered}"
    );
    assert!(rendered.contains("type::record"), "the edit must apply it");
}

#[tokio::test]
async fn a_current_function_name_produces_no_rename_warning() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", "RETURN type::record('person', 'one');").await;

    let diagnostics = notifier
        .last_published_for(&uri("a.surql"))
        .expect("diagnostics");
    assert!(
        !diagnostics.iter().any(|diagnostic| diagnostic.code
            == Some(NumberOrString::String("renamed-function".to_string()))),
        "got {diagnostics:?}"
    );
}

#[tokio::test]
async fn an_analyzer_name_is_offered_where_an_index_references_one() {
    // Nothing extracted `DEFINE ANALYZER` before, so this slot had nothing to
    // offer no matter how it was classified.
    let (core, _, _) = core_with(Default::default(), Default::default());
    let second = "DEFINE INDEX i ON person FIELDS name FULLTEXT ANALYZER ";
    let text = format!("DEFINE ANALYZER my_an TOKENIZERS BLANK;\n{second}");
    open(&core, "a.surql", &text).await;

    let items = complete(&core, "a.surql", 1, second.len() as u32).await;

    assert!(
        labels(&items).contains(&"my_an"),
        "expected the defined analyzer, got {:?}",
        labels(&items)
    );
    assert!(
        !labels(&items).iter().any(|label| label.contains("::")),
        "no function is legal in an analyzer slot: {:?}",
        labels(&items)
    );
}

#[tokio::test]
async fn remove_analyzer_offers_the_existing_names() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        "DEFINE ANALYZER my_an TOKENIZERS BLANK;\nREMOVE ANALYZER ",
    )
    .await;

    let items = complete(&core, "a.surql", 1, "REMOVE ANALYZER ".len() as u32).await;
    assert_eq!(labels(&items), vec!["my_an"]);
}

#[tokio::test]
async fn a_define_param_name_is_offered_in_an_expression() {
    // The model has always held these — hover and go-to-definition resolve them
    // — but nothing ever offered them.
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", "DEFINE PARAM $rate VALUE 0.2;\nRETURN $r").await;

    let items = complete(&core, "a.surql", 1, 9).await;
    assert!(
        labels(&items).contains(&"$rate"),
        "expected the defined parameter, got {:?}",
        labels(&items)
    );
}

#[tokio::test]
async fn a_define_access_is_indexed() {
    // The grammar wraps `DEFINE ACCESS` in an `AccessDefinition` node, so the
    // second-keyword lookup returned `None` and the extraction arm never ran.
    // Hover is the observable proof that it does now.
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        "DEFINE ACCESS api ON DATABASE TYPE RECORD;",
    )
    .await;

    let hover = core
        .hover(tower_lsp_server::ls_types::HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri("a.surql"),
                },
                position: Position::new(0, 15),
            },
            work_done_progress_params: Default::default(),
        })
        .await;

    let rendered = format!("{hover:?}");
    assert!(
        rendered.contains("api"),
        "DEFINE ACCESS must reach the model: {rendered}"
    );
}

#[tokio::test]
async fn info_for_offers_only_the_nine_engine_targets() {
    // The reported defect: this position used to return the whole catalogue.
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", &format!("{WITH_TABLE}INFO FOR ")).await;

    let items = complete(&core, "a.surql", 1, 9).await;

    assert_eq!(
        labels(&items),
        vec![
            "ROOT",
            "NAMESPACE",
            "NS",
            "DATABASE",
            "DB",
            "TABLE",
            "TB",
            "USER",
            "INDEX"
        ],
        "INFO FOR accepts exactly these targets"
    );
}

#[tokio::test]
async fn info_for_offers_no_function_and_no_foreign_keyword() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", &format!("{WITH_TABLE}INFO FOR ")).await;

    let items = complete(&core, "a.surql", 1, 9).await;

    for label in labels(&items) {
        assert!(
            !label.contains("::"),
            "no builtin or user function is legal after INFO FOR, got `{label}`"
        );
    }
    for illegal in ["SELECT", "CREATE", "WHERE", "ALLINSIDE", "person"] {
        assert!(
            !labels(&items).contains(&illegal),
            "`{illegal}` is not legal after INFO FOR"
        );
    }
}

#[tokio::test]
async fn a_partial_target_filters_the_head_list_case_insensitively() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", &format!("{WITH_TABLE}INFO FOR ro")).await;

    let items = complete(&core, "a.surql", 1, 11).await;

    assert_eq!(labels(&items), vec!["ROOT"], "lowercase must still match");
}

#[tokio::test]
async fn info_for_table_offers_the_known_tables() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", &format!("{WITH_TABLE}INFO FOR TABLE ")).await;

    let items = complete(&core, "a.surql", 1, 15).await;

    assert!(
        labels(&items).contains(&"person"),
        "expected the defined table, got {:?}",
        labels(&items)
    );
}

#[tokio::test]
async fn define_offers_the_sixteen_sub_forms_and_nothing_else() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", &format!("{WITH_TABLE}DEFINE ")).await;

    let items = complete(&core, "a.surql", 1, 7).await;

    assert_eq!(items.len(), 16, "got {:?}", labels(&items));
    assert!(labels(&items).contains(&"ANALYZER"));
    assert!(
        !labels(&items).contains(&"MODEL"),
        "SurrealDB 3.x has no DEFINE MODEL"
    );
}

#[tokio::test]
async fn a_where_clause_keeps_the_full_list() {
    // The busiest completion position in the language. Narrowing it would hide
    // fields, variables and functions, so it must stay untouched.
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        &format!("{WITH_TABLE}SELECT * FROM person WHERE "),
    )
    .await;

    let items = complete(&core, "a.surql", 1, 27).await;

    assert!(
        labels(&items).iter().any(|label| label.contains("::")),
        "functions must still be offered inside WHERE"
    );
}

#[tokio::test]
async fn an_unclosed_call_keeps_the_full_list() {
    // `(` is a trigger character, so this fires on every keystroke inside a
    // call. The head table must not answer here.
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        &format!("{WITH_TABLE}RETURN string::len("),
    )
    .await;

    let items = complete(&core, "a.surql", 1, 19).await;

    assert!(
        items.len() > 20,
        "an argument position keeps the full list, got {} items",
        items.len()
    );
}

#[tokio::test]
async fn select_from_still_offers_only_tables() {
    // Regression guard: the head table must not shadow the existing
    // table-name scanner.
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", &format!("{WITH_TABLE}SELECT * FROM ")).await;

    let items = complete(&core, "a.surql", 1, 14).await;

    assert!(labels(&items).contains(&"person"));
    assert!(
        !labels(&items).contains(&"SELECT"),
        "a table slot offers no keyword, got {:?}",
        labels(&items)
    );
}

#[tokio::test]
async fn a_statement_after_a_semicolon_is_classified_on_its_own() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "SELECT * FROM person; INFO FOR ";
    open(&core, "a.surql", text).await;

    let items = complete(&core, "a.surql", 0, text.len() as u32).await;

    assert_eq!(
        labels(&items),
        vec![
            "ROOT",
            "NAMESPACE",
            "NS",
            "DATABASE",
            "DB",
            "TABLE",
            "TB",
            "USER",
            "INDEX"
        ],
        "the earlier statement must not leak into the word list"
    );
}

#[tokio::test]
async fn a_half_typed_keyword_is_the_prefix_not_a_committed_word() {
    // Cursor immediately after `FOR` with no space: the author is still typing
    // that word, so the slot is the one `INFO` opens and `FOR` filters it.
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", "INFO FOR").await;

    let items = complete(&core, "a.surql", 0, 8).await;

    assert_eq!(labels(&items), vec!["FOR"]);
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

/// `LET $x: strng = 1` names a type SurrealDB does not have, so the engine
/// refuses to parse the query. This drives the real flow (didOpen → analysis →
/// published diagnostics → code action) to prove the report reaches a client and
/// carries a working quick fix.
#[tokio::test]
async fn unknown_type_yields_did_you_mean_diagnostic_and_quick_fix() {
    use tower_lsp_server::ls_types::CodeActionOrCommand;

    let (core, notifier, _) = core_with(Default::default(), Default::default());
    let text = "LET $a: strng = 1;";
    open(&core, "badtype.surql", text).await;

    let diagnostics = notifier
        .last_published_for(&uri("badtype.surql"))
        .expect("diagnostics published");
    let unknown: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.code == Some(NumberOrString::String("unknown-type".to_string()))
        })
        .collect();
    assert_eq!(
        unknown.len(),
        1,
        "exactly one unknown-type diagnostic: {diagnostics:?}"
    );
    let diagnostic = unknown[0];
    // An ERROR, not a warning: the query never runs.
    assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(
        diagnostic.message,
        "Unknown type `strng`. Did you mean `string`?"
    );
    // The squiggle covers only the type name.
    assert_eq!(diagnostic.range.start.line, 0);
    assert_eq!(diagnostic.range.start.character, 8);
    assert_eq!(diagnostic.range.end.line, 0);
    assert_eq!(diagnostic.range.end.character, 13);

    let code_actions = core
        .code_action(tower_lsp_server::ls_types::CodeActionParams {
            text_document: TextDocumentIdentifier {
                uri: uri("badtype.surql"),
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
    assert_eq!(quick_fix.title, "Replace `strng` with `string`");
}

/// The report must not vanish when a user turns the type checker off. An unknown
/// type name is a syntax fault, so it lives in the syntax pass; the gated
/// `let-type` check is here as the control that proves the toggle really applied.
#[tokio::test]
async fn unknown_type_survives_disabled_type_checking() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    core.did_change_configuration(DidChangeConfigurationParams {
        settings: json!({ "surrealql": { "analysis": { "enableTypeChecking": false } } }),
    })
    .await;

    open(
        &core,
        "gated.surql",
        "LET $a: strng = 1;\nLET $b: int = 'a';",
    )
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("gated.surql"))
        .expect("diagnostics published");
    let codes: Vec<_> = diagnostics
        .iter()
        .filter_map(|diagnostic| match &diagnostic.code {
            Some(NumberOrString::String(code)) => Some(code.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        codes.contains(&"unknown-type"),
        "an unknown type name is a syntax fault and is not gated: {codes:?}"
    );
    assert!(
        !codes.contains(&"let-type"),
        "the gated type checks must be off, or this test proves nothing: {codes:?}"
    );
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

/// PR #18 review: singular/plural sibling tables are a naming
/// convention, not typos — `orders` next to explicit `order` must not
/// warn (and must not offer a quick fix that rewrites the query
/// against a different real table).
#[tokio::test]
async fn sibling_singular_plural_tables_are_not_typos() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "plural.surql",
        "DEFINE TABLE order SCHEMAFULL;\nCREATE orders SET total = 1;",
    )
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("plural.surql"))
        .expect("published");
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != Some(NumberOrString::String("unknown-table".to_string()))
        }),
        "plural sibling of an explicit table must not be flagged: {diagnostics:?}"
    );
}

/// The plural guard must not swallow real typos of s-ending names:
/// `address` pluralises with `es`, so `addres` is a dropped letter,
/// not a singular sibling.
#[tokio::test]
async fn trailing_s_typo_of_s_ending_table_still_warns() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "address.surql",
        "DEFINE TABLE address SCHEMAFULL;\nCREATE addres SET street = 'x';",
    )
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("address.surql"))
        .expect("published");
    let unknown = diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.code == Some(NumberOrString::String("unknown-table".to_string()))
        })
        .expect("dropped-letter typo of an s-ending table must still warn");
    assert!(unknown.message.contains("Did you mean `address`?"));
}

/// PR #18 review: a name used in several statements is a deliberate
/// (if undeclared) table. Trade-off documented here: the same typo
/// pasted twice also goes silent.
#[tokio::test]
async fn repeated_usage_of_inferred_table_is_not_a_typo() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "repeated.surql",
        "DEFINE TABLE person SCHEMAFULL;\n\
         CREATE prson SET x = 1;\n\
         SELECT * FROM prson;",
    )
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("repeated.surql"))
        .expect("published");
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != Some(NumberOrString::String("unknown-table".to_string()))
        }),
        "multi-use inferred names are deliberate tables: {diagnostics:?}"
    );
}

/// PR #18 review: when the DB connection is down, remote tables drop
/// out of the merged model and local near-misses would warn in bulk —
/// right when the metadata-unavailable toast already fires. Typo
/// detection must stand down while metadata is degraded.
#[tokio::test]
async fn typo_detection_suppressed_while_metadata_unavailable() {
    use surrealql_language_server::semantic::types::LiveMetadataSnapshot;

    let failing = LiveMetadataSnapshot {
        documents: Default::default(),
        errors: vec!["failed to connect to SurrealDB: connection refused".to_string()],
    };
    let (core, notifier, _) = core_with(Default::default(), failing);

    // The failing snapshot only reaches the model through a fetch —
    // drive the real initialize flow, not just did_open.
    core.initialize(InitializeParams::default()).await;
    core.initialized().await;
    open(
        &core,
        "degraded.surql",
        "DEFINE TABLE person SCHEMAFULL;\nCREATE prson SET x = 1;",
    )
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("degraded.surql"))
        .expect("published");
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic.code != Some(NumberOrString::String("unknown-table".to_string()))
        }),
        "typo detection must stand down while metadata is degraded: {diagnostics:?}"
    );
    assert!(
        notifier
            .shows()
            .iter()
            .any(|(_, message)| message.contains("live schema metadata unavailable")),
        "the outage itself is still reported"
    );
}

/// PR #18 review: a persistently bad configuration must not re-log
/// the same warnings on every configuration push.
#[tokio::test]
async fn settings_warnings_do_not_repeat() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    core.initialize(InitializeParams::default()).await;

    let bad_payload = json!({ "surrealql": { "metadata": { "mode": "workspaceanddb" } } });
    core.did_change_configuration(DidChangeConfigurationParams {
        settings: bad_payload.clone(),
    })
    .await;
    core.did_change_configuration(DidChangeConfigurationParams {
        settings: bad_payload,
    })
    .await;

    let warning_count = notifier
        .logs()
        .iter()
        .filter(|(level, message)| {
            *level == MessageType::WARNING && message.starts_with("SurrealQL settings:")
        })
        .count();
    assert_eq!(
        warning_count,
        1,
        "identical warning sets must log once: {:?}",
        notifier.logs()
    );

    // A clean payload resolves the warnings — once.
    core.did_change_configuration(DidChangeConfigurationParams {
        settings: json!({ "surrealql": {} }),
    })
    .await;
    assert!(
        notifier.logs().iter().any(|(level, message)| {
            *level == MessageType::INFO && message.contains("previous warnings resolved")
        }),
        "recovery must be logged: {:?}",
        notifier.logs()
    );
}

/// A client with no `surrealql` workspace section answers the
/// configuration pull with `None` (unsupported) or JSON `null`
/// (VS Code / Neovim) — neither must reset the warning-dedup state
/// nor fire a spurious "resolved" line right after the
/// initializationOptions warnings were logged.
#[tokio::test]
async fn configless_pull_does_not_resolve_init_options_warnings() {
    for pulled in [None, Some(serde_json::Value::Null)] {
        let (core, notifier, _) = core_with(Default::default(), Default::default());
        *notifier.configuration.lock().unwrap() = pulled;

        core.initialize(InitializeParams {
            initialization_options: Some(json!({
                "surrealql": { "connection": { "endpint": "ws://x:8000/rpc" } }
            })),
            ..InitializeParams::default()
        })
        .await;
        core.initialized().await;

        let logs = notifier.logs();
        let warning_count = logs
            .iter()
            .filter(|(level, message)| {
                *level == MessageType::WARNING && message.contains("endpint")
            })
            .count();
        assert_eq!(warning_count, 1, "warning logged exactly once: {logs:?}");
        assert!(
            logs.iter()
                .all(|(_, message)| !message.contains("previous warnings resolved")),
            "a config-less pull must not fake a resolution: {logs:?}"
        );
    }
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

#[tokio::test]
async fn namespace_completion_comes_from_the_generated_catalogue() {
    // The hand-written list this replaced offered `not::` and `sleep::`, which
    // are bare functions and not namespaces, and hid eight real ones. `set::`
    // was the costly omission: 24 functions the method checker already resolved
    // but completion never offered.
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "RETURN ";
    open(&core, "a.surql", text).await;

    let items = complete(&core, "a.surql", 0, text.len() as u32).await;
    let offered = labels(&items);

    for real in [
        "set::",
        "file::",
        "bytes::",
        "api::",
        "value::",
        "eval::",
        "schema::",
        "sequence::",
    ] {
        assert!(offered.contains(&real), "`{real}` must be offered");
    }
    for phantom in ["not::", "sleep::"] {
        assert!(
            !offered.contains(&phantom),
            "`{phantom}` is a bare function, not a namespace"
        );
    }
    // The ones that already worked must keep working.
    for kept in ["string::", "array::", "math::", "type::"] {
        assert!(offered.contains(&kept), "`{kept}` regressed");
    }
}

#[tokio::test]
async fn a_dot_on_a_typed_value_offers_that_receivers_methods() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "RETURN \"abc\".";
    open(&core, "a.surql", text).await;

    let items = complete(&core, "a.surql", 0, text.len() as u32).await;
    let offered = labels(&items);

    for string_method in ["len", "slug", "uppercase", "split", "is_alphanum"] {
        assert!(
            offered.contains(&string_method),
            "`{string_method}` missing"
        );
    }
    // A method that belongs to another receiver must not be offered.
    for foreign in ["area", "centroid", "days"] {
        assert!(
            !offered.contains(&foreign),
            "`{foreign}` is not a string method"
        );
    }
    // The shared block reaches every receiver, so these are string methods too.
    for shared in ["to_string", "is_number", "chain"] {
        assert!(offered.contains(&shared), "`{shared}` missing");
    }
}

#[tokio::test]
async fn a_dot_on_a_number_offers_the_math_methods() {
    // The gap the CHANGELOG named: `<number>.round()` is `math::round`, so the
    // old `<receiver>::<method>` guess offered nothing here.
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "LET $n = 5;\nRETURN $n.";
    open(&core, "a.surql", text).await;

    let items = complete(&core, "a.surql", 1, 10).await;
    let offered = labels(&items);
    for numeric in ["round", "abs", "floor", "ceil"] {
        assert!(offered.contains(&numeric), "`{numeric}` missing");
    }
}

#[tokio::test]
async fn a_dot_on_a_variable_no_longer_answers_with_an_empty_list() {
    // `$s.` used to read `s` as a *table* name, find no fields on it, and return
    // an empty popup — the sharpest completion defect in the server.
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "LET $s = \"abc\";\nRETURN $s.";
    open(&core, "a.surql", text).await;

    let items = complete(&core, "a.surql", 1, 10).await;
    assert!(!items.is_empty(), "the popup must not be empty");
    assert!(labels(&items).contains(&"len"), "got {:?}", labels(&items));
}

#[tokio::test]
async fn a_dot_on_an_untyped_receiver_falls_back_to_every_method() {
    // Field access types as `unknown`, which is common. An empty list there would
    // read as a broken feature, so every method is offered instead.
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "RETURN $row.field.";
    open(&core, "a.surql", text).await;

    let items = complete(&core, "a.surql", 0, text.len() as u32).await;
    let offered = labels(&items);
    assert!(
        offered.contains(&"len"),
        "got {:?}",
        &offered[..offered.len().min(12)]
    );
    assert!(
        offered.contains(&"round"),
        "the fallback spans every receiver"
    );
}

#[tokio::test]
async fn signature_help_works_for_a_method() {
    // `'abc'.slice(` reads as one whitespace-delimited token, so this used to
    // match nothing in either function table.
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "RETURN 'abc'.slice(";
    open(&core, "a.surql", text).await;

    let help = signature_help_at(&core, "a.surql", 0, text.len() as u32).await;
    let label = help.signatures[0].label.clone();
    assert!(label.starts_with(".slice("), "got {label}");
    // The receiver fills parameter zero, so it must not be listed.
    assert!(
        !label.contains("string,"),
        "the receiver must be dropped: {label}"
    );
}

#[tokio::test]
async fn a_namespace_prefix_offers_the_functions_inside_it() {
    // The reported defect: typing `rand::` offered the namespace and then
    // nothing inside it. Completion iterated only the 79 *curated* builtins,
    // which cover `string::` and `type::` alone — so 355 of the 434 functions
    // the parser accepts were invisible, and every other namespace resolved
    // empty.
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "RETURN rand::";
    open(&core, "a.surql", text).await;

    let items = complete(&core, "a.surql", 0, text.len() as u32).await;
    let offered = labels(&items);
    for name in [
        "rand::uuid::v4",
        "rand::uuid::v7",
        "rand::uuid",
        "rand::bool",
        "rand::int",
        "rand::time",
    ] {
        assert!(offered.contains(&name), "`{name}` missing from {offered:?}");
    }
}

#[tokio::test]
async fn every_namespace_resolves_to_its_functions() {
    // Not just `rand::` — the same hole emptied every namespace outside the two
    // the curated table happens to cover.
    let (core, _, _) = core_with(Default::default(), Default::default());
    for (prefix, expected) in [
        ("array::", "array::distinct"),
        ("math::", "math::round"),
        ("time::", "time::now"),
        ("crypto::", "crypto::sha256"),
        ("vector::", "vector::dot"),
        ("duration::", "duration::days"),
        ("set::", "set::union"),
    ] {
        let text = format!("RETURN {prefix}");
        open(&core, "a.surql", &text).await;
        let items = complete(&core, "a.surql", 0, text.len() as u32).await;
        assert!(
            labels(&items).contains(&expected),
            "`{expected}` missing after typing `{prefix}`"
        );
    }
}

#[tokio::test]
async fn a_builtin_constant_is_offered() {
    // `math::PI` takes no arguments, so it is not a function entry at all.
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "RETURN math::P";
    open(&core, "a.surql", text).await;

    let items = complete(&core, "a.surql", 0, text.len() as u32).await;
    assert!(
        labels(&items).contains(&"math::PI"),
        "got {:?}",
        labels(&items)
    );
}

#[tokio::test]
async fn a_curated_function_keeps_its_prose() {
    // The generated catalogue must not shadow the 79 entries that carry a
    // summary and a docs link.
    let (core, _, _) = core_with(Default::default(), Default::default());
    let text = "RETURN string::len";
    open(&core, "a.surql", text).await;

    let items = complete(&core, "a.surql", 0, text.len() as u32).await;
    let entry = items
        .iter()
        .find(|item| item.label == "string::len")
        .expect("string::len offered");
    assert!(
        entry.documentation.is_some(),
        "a curated entry keeps its prose"
    );
    assert_eq!(
        items
            .iter()
            .filter(|item| item.label == "string::len")
            .count(),
        1,
        "and is offered exactly once"
    );
}

// ──────────────────────────────────────────────────────────────────────
// analysis.maxSyntaxDiagnostics
// ──────────────────────────────────────────────────────────────────────

fn syntax_diagnostic_count(diagnostics: &[Diagnostic]) -> usize {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == Some(NumberOrString::String("parse".to_string())))
        .count()
}

/// A document with far more than a hundred parse errors used to publish
/// exactly 100 and stop, which read as the server having given up.
#[tokio::test]
async fn a_document_past_the_old_cap_publishes_past_a_hundred() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    open(&core, "many.surql", &"@@@ ;\n".repeat(500)).await;

    let diagnostics = notifier
        .last_published_for(&uri("many.surql"))
        .expect("published");
    assert!(
        syntax_diagnostic_count(&diagnostics) > 100,
        "got {}",
        syntax_diagnostic_count(&diagnostics)
    );
}

#[tokio::test]
async fn the_syntax_cap_is_configurable() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    core.did_change_configuration(DidChangeConfigurationParams {
        settings: json!({ "surrealql": { "analysis": { "maxSyntaxDiagnostics": 5 } } }),
    })
    .await;

    open(&core, "capped.surql", &"@@@ ;\n".repeat(500)).await;

    let diagnostics = notifier
        .last_published_for(&uri("capped.surql"))
        .expect("published");
    assert_eq!(syntax_diagnostic_count(&diagnostics), 5, "{diagnostics:?}");
}

/// The cap is applied while the tree is walked, so a document analyzed under
/// the old value has to be re-analyzed when the value moves. Without that,
/// raising the cap appears to do nothing until the buffer is edited.
#[tokio::test]
async fn raising_the_cap_reanalyzes_already_open_documents() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    core.did_change_configuration(DidChangeConfigurationParams {
        settings: json!({ "surrealql": { "analysis": { "maxSyntaxDiagnostics": 5 } } }),
    })
    .await;
    open(&core, "grow.surql", &"@@@ ;\n".repeat(500)).await;
    assert_eq!(
        syntax_diagnostic_count(
            &notifier
                .last_published_for(&uri("grow.surql"))
                .expect("published")
        ),
        5
    );

    core.did_change_configuration(DidChangeConfigurationParams {
        settings: json!({ "surrealql": { "analysis": { "maxSyntaxDiagnostics": 60 } } }),
    })
    .await;

    let diagnostics = notifier
        .last_published_for(&uri("grow.surql"))
        .expect("published");
    assert_eq!(
        syntax_diagnostic_count(&diagnostics),
        60,
        "the open buffer must be re-analyzed under the new cap: {diagnostics:?}"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Debounce and edit supersession
// ──────────────────────────────────────────────────────────────────────

/// Settings with an explicit debounce. `0` makes a change synchronous, which is
/// what the ordering tests want; a real value exercises the coalescing.
fn settings_with_debounce(ms: u64) -> ServerSettings {
    let mut settings = ServerSettings::default();
    settings.analysis.diagnostic_debounce_ms = ms;
    settings
}

/// A `didChange` carrying one full-document replacement, as the server
/// advertises `TextDocumentSyncKind::FULL`.
fn change(path: &str, version: i32, text: &str) -> DidChangeTextDocumentParams {
    DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri(path),
            version,
        },
        content_changes: vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_string(),
        }],
    }
}

/// The table name the open buffer currently defines, read through the real
/// `documentSymbol` handler. Each version writes a different name, so this says
/// which analysis is the one the server is serving.
async fn defined_table(core: &common::TestCore, path: &str) -> Option<String> {
    let response = core
        .document_symbol(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri(path) },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await?;
    let DocumentSymbolResponse::Nested(symbols) = response else {
        return None;
    };
    symbols.first().map(|symbol| symbol.name.clone())
}

/// Edits arriving faster than the debounce must collapse to one analysis.
///
/// The edits are *staggered* deliberately. Spawned all at once they would
/// coalesce through version supersession alone — a later edit records its
/// version before an earlier one gets to publish — and the test would pass with
/// the debounce switched off, proving nothing about it. Staggering them by less
/// than the window means each edit is the newest when it starts, so only the
/// wait can collapse them.
#[tokio::test]
async fn edits_faster_than_the_debounce_collapse_to_one_analysis() {
    const DEBOUNCE_MS: u64 = 300;
    const STAGGER_MS: u64 = 30;

    let (core, notifier, _) = common::core_with(Default::default(), Default::default());
    let core = std::sync::Arc::new(core);
    core.apply_settings(settings_with_debounce(DEBOUNCE_MS))
        .await;
    open(&core, "burst.surql", "DEFINE TABLE t1 SCHEMAFULL;").await;
    let after_open = notifier.published().len();

    let mut handles = Vec::new();
    for version in 2..=6 {
        let core = std::sync::Arc::clone(&core);
        handles.push(tokio::spawn(async move {
            // Arrive while the previous edit is still inside its window.
            tokio::time::sleep(std::time::Duration::from_millis(
                STAGGER_MS * u64::from(version as u32 - 2),
            ))
            .await;
            core.did_change(change(
                "burst.surql",
                version,
                &format!("DEFINE TABLE t{version} SCHEMAFULL;"),
            ))
            .await;
        }));
    }
    for handle in handles {
        handle.await.expect("no panic");
    }

    let publishes = notifier.published().len() - after_open;
    assert_eq!(
        publishes, 1,
        "5 edits inside one {DEBOUNCE_MS} ms window published {publishes} times; \
         the debounce did not collapse them"
    );
    assert_eq!(
        defined_table(&core, "burst.surql").await.as_deref(),
        Some("TABLE t6"),
        "the surviving analysis must be the newest edit"
    );
}

/// An out-of-order edit must not overwrite a newer one. Spawning `did_change`
/// in the native adapter makes this reachable — two edits can be in flight at
/// once — so the core carries a version per document and drops the stale one.
#[tokio::test]
async fn a_stale_change_does_not_overwrite_a_newer_one() {
    let (core, notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(&core, "order.surql", "DEFINE TABLE t1 SCHEMAFULL;").await;

    // Version 9 arrives, then version 3. The older one must be discarded.
    core.did_change(change("order.surql", 9, "DEFINE TABLE t9 SCHEMAFULL;"))
        .await;
    core.did_change(change("order.surql", 3, "DEFINE TABLE t3 SCHEMAFULL;"))
        .await;

    assert_eq!(
        defined_table(&core, "order.surql").await.as_deref(),
        Some("TABLE t9"),
        "an older version overwrote a newer one"
    );
    assert!(
        !notifier.published().is_empty(),
        "the newer version must still have published"
    );
}

/// Reopening a file must not freeze its diagnostics.
///
/// `document_versions` recorded a high-water mark per URI and `did_close`
/// removed the document but not its version. A client that restarts versioning
/// on reopen (VS Code does), then sent `didChange` at version 2 against a
/// remembered 57, and `upsert_open_document` dropped it as stale. Every edit
/// after that was dropped too, so the buffer showed the diagnostics it had when
/// it was opened and never updated again.
#[tokio::test]
async fn reopening_a_file_does_not_freeze_its_diagnostics() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;

    // Edit it up to a high version, the way a real session does.
    open(&core, "reopen.surql", "DEFINE TABLE t1 SCHEMAFULL;").await;
    core.did_change(change("reopen.surql", 57, "DEFINE TABLE t57 SCHEMAFULL;"))
        .await;
    assert_eq!(
        defined_table(&core, "reopen.surql").await.as_deref(),
        Some("TABLE t57"),
    );

    core.did_close(DidCloseTextDocumentParams {
        text_document: TextDocumentIdentifier {
            uri: uri("reopen.surql"),
        },
    })
    .await;

    // The client reopens and starts counting again from 1.
    open(&core, "reopen.surql", "DEFINE TABLE fresh1 SCHEMAFULL;").await;
    core.did_change(change("reopen.surql", 2, "DEFINE TABLE fresh2 SCHEMAFULL;"))
        .await;

    assert_eq!(
        defined_table(&core, "reopen.surql").await.as_deref(),
        Some("TABLE fresh2"),
        "the edit after reopening was dropped as stale against the version the \
         file had before it was closed"
    );
}

/// `didOpen` is never delayed. The file just appeared and the user is waiting to
/// see what is wrong with it.
#[tokio::test]
async fn did_open_is_not_debounced() {
    let (core, notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(10_000)).await;

    let started = std::time::Instant::now();
    open(&core, "immediate.surql", "SELECT * FROM;").await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "didOpen waited {elapsed:?}; it must not go through the debounce"
    );
    assert!(
        !notifier.published().is_empty(),
        "didOpen must publish diagnostics"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Graph traversals
//
// The reported defect: `SELECT ->is_friends_with->person AS friends FROM
// person` resolved nothing. Two separate causes met here — `is_token_char`
// accepts `-`, `<` and `>`, so a traversal scanned as one token and neither
// hover nor the completion prefix could see the names in it; and nothing in
// the model knew which tables an edge joins.
// ──────────────────────────────────────────────────────────────────────

/// A schema that declares one edge and one unrelated table, so a test can tell
/// ranking apart from "everything happens to be an edge".
const GRAPH_SCHEMA: &str = concat!(
    "DEFINE TABLE person SCHEMAFULL;\n",
    "DEFINE TABLE unrelated SCHEMAFULL;\n",
    "DEFINE TABLE is_friends_with TYPE RELATION IN person OUT person;\n",
);

async fn hover_text(core: &common::TestCore, path: &str, line: u32, character: u32) -> String {
    let hover = core
        .hover(tower_lsp_server::ls_types::HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri(path) },
                position: Position::new(line, character),
            },
            work_done_progress_params: Default::default(),
        })
        .await;
    format!("{hover:?}")
}

/// The exact query from the report. The edge name must resolve to its table.
#[tokio::test]
async fn an_edge_name_in_a_traversal_resolves_to_its_table() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let query = "SELECT ->is_friends_with->person AS friends FROM person WHERE friends.length > 0;";
    open(&core, "a.surql", &format!("{GRAPH_SCHEMA}{query}")).await;

    // Column 12 is inside `is_friends_with` on the query line.
    let rendered = hover_text(&core, "a.surql", 3, 12).await;
    assert!(
        rendered.contains("is_friends_with"),
        "hovering the edge must resolve it, got {rendered}"
    );
}

/// Go-to-definition on the edge name must reach its `DEFINE TABLE`.
#[tokio::test]
async fn go_to_definition_reaches_an_edge_table() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        &format!("{GRAPH_SCHEMA}SELECT ->is_friends_with->person AS f FROM person;"),
    )
    .await;

    let found = core
        .goto_definition(tower_lsp_server::ls_types::GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri("a.surql"),
                },
                position: Position::new(3, 12),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await;

    let rendered = format!("{found:?}");
    assert!(
        rendered.contains("line: 2"),
        "must jump to the `DEFINE TABLE is_friends_with` on line 2, got {rendered}"
    );
}

/// The core of the request: after `->`, the edges connected to the statement's
/// table come first.
#[tokio::test]
async fn a_connected_edge_is_ranked_first_after_an_arrow() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let query = "SELECT -> FROM person;";
    open(&core, "a.surql", &format!("{GRAPH_SCHEMA}{query}")).await;

    // Just after the `->`.
    let items = complete(&core, "a.surql", 3, 9).await;

    assert_eq!(
        labels(&items).first(),
        Some(&"is_friends_with"),
        "the edge that leaves `person` must lead, got {:?}",
        labels(&items)
    );
    assert!(
        labels(&items).contains(&"unrelated"),
        "an unconnected table is ranked lower, not hidden: {:?}",
        labels(&items)
    );
}

/// The popup must not come back empty. Before the prefix fix, every builder
/// filtered its candidates against the literal string `person->`.
#[tokio::test]
async fn completion_after_an_arrow_is_not_empty() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let query = "SELECT * FROM person->";
    open(&core, "a.surql", &format!("{GRAPH_SCHEMA}{query}")).await;

    let items = complete(&core, "a.surql", 3, 22).await;

    assert!(!items.is_empty(), "the list must not be empty");
    assert_eq!(
        labels(&items).first(),
        Some(&"is_friends_with"),
        "got {:?}",
        labels(&items)
    );
}

/// A written base anchors on itself, so the ranking works without a `FROM`.
#[tokio::test]
async fn a_written_base_anchors_the_ranking() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let query = "SELECT person->";
    open(&core, "a.surql", &format!("{GRAPH_SCHEMA}{query}")).await;

    let items = complete(&core, "a.surql", 3, query.len() as u32).await;
    assert_eq!(
        labels(&items).first(),
        Some(&"is_friends_with"),
        "got {:?}",
        labels(&items)
    );
}

/// The second hop leaves the edge, so it must offer the tables that edge
/// reaches — not the edges again.
#[tokio::test]
async fn the_second_hop_offers_the_tables_the_edge_reaches() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let before_cursor = "SELECT ->is_friends_with->";
    open(
        &core,
        "a.surql",
        &format!("{GRAPH_SCHEMA}{before_cursor} FROM person;"),
    )
    .await;

    let items = complete(&core, "a.surql", 3, before_cursor.len() as u32).await;
    assert_eq!(
        labels(&items).first(),
        Some(&"person"),
        "`is_friends_with` points at `person`, got {:?}",
        labels(&items)
    );
}

/// With no table after `FROM` there is nothing to rank against, so the list
/// falls back to the plain table order rather than inventing a winner. This is
/// the "only when a table is defined after FROM" half of the request.
#[tokio::test]
async fn without_a_from_table_no_edge_is_promoted() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", &format!("{GRAPH_SCHEMA}SELECT ->")).await;

    let items = complete(&core, "a.surql", 3, 9).await;

    assert!(!items.is_empty(), "tables are still offered");
    assert!(
        items
            .iter()
            .all(|item| item.sort_text.as_deref() != Some("0-0-is_friends_with")),
        "nothing may be promoted without an anchor: {:?}",
        items
            .iter()
            .map(|item| (&item.label, &item.sort_text))
            .collect::<Vec<_>>()
    );
}

/// An edge that only a `RELATE` witnesses ranks exactly like a declared one.
/// This is the case SurrealDB's own graph corpus is written in.
#[tokio::test]
async fn an_edge_known_only_from_relate_is_ranked_too() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        concat!(
            "DEFINE TABLE person SCHEMALESS;\n",
            "DEFINE TABLE unrelated SCHEMALESS;\n",
            "RELATE person:a->knows->person:b;\n",
            "SELECT -> FROM person;",
        ),
    )
    .await;

    let items = complete(&core, "a.surql", 3, 9).await;
    assert_eq!(
        labels(&items).first(),
        Some(&"knows"),
        "got {:?}",
        labels(&items)
    );
}

/// A traversal in a target position must not draw an `unknown-table` warning
/// on the edge it passes through.
#[tokio::test]
async fn a_traversal_target_draws_no_unknown_table_warning() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        &format!("{GRAPH_SCHEMA}SELECT * FROM person->is_friends_with->person;"),
    )
    .await;

    let published = notifier.published();
    let codes: Vec<String> = published
        .iter()
        .flat_map(|(_, diagnostics)| diagnostics.iter())
        .filter_map(|diagnostic| match &diagnostic.code {
            Some(NumberOrString::String(code)) => Some(code.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !codes.iter().any(|code| code == "unknown-table"),
        "a traversal must not report an unknown table, got {codes:?}"
    );
}

/// `->` is also the return arrow of `DEFINE FUNCTION`. The graph branch runs
/// before the other classifiers, so this pins what it does there.
///
/// A type is wanted, not a table — but the server has never modelled that slot,
/// and before the prefix fix it answered with an *empty* popup, because every
/// builder filtered against the literal string `->`. Offering the tables is no
/// worse, and `record<…>` types do name tables. What matters is that the list
/// is not empty and nothing is falsely promoted.
#[tokio::test]
async fn the_function_return_arrow_is_not_hijacked() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let head = "DEFINE FUNCTION fn::x() ->";
    open(&core, "a.surql", &format!("{GRAPH_SCHEMA}{head}")).await;

    let items = complete(&core, "a.surql", 3, head.len() as u32).await;

    assert!(
        items
            .iter()
            .all(|item| item.sort_text.as_deref() != Some("0-0-is_friends_with")),
        "no edge may be promoted here — there is no table to traverse from"
    );
}

/// With a space after it, the return arrow is not a graph slot at all, so the
/// pre-existing behaviour is untouched.
#[tokio::test]
async fn a_space_after_an_arrow_ends_the_graph_slot() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let head = "SELECT * FROM person-> ";
    open(&core, "a.surql", &format!("{GRAPH_SCHEMA}{head}")).await;

    let items = complete(&core, "a.surql", 3, head.len() as u32).await;
    assert!(
        items
            .iter()
            .all(|item| item.sort_text.as_deref() != Some("0-0-is_friends_with")),
        "the hop is over once a space follows the arrow"
    );
}

/// Items after an arrow must say exactly which characters they replace.
///
/// Without a `textEdit` the client decides the range from its own word scan.
/// That is safe everywhere else in this handler — a space or a `.` precedes the
/// cursor — but after `->` a client whose word pattern admits `-` or `>` reads
/// the prefix as `person->`, matches no item against it, and shows an empty
/// popup no matter what the server returned. Ctrl+Space does not help, because
/// the filtering happens after the response arrives.
#[tokio::test]
async fn graph_items_carry_an_explicit_replace_range() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let head = "SELECT * FROM person->";
    open(&core, "a.surql", &format!("{GRAPH_SCHEMA}{head}")).await;

    let items = complete(&core, "a.surql", 3, head.len() as u32).await;
    let first = items.first().expect("at least one item");

    let Some(tower_lsp_server::ls_types::CompletionTextEdit::Edit(edit)) = &first.text_edit else {
        panic!(
            "a graph item must carry a plain textEdit, got {:?}",
            first.text_edit
        );
    };
    assert_eq!(edit.new_text, "is_friends_with");
    assert_eq!(
        (edit.range.start.line, edit.range.start.character),
        (3, head.len() as u32),
        "nothing is typed yet, so the edit inserts at the cursor"
    );
    assert_eq!(edit.range.end, edit.range.start);
    assert_eq!(
        first.filter_text.as_deref(),
        Some("is_friends_with"),
        "the client must filter on the name, not on the text it scanned"
    );
}

/// With a half-typed name the range must cover that name — and only it, not
/// the arrow before it.
#[tokio::test]
async fn the_replace_range_covers_the_typed_hop_only() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let head = "SELECT * FROM person->is_fr";
    open(&core, "a.surql", &format!("{GRAPH_SCHEMA}{head}")).await;

    let items = complete(&core, "a.surql", 3, head.len() as u32).await;
    let first = items.first().expect("at least one item");

    let Some(tower_lsp_server::ls_types::CompletionTextEdit::Edit(edit)) = &first.text_edit else {
        panic!("expected a plain textEdit");
    };
    assert_eq!(
        (edit.range.start.character, edit.range.end.character),
        // `is_fr` is five characters, and the arrow before it is untouched.
        (head.len() as u32 - 5, head.len() as u32),
        "the edit must replace `is_fr` and leave `->` alone"
    );
    assert_eq!(edit.new_text, "is_friends_with");
}

/// Every item in the list gets the range, not only the promoted ones —
/// otherwise picking an ordinary table would insert at the wrong place.
#[tokio::test]
async fn every_graph_item_carries_the_range() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let head = "SELECT * FROM person->";
    open(&core, "a.surql", &format!("{GRAPH_SCHEMA}{head}")).await;

    let items = complete(&core, "a.surql", 3, head.len() as u32).await;
    assert!(items.len() > 1, "expected more than the promoted edge");
    assert!(
        items.iter().all(|item| item.text_edit.is_some()),
        "an item without a range would insert in the wrong place"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Column completion in a projection list
//
// Only the *first* column of a `SELECT` or `SET` list ever completed. The scan
// that skips backwards over already-written columns read straight through the
// opening keyword, so after a comma the slot went unrecognised and the position
// fell through to the whole ~765-item catalogue.
// ──────────────────────────────────────────────────────────────────────

/// A schema with one plain table, one nested field, and one edge.
const FIELD_SCHEMA: &str = concat!(
    "DEFINE TABLE person SCHEMAFULL;\n",
    "DEFINE FIELD name ON person TYPE string;\n",
    "DEFINE FIELD address.street ON person TYPE string;\n",
);

/// Every column position in a projection list offers the same columns.
#[tokio::test]
async fn every_column_position_in_a_select_list_completes() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let query = "SELECT id, name, address.street FROM person;";
    open(&core, "a.surql", &format!("{FIELD_SCHEMA}{query}")).await;

    for (what, character) in [
        ("after `SELECT `", 7),
        ("right after the first `,`", 10),
        ("after `, `", 11),
        ("after the second `, `", 17),
    ] {
        let items = complete(&core, "a.surql", 3, character).await;
        let found = labels(&items);
        for column in ["id", "name", "address.street"] {
            assert!(
                found.contains(&column),
                "{what}: `{column}` must be offered, got {found:?}"
            );
        }
        assert!(
            !found.iter().any(|label| label.starts_with('$')),
            "{what}: a column slot offers no variables, got {found:?}"
        );
    }
}

/// The same for a `SET` list, which shares the scan.
#[tokio::test]
async fn every_column_position_in_a_set_list_completes() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    // The value is a number on purpose. A quote anywhere in an already-written
    // item aborts the backward scan — a pre-existing guard, because a backward
    // reader cannot tell an opening quote from a closing one — so
    // `SET name = 'a', ` degrades to the full list. That is a separate
    // limitation from the one this test covers.
    let query = "UPDATE person SET age = 29, ";
    open(&core, "a.surql", &format!("{FIELD_SCHEMA}{query}")).await;

    let items = complete(&core, "a.surql", 3, query.len() as u32).await;

    let found = labels(&items);
    assert!(
        found.contains(&"name"),
        "a `SET` list must keep completing after a comma, got {found:?}"
    );
    assert!(!found.contains(&"*"), "`SET` takes no `*`, got {found:?}");
}

/// A table named `preset` ends in the letters of `SET`. The keyword check is
/// word-bounded so it does not read as one.
#[tokio::test]
async fn a_name_ending_in_a_keyword_is_not_read_as_one() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let query = "SELECT name, preset, ";
    open(
        &core,
        "a.surql",
        &format!("{FIELD_SCHEMA}{query} FROM person;"),
    )
    .await;

    let items = complete(&core, "a.surql", 3, query.len() as u32).await;

    let found = labels(&items);
    assert!(
        found.contains(&"*"),
        "the list still belongs to `SELECT`, not to a `SET` found inside `preset`: {found:?}"
    );
}

/// `id` exists on every record and no `DEFINE FIELD` declares it, so nothing
/// used to offer it — on the single most common projection in SurrealQL.
#[tokio::test]
async fn the_implicit_id_column_is_offered() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        &format!("{FIELD_SCHEMA}SELECT  FROM person;"),
    )
    .await;

    let items = complete(&core, "a.surql", 3, 7).await;
    let id = items
        .iter()
        .find(|item| item.label == "id")
        .expect("`id` must be offered");
    assert_eq!(
        id.kind,
        Some(tower_lsp_server::ls_types::CompletionItemKind::FIELD)
    );
    assert!(
        !labels(&items)
            .iter()
            .any(|label| *label == "in" || *label == "out"),
        "a plain table has no `in` / `out`"
    );
}

/// An edge table carries `in` and `out` as well, written by `RELATE`.
#[tokio::test]
async fn an_edge_table_offers_its_endpoints_as_columns() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        concat!(
            "DEFINE TABLE knows TYPE RELATION IN person OUT person;\n",
            "SELECT  FROM knows;",
        ),
    )
    .await;

    let items = complete(&core, "a.surql", 1, 7).await;

    let found = labels(&items);
    for column in ["id", "in", "out"] {
        assert!(
            found.contains(&column),
            "an edge must offer `{column}`, got {found:?}"
        );
    }
}

/// A declared column of the same name wins, so it is not listed twice.
#[tokio::test]
async fn a_declared_id_is_not_duplicated_by_the_implicit_one() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        concat!(
            "DEFINE TABLE person SCHEMAFULL;\n",
            "DEFINE FIELD id ON person TYPE string;\n",
            "SELECT  FROM person;",
        ),
    )
    .await;

    let items = complete(&core, "a.surql", 2, 7).await;

    let found = labels(&items);
    assert_eq!(
        found.iter().filter(|label| **label == "id").count(),
        1,
        "got {found:?}"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Hovering a column
//
// A column name means nothing on its own — `name` belongs to whichever table
// the statement reads — so the global token lookup that answers for tables,
// functions and params could never answer for one. Hovering a column returned
// nothing at all.
// ──────────────────────────────────────────────────────────────────────

const HOVER_SCHEMA: &str = concat!(
    "DEFINE TABLE person SCHEMAFULL;\n",
    "DEFINE FIELD name ON person TYPE string COMMENT 'The display name';\n",
    "DEFINE FIELD address.street ON person TYPE string;\n",
    "DEFINE INDEX person_name ON person FIELDS name UNIQUE;\n",
);

/// Hover text at a column of the line that follows `HOVER_SCHEMA`.
async fn column_hover(core: &common::TestCore, character: u32) -> String {
    hover_text(core, "a.surql", 4, character).await
}

#[tokio::test]
async fn hovering_a_column_shows_what_the_schema_stores() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let query = "SELECT id, name, address.street FROM person;";
    open(&core, "a.surql", &format!("{HOVER_SCHEMA}{query}")).await;

    // A cursor *inside* `name`, which starts at column 11.
    let hover = column_hover(&core, 15).await;
    for expected in [
        "FIELD person.name", // named with its table, as a table hover is
        "The display name",  // the COMMENT
        "Type: `string`",    // the declared type
        "Permissions",       // the same posture line a table hover carries
        "person_name",       // and the index that covers it
    ] {
        assert!(
            hover.contains(expected),
            "hovering `name` must report {expected:?}, got {hover}"
        );
    }
}

/// A nested column is stored under its whole path, and `.` is not a token
/// character — so the segment under the pointer had to be widened to the path
/// or it matched nothing. Either end must give the same answer.
#[tokio::test]
async fn hovering_either_half_of_a_nested_column_resolves_the_whole_path() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let query = "SELECT id, name, address.street FROM person;";
    open(&core, "a.surql", &format!("{HOVER_SCHEMA}{query}")).await;

    // `address` spans 17..24, `street` spans 25..31.
    for (what, character) in [("address", 24), ("street", 28), ("street end", 31)] {
        let hover = column_hover(&core, character).await;
        assert!(
            hover.contains("FIELD person.address.street"),
            "hovering {what} must resolve the whole path, got {hover}"
        );
    }
}

/// `id` exists on every record and no `DEFINE FIELD` declares it.
#[tokio::test]
async fn hovering_the_implicit_id_column_explains_it() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let query = "SELECT id, name, address.street FROM person;";
    open(&core, "a.surql", &format!("{HOVER_SCHEMA}{query}")).await;

    let hover = column_hover(&core, 9).await;
    assert!(hover.contains("FIELD person.id"), "got {hover}");
    assert!(hover.contains("Source: built-in"), "got {hover}");
}

/// Written out as `table.column`, which names its own table rather than
/// borrowing the statement's.
#[tokio::test]
async fn hovering_a_qualified_column_resolves_through_its_table() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        &format!("{HOVER_SCHEMA}SELECT person.name FROM person;"),
    )
    .await;

    let hover = column_hover(&core, 18).await;
    assert!(hover.contains("FIELD person.name"), "got {hover}");
}

/// The table itself must still hover as a table. A column and a table can share
/// a name, and the column wins only where a column is what the pointer is on.
#[tokio::test]
async fn hovering_the_table_still_reports_the_table() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let query = "SELECT id, name, address.street FROM person;";
    open(&core, "a.surql", &format!("{HOVER_SCHEMA}{query}")).await;

    // `person` after `FROM` spans 37..43.
    let hover = column_hover(&core, 43).await;
    assert!(hover.contains("TABLE person"), "got {hover}");
    assert!(!hover.contains("FIELD"), "got {hover}");
}

/// A name that is no column of the statement's table still resolves the way it
/// always did, rather than being claimed as a column.
#[tokio::test]
async fn a_non_column_token_is_unaffected() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        &format!("{HOVER_SCHEMA}SELECT string::len(name) FROM person;"),
    )
    .await;

    // `string::len` spans 7..18.
    let hover = column_hover(&core, 18).await;
    assert!(hover.contains("string::len"), "got {hover}");
}

// ──────────────────────────────────────────────────────────────────────
// Reading a member off a value, end to end
// ──────────────────────────────────────────────────────────────────────

/// The reported query, verbatim.
const ROW_QUERY: &str = concat!(
    "DEFINE TABLE person SCHEMAFULL;\n",                   // 0
    "DEFINE FIELD name ON person TYPE string;\n",          // 1
    "DEFINE FIELD age ON person TYPE int;\n",              // 2
    "LET $people = (SELECT id, name, age FROM person);\n", // 3
    "FOR $person IN $people {\n",                          // 4
    "    LET $upper = array::at([], $person.age);\n",      // 5
    "};\n",                                                // 6
);

/// `$person.age` spans a known column of the row the query built.
#[tokio::test]
async fn hovering_a_property_of_a_bound_row_resolves_it() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", ROW_QUERY).await;

    let line = "    LET $upper = array::at([], $person.age);";
    let age = line.find(".age").expect("the property") + 1;

    // Every column of the word, including the first — which used to be dead
    // because the character before it is the `.`.
    for offset in 0..3u32 {
        let hover = hover_text(&core, "a.surql", 5, age as u32 + offset).await;
        assert!(
            hover.contains("age") && hover.contains("int"),
            "hovering `age` at +{offset} must report its type, got {hover}"
        );
    }
}

/// After the `.`, the row's own columns are what is legal — and they lead.
#[tokio::test]
async fn a_property_completes_after_a_dot_on_a_bound_row() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", ROW_QUERY).await;

    let line = "    LET $upper = array::at([], $person.age);";
    let after_dot = line.find(".age").expect("the property") + 1;

    let items = complete(&core, "a.surql", 5, after_dot as u32).await;
    let found = labels(&items);

    assert_eq!(
        &found[..3],
        &["id", "name", "age"],
        "the row's columns lead, got {found:?}"
    );
    assert!(
        found.len() > 3,
        "methods are still offered below them, got {found:?}"
    );
    assert!(
        !found.iter().any(|label| label.starts_with('$')),
        "a member slot offers no variables, got {found:?}"
    );
}

/// Half-typed, the list narrows to the matching column.
#[tokio::test]
async fn a_half_typed_property_filters_the_members() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(
        &core,
        "a.surql",
        &ROW_QUERY.replace("$person.age", "$person.na"),
    )
    .await;

    let line = "    LET $upper = array::at([], $person.na);";
    let end = line.find(".na").expect("the property") + 3;

    let items = complete(&core, "a.surql", 5, end as u32).await;

    let found = labels(&items);
    assert!(found.contains(&"name"), "got {found:?}");
    assert!(!found.contains(&"age"), "got {found:?}");
}

/// A record points at a declared table, so its columns complete too — and the
/// hover is the schema's, which carries what a bare type cannot.
#[tokio::test]
async fn a_record_valued_variable_completes_and_hovers_its_columns() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let query = "LET $p = (SELECT VALUE id FROM ONLY person);\nLET $n = $p.name;";
    open(
        &core,
        "a.surql",
        &format!("{}{query}", &ROW_QUERY[..ROW_QUERY.find("LET").unwrap()]),
    )
    .await;

    // Line 4 is `LET $n = $p.name;`.
    let line = "LET $n = $p.name;";
    let dot = line.find(".name").expect("the property");

    let items = complete(&core, "a.surql", 4, dot as u32 + 1).await;

    let found = labels(&items);
    assert!(found.contains(&"name"), "got {found:?}");
    assert!(found.contains(&"age"), "got {found:?}");

    let hover = hover_text(&core, "a.surql", 4, dot as u32 + 3).await;
    assert!(
        hover.contains("FIELD person.name"),
        "a record's column hovers as the declared column, got {hover}"
    );
}

/// Hovering the first character of an ordinary word must work too — the same
/// fix, and it was broken for every kind of token, not only properties.
#[tokio::test]
async fn hovering_the_first_character_of_a_word_resolves_it() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    open(&core, "a.surql", ROW_QUERY).await;

    // `person` after `FROM` on line 3.
    let line = "LET $people = (SELECT id, name, age FROM person);";
    let person = line.rfind("person").expect("the table");

    let hover = hover_text(&core, "a.surql", 3, person as u32).await;
    assert!(
        hover.contains("TABLE person"),
        "the first glyph of a word must resolve, got {hover}"
    );
}
