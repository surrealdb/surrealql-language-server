//! End-to-end tests driving [`LanguageServerCore`] through its public
//! API with recording mocks — the same pipeline real clients exercise
//! (didOpen → analysis → merged model → published diagnostics).

mod common;

use common::{core_with, uri};
use serde_json::json;
use tower_lsp_server::ls_types::{
    CompletionItem, CompletionParams, CompletionResponse, DiagnosticSeverity,
    DidChangeConfigurationParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, MessageType, NumberOrString, Position, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams,
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
