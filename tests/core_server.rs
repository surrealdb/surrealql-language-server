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

// ──────────────────────────────────────────────────────────────────────
// Code actions honour the requested range and kinds
// ──────────────────────────────────────────────────────────────────────

/// Request code actions over `range`, optionally narrowed to `only`.
async fn actions_at(
    core: &common::TestCore,
    path: &str,
    range: tower_lsp_server::ls_types::Range,
    only: Option<Vec<tower_lsp_server::ls_types::CodeActionKind>>,
) -> Vec<String> {
    core.code_action(tower_lsp_server::ls_types::CodeActionParams {
        text_document: TextDocumentIdentifier { uri: uri(path) },
        range,
        context: tower_lsp_server::ls_types::CodeActionContext {
            diagnostics: Vec::new(),
            only,
            ..Default::default()
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    })
    .await
    .unwrap_or_default()
    .into_iter()
    .filter_map(|action| match action {
        tower_lsp_server::ls_types::CodeActionOrCommand::CodeAction(action) => Some(action.title),
        _ => None,
    })
    .collect()
}

fn line_range(line: u32) -> tower_lsp_server::ls_types::Range {
    tower_lsp_server::ls_types::Range {
        start: tower_lsp_server::ls_types::Position::new(line, 0),
        end: tower_lsp_server::ls_types::Position::new(line, 0),
    }
}

/// `params.range` was ignored, so a cursor anywhere in a file offered an
/// "Add PERMISSIONS clause" action for *every* permission-less table in it:
/// three lightbulb entries for tables nowhere near the cursor.
#[tokio::test]
async fn code_actions_are_limited_to_the_requested_range() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    open(
        &core,
        "perms.surql",
        "DEFINE TABLE alpha SCHEMAFULL;\nDEFINE TABLE beta SCHEMAFULL;\nDEFINE TABLE gamma SCHEMAFULL;\n",
    )
    .await;

    let on_beta = actions_at(&core, "perms.surql", line_range(1), None).await;
    assert_eq!(
        on_beta,
        vec!["Add PERMISSIONS clause to table `beta`".to_string()],
        "the cursor is on line 2; the other two tables are not offered"
    );

    let whole_file = actions_at(
        &core,
        "perms.surql",
        tower_lsp_server::ls_types::Range {
            start: tower_lsp_server::ls_types::Position::new(0, 0),
            end: tower_lsp_server::ls_types::Position::new(2, 30),
        },
        None,
    )
    .await;
    assert_eq!(
        whole_file.len(),
        3,
        "selecting the whole file still offers all three: {whole_file:?}"
    );
}

/// `context.only` was ignored too, so a client asking for quick fixes got
/// refactors back, which is how a refactor ends up in VS Code's Quick Fix menu.
#[tokio::test]
async fn code_actions_honour_the_requested_kinds() {
    use tower_lsp_server::ls_types::CodeActionKind;

    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    open(&core, "only.surql", "DEFINE TABLE alpha SCHEMAFULL;\n").await;

    let refactors = actions_at(
        &core,
        "only.surql",
        line_range(0),
        Some(vec![CodeActionKind::REFACTOR_REWRITE]),
    )
    .await;
    assert_eq!(refactors.len(), 1, "the PERMISSIONS action is a refactor");

    let quick_fixes = actions_at(
        &core,
        "only.surql",
        line_range(0),
        Some(vec![CodeActionKind::QUICKFIX]),
    )
    .await;
    assert!(
        quick_fixes.is_empty(),
        "a quick-fix request must not return a refactor: {quick_fixes:?}"
    );

    // `refactor` matches `refactor.rewrite`: a requested kind covers the more
    // specific kinds beneath it.
    let umbrella = actions_at(
        &core,
        "only.surql",
        line_range(0),
        Some(vec![CodeActionKind::REFACTOR]),
    )
    .await;
    assert_eq!(
        umbrella.len(),
        1,
        "`refactor` must match `refactor.rewrite`"
    );
}

/// `analysis.enableCodeActions` parsed, validated, serialized and did nothing.
#[tokio::test]
async fn disabling_code_actions_silences_them() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    open(&core, "off.surql", "DEFINE TABLE alpha SCHEMAFULL;\n").await;
    assert_eq!(
        actions_at(&core, "off.surql", line_range(0), None)
            .await
            .len(),
        1
    );

    let mut settings = ServerSettings::default();
    settings.analysis.enable_code_actions = false;
    core.apply_settings(settings).await;

    assert!(
        actions_at(&core, "off.surql", line_range(0), None)
            .await
            .is_empty(),
        "the setting is advertised; it must do something"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Signature help counts the right argument
// ──────────────────────────────────────────────────────────────────────

/// The active parameter used to be "every comma after the last `(`", which is
/// wrong as soon as an argument contains a comma of its own.
#[tokio::test]
async fn signature_help_ignores_commas_inside_a_nested_argument() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    // Cursor after `[1, 2, 3], `: the second argument of math::max, not the
    // fourth. The old count said 3.
    let text = "RETURN math::max([1, 2, 3], ";
    open(&core, "nested.surql", text).await;

    let help = signature_help_at(&core, "nested.surql", 0, text.len() as u32).await;
    assert_eq!(
        help.active_parameter,
        Some(1),
        "an array argument's commas were counted as argument separators"
    );
}

/// A comma inside a string literal is not an argument separator either.
#[tokio::test]
async fn signature_help_ignores_commas_inside_a_string() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    let text = "RETURN string::concat('a, b, c', ";
    open(&core, "string.surql", text).await;

    let help = signature_help_at(&core, "string.surql", 0, text.len() as u32).await;
    assert_eq!(help.active_parameter, Some(1));
}

/// The innermost open call is the one to describe, not the outermost.
#[tokio::test]
async fn signature_help_describes_the_innermost_open_call() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    let text = "RETURN math::max(1, string::concat('a', ";
    open(&core, "inner.surql", text).await;

    let help = signature_help_at(&core, "inner.surql", 0, text.len() as u32).await;
    assert!(
        help.signatures
            .first()
            .is_some_and(|signature| signature.label.contains("string::concat")),
        "expected the inner call, got {:?}",
        help.signatures.first().map(|s| s.label.clone())
    );
    assert_eq!(help.active_parameter, Some(1));
}

// ──────────────────────────────────────────────────────────────────────
// Document highlight covers tables and fields, with real kinds
// ──────────────────────────────────────────────────────────────────────

async fn highlights_at(
    core: &common::TestCore,
    path: &str,
    line: u32,
    character: u32,
) -> Vec<(u32, tower_lsp_server::ls_types::DocumentHighlightKind)> {
    let mut found: Vec<_> = core
        .document_highlight(tower_lsp_server::ls_types::DocumentHighlightParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri(path) },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .into_iter()
        .map(|highlight| {
            (
                highlight.range.start.line,
                highlight.kind.expect("a kind is always set"),
            )
        })
        .collect();
    found.sort_by_key(|(line, _)| *line);
    found
}

/// Highlighting used to cover custom functions only, and to call every
/// occurrence a READ, so putting the cursor on a table name lit up nothing, and
/// an editor could not tell a `SELECT` from the `DELETE` below it.
#[tokio::test]
async fn highlighting_a_table_distinguishes_reads_from_writes() {
    use tower_lsp_server::ls_types::DocumentHighlightKind;

    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    open(
        &core,
        "hl.surql",
        "DEFINE TABLE person SCHEMAFULL;\n\
         SELECT * FROM person;\n\
         DELETE person;\n\
         CREATE person SET name = 'a';\n",
    )
    .await;

    // Cursor on `person` in the SELECT.
    let found = highlights_at(&core, "hl.surql", 1, 15).await;
    assert_eq!(
        found,
        vec![
            (0, DocumentHighlightKind::WRITE), // the DEFINE introduces it
            (1, DocumentHighlightKind::READ),  // SELECT
            (2, DocumentHighlightKind::WRITE), // DELETE
            (3, DocumentHighlightKind::WRITE), // CREATE
        ],
        "expected one highlight per occurrence, with reads and writes distinguished"
    );
}

/// A name nothing in the document mentions highlights nothing: the walk must
/// not match on substrings or light up unrelated tokens.
#[tokio::test]
async fn highlighting_an_unrelated_token_finds_nothing() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    open(&core, "none.surql", "SELECT * FROM person;\n").await;
    assert!(highlights_at(&core, "none.surql", 0, 0).await.is_empty());
}

// ──────────────────────────────────────────────────────────────────────
// Workspace roots, from whichever field the client filled in
// ──────────────────────────────────────────────────────────────────────

/// The folders the server decided to index, read back through the loader it
/// asked to walk them.
async fn indexed_roots(params: InitializeParams) -> Vec<std::path::PathBuf> {
    let (core, _notifier, _, loader) =
        common::core_with_loader(Default::default(), Default::default());
    core.initialize(params).await;
    core.initialized().await;
    loader.folders.lock().unwrap().clone()
}

/// `rootUri` is deprecated but still what eglot and several minimal clients
/// send, and reading only `workspaceFolders` meant such a client silently got
/// **no** workspace schema: every cross-file table came back undefined with
/// nothing to explain it.
#[tokio::test]
async fn a_client_that_sends_only_root_uri_still_gets_a_workspace() {
    #[allow(deprecated)]
    let params = InitializeParams {
        root_uri: Some(uri_for_dir("/tmp/surql-root-uri")),
        ..InitializeParams::default()
    };
    assert_eq!(
        indexed_roots(params).await,
        vec![std::path::PathBuf::from("/tmp/surql-root-uri")],
    );
}

/// `workspaceFolders` still wins when both are present.
#[tokio::test]
async fn workspace_folders_take_precedence_over_root_uri() {
    #[allow(deprecated)]
    let params = InitializeParams {
        root_uri: Some(uri_for_dir("/tmp/surql-old")),
        workspace_folders: Some(vec![tower_lsp_server::ls_types::WorkspaceFolder {
            uri: uri_for_dir("/tmp/surql-new"),
            name: "new".to_string(),
        }]),
        ..InitializeParams::default()
    };
    assert_eq!(
        indexed_roots(params).await,
        vec![std::path::PathBuf::from("/tmp/surql-new")],
    );
}

fn uri_for_dir(path: &str) -> tower_lsp_server::ls_types::Uri {
    use std::str::FromStr as _;
    tower_lsp_server::ls_types::Uri::from_str(&format!("file://{path}")).expect("valid file uri")
}

// ──────────────────────────────────────────────────────────────────────
// Folding and selection ranges
// ──────────────────────────────────────────────────────────────────────

async fn folds(core: &common::TestCore, path: &str) -> Vec<(u32, u32, Option<String>)> {
    let mut found: Vec<_> = core
        .folding_range(tower_lsp_server::ls_types::FoldingRangeParams {
            text_document: TextDocumentIdentifier { uri: uri(path) },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|range| {
            (
                range.start_line,
                range.end_line,
                range.kind.map(|kind| format!("{kind:?}")),
            )
        })
        .collect();
    found.sort();
    found
}

/// A multi-line `DEFINE FUNCTION` folds; the one-liner beside it does not.
#[tokio::test]
async fn multi_line_regions_fold_and_single_line_ones_do_not() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    open(
        &core,
        "fold.surql",
        "DEFINE TABLE person SCHEMAFULL;\n\
         DEFINE FUNCTION fn::greet($name: string) {\n\
         \x20   RETURN 'hi';\n\
         };\n",
    )
    .await;

    let found = folds(&core, "fold.surql").await;
    assert!(
        found.iter().any(|(start, end, _)| *start == 1 && *end >= 2),
        "the multi-line function must fold: {found:?}"
    );
    assert!(
        !found.iter().any(|(start, _, _)| *start == 0),
        "the single-line DEFINE TABLE must not offer a fold: {found:?}"
    );
}

/// The last line is excluded so the closing brace stays visible when collapsed,
/// which is what every editor's built-in folding does.
#[tokio::test]
async fn a_fold_stops_before_its_closing_line() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    open(
        &core,
        "brace.surql",
        "DEFINE FUNCTION fn::f() {\n    RETURN 1;\n};\n",
    )
    .await;

    let found = folds(&core, "brace.surql").await;
    assert!(
        found.iter().any(|(start, end, _)| *start == 0 && *end == 1),
        "expected a fold from line 1 to line 2, got {found:?}"
    );
}

/// Consecutive comment lines fold as one block; a blank line splits them.
#[tokio::test]
async fn consecutive_comments_fold_as_one_block() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    open(
        &core,
        "comments.surql",
        "-- one\n-- two\n-- three\n\n-- separate\nRETURN 1;\n",
    )
    .await;

    let found = folds(&core, "comments.surql").await;
    let comment_folds: Vec<_> = found
        .iter()
        .filter(|(_, _, kind)| kind.as_deref() == Some("Comment"))
        .collect();
    assert_eq!(
        comment_folds.len(),
        1,
        "the three adjacent comments are one block and the lone one is not foldable: {found:?}"
    );
    assert_eq!((comment_folds[0].0, comment_folds[0].1), (0, 2));
}

/// Expand-selection walks outward, and every step must be strictly larger or an
/// editor appears stuck.
#[tokio::test]
async fn selection_range_widens_at_every_step() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    let text = "SELECT name FROM person;\n";
    open(&core, "sel.surql", text).await;

    let ranges = core
        .selection_range(tower_lsp_server::ls_types::SelectionRangeParams {
            text_document: TextDocumentIdentifier {
                uri: uri("sel.surql"),
            },
            // On `person`.
            positions: vec![Position::new(0, 18)],
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .expect("selection ranges");
    assert_eq!(ranges.len(), 1, "one chain per requested position");

    let mut sizes = Vec::new();
    let mut current = Some(&ranges[0]);
    while let Some(selection) = current {
        let range = selection.range;
        sizes.push((
            range.start.character,
            range.end.character,
            range.end.line - range.start.line,
        ));
        current = selection.parent.as_deref();
    }

    assert!(sizes.len() >= 2, "expected a chain, got {sizes:?}");
    for pair in sizes.windows(2) {
        let (inner, outer) = (pair[0], pair[1]);
        assert!(
            outer.0 <= inner.0 && (outer.1 >= inner.1 || outer.2 > inner.2),
            "each step must widen: {inner:?} then {outer:?}"
        );
    }
}

// ──────────────────────────────────────────────────────────────────────
// Oversize documents are declined, not dropped
// ──────────────────────────────────────────────────────────────────────

/// The filesystem walk has skipped files over 2 MB since 0.3, but a buffer the
/// *editor* pushes went straight into the analyzer with no bound: the wider of
/// the two doors, and the unguarded one.
///
/// The document must still be tracked. Dropping its text would lose the
/// server's record of a buffer the client still has open, and under incremental
/// sync it would desynchronise permanently.
#[tokio::test]
async fn an_oversize_document_is_tracked_but_not_analysed() {
    let (core, notifier, _) = common::core_with(Default::default(), Default::default());
    let mut settings = ServerSettings::default();
    settings.analysis.diagnostic_debounce_ms = 0;
    settings.analysis.max_document_bytes = 1024;
    core.apply_settings(settings).await;

    // Well over the cap, and otherwise perfectly valid.
    let text = "DEFINE TABLE person SCHEMAFULL;\n".repeat(200);
    open(&core, "big.surql", &text).await;

    let published = notifier
        .published()
        .into_iter()
        .rev()
        .find(|(published_uri, _)| *published_uri == uri("big.surql"))
        .map(|(_, diagnostics)| diagnostics)
        .expect("an oversize document still publishes");
    assert_eq!(published.len(), 1, "one explanation, not a flood");
    assert!(
        published[0].message.contains("analysis limit"),
        "the user must be told why it is silent: {:?}",
        published[0].message
    );
    assert_eq!(
        published[0].severity,
        Some(tower_lsp_server::ls_types::DiagnosticSeverity::INFORMATION),
        "nothing is wrong with the file; the server declined to read it"
    );

    // Tracked: the document answers requests rather than being unknown.
    assert_eq!(
        defined_table(&core, "big.surql").await,
        None,
        "no symbols, because it was not analysed"
    );
}

/// Under the cap, nothing changes.
#[tokio::test]
async fn a_document_under_the_cap_is_analysed_normally() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    let mut settings = ServerSettings::default();
    settings.analysis.diagnostic_debounce_ms = 0;
    settings.analysis.max_document_bytes = 1024;
    core.apply_settings(settings).await;

    open(&core, "small.surql", "DEFINE TABLE person SCHEMAFULL;\n").await;
    assert_eq!(
        defined_table(&core, "small.surql").await.as_deref(),
        Some("TABLE person"),
    );
}

/// `0` means no cap, for anyone who really does open a generated dump.
#[tokio::test]
async fn a_zero_cap_removes_the_limit() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    let mut settings = ServerSettings::default();
    settings.analysis.diagnostic_debounce_ms = 0;
    settings.analysis.max_document_bytes = 0;
    core.apply_settings(settings).await;

    let text = "DEFINE TABLE person SCHEMAFULL;\n".repeat(200);
    open(&core, "uncapped.surql", &text).await;
    assert_eq!(
        defined_table(&core, "uncapped.surql").await.as_deref(),
        Some("TABLE person"),
    );
}

// ──────────────────────────────────────────────────────────────────────
// The authoritative buffer
// ──────────────────────────────────────────────────────────────────────

/// Edits arriving through the real path produce the same text as the same edits
/// applied one after another.
///
/// Trivially true under full-document sync, where every notification carries the
/// whole text. The point is that it exists *before* incremental sync, so the
/// commit that switches over has a test that was already green to break.
#[tokio::test]
async fn interleaved_edits_produce_the_same_text_as_serial_ones() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(&core, "seq.surql", "DEFINE TABLE t0 SCHEMAFULL;").await;

    for version in 1..=8 {
        core.did_change(change(
            "seq.surql",
            version,
            &format!("DEFINE TABLE t{version} SCHEMAFULL;"),
        ))
        .await;
    }

    assert_eq!(
        defined_table(&core, "seq.surql").await.as_deref(),
        Some("TABLE t8"),
        "the last edit applied must be the one that stands"
    );
}

/// Applying and analysing are separate calls now, so the ordering guarantee can
/// be tested without racing a spawned task: apply every edit first, then analyse
/// once. The newest text must win regardless of how many analyses were skipped.
#[tokio::test]
async fn applying_ahead_of_analysis_keeps_the_newest_text() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(&core, "ahead.surql", "DEFINE TABLE t0 SCHEMAFULL;").await;

    // Five edits land before a single analysis runs: what a burst looks like
    // when the debounce collapses it.
    for version in 1..=5 {
        core.did_change(change(
            "ahead.surql",
            version,
            &format!("DEFINE TABLE t{version} SCHEMAFULL;"),
        ))
        .await;
    }

    assert_eq!(
        defined_table(&core, "ahead.surql").await.as_deref(),
        Some("TABLE t5"),
    );
}

/// A change for a document the server never saw an open for is taken as the
/// whole content rather than dropped: the client believes the buffer exists, and
/// disagreeing with it silently is worse than accepting the text.
#[tokio::test]
async fn a_change_without_an_open_is_still_applied() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;

    core.did_change(change(
        "unopened.surql",
        3,
        "DEFINE TABLE ghost SCHEMAFULL;",
    ))
    .await;

    assert_eq!(
        defined_table(&core, "unopened.surql").await.as_deref(),
        Some("TABLE ghost"),
    );
}

// ──────────────────────────────────────────────────────────────────────
// Incremental sync
// ──────────────────────────────────────────────────────────────────────

/// One ranged edit: `(start line, start character)`, `(end …)`, replacement.
/// Characters are UTF-16 code units, as the protocol counts them.
type RangedEdit<'a> = ((u32, u32), (u32, u32), &'a str);

fn ranged(path: &str, version: i32, edits: &[RangedEdit<'_>]) -> DidChangeTextDocumentParams {
    DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri(path),
            version,
        },
        content_changes: edits
            .iter()
            .map(|(start, end, text)| TextDocumentContentChangeEvent {
                range: Some(tower_lsp_server::ls_types::Range {
                    start: Position::new(start.0, start.1),
                    end: Position::new(end.0, end.1),
                }),
                range_length: None,
                text: (*text).to_string(),
            })
            .collect(),
    }
}

/// Read back exactly what the server believes the buffer contains.
async fn buffer_text(core: &common::TestCore, path: &str) -> Option<String> {
    core.buffer_snapshot(&uri(path))
}

/// Three partial changes must produce the same text as the one full replacement
/// they add up to. This is the property incremental sync lives or dies on.
#[tokio::test]
async fn partial_changes_equal_the_full_replacement() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(&core, "inc.surql", "SELECT a FROM t;\n").await;

    // "SELECT a FROM t;" -> "SELECT name FROM person;"
    core.did_change(ranged(
        "inc.surql",
        2,
        &[
            ((0, 7), (0, 8), "name"),     // a -> name
            ((0, 17), (0, 18), "person"), // t -> person
        ],
    ))
    .await;

    assert_eq!(
        buffer_text(&core, "inc.surql").await.as_deref(),
        Some("SELECT name FROM person;\n"),
        "the second edit must be applied against the text the first produced"
    );
}

/// The second change in a batch is expressed against the text the first
/// produced, so the line index has to be rebuilt per change rather than per
/// notification. An insertion that adds a line proves it.
#[tokio::test]
async fn a_later_change_sees_the_earlier_one() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(&core, "lines.surql", "one\nthree\n").await;

    core.did_change(ranged(
        "lines.surql",
        2,
        &[
            ((0, 3), (0, 3), "\ntwo"), // insert a line
            ((2, 0), (2, 5), "THREE"), // line 2 only exists after the first edit
        ],
    ))
    .await;

    assert_eq!(
        buffer_text(&core, "lines.surql").await.as_deref(),
        Some("one\ntwo\nTHREE\n"),
    );
}

/// A range crossing a character outside the BMP. `LineIndex` counts UTF-16 code
/// units, so an emoji is two of them and one `char`: the arithmetic that a full
/// sync never exercised, and the one where an off-by-one compounds forever.
#[tokio::test]
async fn a_range_across_a_surrogate_pair_converts_correctly() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    // 🦀 is one char, two UTF-16 units, four bytes.
    open(&core, "utf.surql", "RETURN '🦀 crab';\n").await;

    // Replace `crab`, which starts at UTF-16 offset 8+2+1 = 11.
    core.did_change(ranged("utf.surql", 2, &[((0, 11), (0, 15), "lobster")]))
        .await;

    assert_eq!(
        buffer_text(&core, "utf.surql").await.as_deref(),
        Some("RETURN '🦀 lobster';\n"),
        "a surrogate pair must count as two UTF-16 units, not one"
    );
}

/// A multi-byte character inside the BMP: ₹ is three bytes and one UTF-16 unit.
#[tokio::test]
async fn a_range_across_a_multi_byte_character_converts_correctly() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(&core, "rupee.surql", "RETURN '₹ 5';\n").await;

    // R E T U R N ␣ ' ₹ ␣ 5 ' ;  `₹` is three bytes but one UTF-16 unit, so
    // `5` sits at unit 10, which is the whole point of the case.
    core.did_change(ranged("rupee.surql", 2, &[((0, 10), (0, 11), "10")]))
        .await;

    assert_eq!(
        buffer_text(&core, "rupee.surql").await.as_deref(),
        Some("RETURN '₹ 10';\n"),
    );
}

/// Insertion at the very end, and deletion to the end.
#[tokio::test]
async fn edits_at_the_document_boundary_apply() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(&core, "edge.surql", "RETURN 1;\n").await;

    core.did_change(ranged("edge.surql", 2, &[((1, 0), (1, 0), "RETURN 2;\n")]))
        .await;
    assert_eq!(
        buffer_text(&core, "edge.surql").await.as_deref(),
        Some("RETURN 1;\nRETURN 2;\n"),
    );

    core.did_change(ranged("edge.surql", 3, &[((0, 9), (2, 0), "")]))
        .await;
    assert_eq!(
        buffer_text(&core, "edge.surql").await.as_deref(),
        Some("RETURN 1;"),
    );
}

/// `\r\n` line endings: the terminator is two bytes but the position of the
/// next line is unaffected.
#[tokio::test]
async fn crlf_line_endings_are_handled() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(&core, "crlf.surql", "RETURN 1;\r\nRETURN 2;\r\n").await;

    core.did_change(ranged("crlf.surql", 2, &[((1, 7), (1, 8), "9")]))
        .await;
    assert_eq!(
        buffer_text(&core, "crlf.surql").await.as_deref(),
        Some("RETURN 1;\r\nRETURN 9;\r\n"),
    );
}

/// An out-of-bounds range desynchronises the buffer rather than splicing
/// somewhere plausible: `LineIndex::offset` clamps, so the wrong answer would
/// otherwise look like a right one and compound with every later edit. A whole
/// document clears it.
#[tokio::test]
async fn an_out_of_bounds_range_desyncs_until_a_full_document_arrives() {
    let (core, notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(&core, "desync.surql", "RETURN 1;\n").await;

    // Line 99 does not exist.
    core.did_change(ranged("desync.surql", 2, &[((99, 0), (99, 4), "nope")]))
        .await;
    assert_eq!(
        buffer_text(&core, "desync.surql").await.as_deref(),
        Some("RETURN 1;\n"),
        "an impossible range must change nothing"
    );

    // Further ranged edits are refused while desynced.
    core.did_change(ranged("desync.surql", 3, &[((0, 7), (0, 8), "2")]))
        .await;
    assert_eq!(
        buffer_text(&core, "desync.surql").await.as_deref(),
        Some("RETURN 1;\n"),
        "ranged edits stay refused until the client resends the document"
    );

    // A full replacement re-establishes the baseline.
    core.did_change(change("desync.surql", 4, "RETURN 7;\n"))
        .await;
    assert_eq!(
        buffer_text(&core, "desync.surql").await.as_deref(),
        Some("RETURN 7;\n"),
    );

    // And ranged edits work again.
    core.did_change(ranged("desync.surql", 5, &[((0, 7), (0, 8), "8")]))
        .await;
    assert_eq!(
        buffer_text(&core, "desync.surql").await.as_deref(),
        Some("RETURN 8;\n"),
        "the desync must clear, not persist for the life of the document"
    );

    assert!(
        notifier
            .logs()
            .iter()
            .any(|(_, message)| message.contains("Ranged edits are ignored")),
        "a desync has to be visible, not silent"
    );
}

/// A client that ignores the advertised kind and keeps sending whole documents
/// is still handled: that is the `range: None` branch.
#[tokio::test]
async fn a_client_sending_full_documents_still_works() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(&core, "full.surql", "DEFINE TABLE a SCHEMAFULL;").await;

    core.did_change(change("full.surql", 2, "DEFINE TABLE b SCHEMAFULL;"))
        .await;
    assert_eq!(
        defined_table(&core, "full.surql").await.as_deref(),
        Some("TABLE b"),
    );
}

// ──────────────────────────────────────────────────────────────────────
// Incremental parsing
// ──────────────────────────────────────────────────────────────────────

/// The pending tree must exist after an analysis and vanish when it can no
/// longer describe the buffer.
///
/// Not an implementation detail: a *stale* pending tree is worse than none,
/// because the next parse would build on a description of text that no longer
/// exists. Every path that cannot maintain it has to clear it, and this is what
/// says so.
#[tokio::test]
async fn the_pending_tree_is_kept_and_dropped_at_the_right_moments() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;

    open(&core, "tree.surql", "SELECT a FROM t;\n").await;
    assert!(
        core.has_pending_tree(&uri("tree.surql")),
        "an analysis must leave a tree for the next parse to build on"
    );

    // A ranged edit keeps it: that is the whole point.
    core.did_change(ranged("tree.surql", 2, &[((0, 7), (0, 8), "b")]))
        .await;
    assert!(
        core.has_pending_tree(&uri("tree.surql")),
        "a ranged edit must leave a reusable tree"
    );

    // A whole-document replacement drops the old tree (there is no edit that
    // describes a wholesale replacement), and the analysis that follows leaves a
    // tree of the *new* text, which is what the next parse should build on.
    core.did_change(change("tree.surql", 3, "SELECT c FROM u;\n"))
        .await;
    assert!(core.has_pending_tree(&uri("tree.surql")));
    assert_eq!(
        core.buffer_snapshot(&uri("tree.surql")).as_deref(),
        Some("SELECT c FROM u;\n"),
    );
}

/// A document the analyzer *declined* must leave no tree behind.
///
/// The refusal paths carry an empty tree, because parsing is what they declined.
/// Keeping that as the base for the next parse would apply the intervening
/// edits (byte offsets into a large document) to a tree describing nothing.
#[tokio::test]
async fn a_refused_document_leaves_no_tree_to_build_on() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    let mut settings = ServerSettings::default();
    settings.analysis.diagnostic_debounce_ms = 0;
    settings.analysis.max_document_bytes = 512;
    core.apply_settings(settings).await;

    open(&core, "refused.surql", &"SELECT * FROM t;\n".repeat(100)).await;
    assert!(
        !core.has_pending_tree(&uri("refused.surql")),
        "an empty tree must not become the base for the next incremental parse"
    );
}

/// A document reached through ranged edits must analyse to exactly what the same
/// final text analyses to when opened directly.
///
/// The corpus-wide version of this lives in `tests/conformance.rs`
/// (`incremental_reparse_matches_a_fresh_parse`); this one drives the real
/// server path, so it also covers the bookkeeping around the parse rather than
/// the parse alone.
#[tokio::test]
async fn edits_reach_the_same_analysis_as_opening_the_final_text() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;

    // Type it the way a person would: broken in the middle, then fixed.
    open(&core, "typed.surql", "DEFINE TABLE p SCHEMAFULL;\n").await;
    core.did_change(ranged("typed.surql", 2, &[((0, 13), (0, 14), "person")]))
        .await;
    core.did_change(ranged(
        "typed.surql",
        3,
        &[((1, 0), (1, 0), "SELECT * FROM ")],
    ))
    .await;
    core.did_change(ranged("typed.surql", 4, &[((1, 14), (1, 14), "person;")]))
        .await;

    let typed_tree = core.tree_sexp(&uri("typed.surql")).await.expect("analysed");
    let typed_symbols = document_symbol_names(&core, "typed.surql").await;

    // The same text, opened in one go.
    open(
        &core,
        "opened.surql",
        "DEFINE TABLE person SCHEMAFULL;\nSELECT * FROM person;",
    )
    .await;
    let opened_tree = core
        .tree_sexp(&uri("opened.surql"))
        .await
        .expect("analysed");

    assert_eq!(
        typed_tree, opened_tree,
        "a document reached by editing must parse to the same tree as one opened whole"
    );
    assert_eq!(
        typed_symbols,
        document_symbol_names(&core, "opened.surql").await,
        "and to the same extracted symbols"
    );
}

async fn document_symbol_names(core: &common::TestCore, path: &str) -> Vec<String> {
    let Some(DocumentSymbolResponse::Nested(symbols)) = core
        .document_symbol(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri(path) },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
    else {
        return Vec::new();
    };
    symbols.into_iter().map(|symbol| symbol.name).collect()
}

// ──────────────────────────────────────────────────────────────────────
// Pull diagnostics, and the switch that stops the pushing
// ──────────────────────────────────────────────────────────────────────

/// An `initialize` payload from a client that pulls diagnostics.
fn pulling_client() -> InitializeParams {
    InitializeParams {
        capabilities: tower_lsp_server::ls_types::ClientCapabilities {
            text_document: Some(tower_lsp_server::ls_types::TextDocumentClientCapabilities {
                diagnostic: Some(Default::default()),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..InitializeParams::default()
    }
}

async fn pulled_diagnostics(core: &common::TestCore, path: &str) -> Vec<Diagnostic> {
    let result = core
        .document_diagnostic(tower_lsp_server::ls_types::DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri: uri(path) },
            identifier: None,
            previous_result_id: None,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await;
    match result {
        tower_lsp_server::ls_types::DocumentDiagnosticReportResult::Report(
            tower_lsp_server::ls_types::DocumentDiagnosticReport::Full(report),
        ) => report.full_document_diagnostic_report.items,
        _ => Vec::new(),
    }
}

/// A client that pulls must not also be pushed to: doing both is how every
/// diagnostic ends up rendered twice.
#[tokio::test]
async fn a_pulling_client_is_not_pushed_to() {
    let (core, notifier, _) = common::core_with(Default::default(), Default::default());
    core.initialize(pulling_client()).await;
    core.apply_settings(settings_with_debounce(0)).await;

    let before = notifier.published().len();
    open(&core, "pull.surql", "SELECT * FROM;").await;
    core.did_change(change("pull.surql", 2, "SELECT * FROM ;;"))
        .await;

    assert_eq!(
        notifier.published().len(),
        before,
        "a pulling client was pushed to anyway"
    );

    // But the diagnostics are there when asked for.
    let pulled = pulled_diagnostics(&core, "pull.surql").await;
    assert!(
        !pulled.is_empty(),
        "a pull must return what the push would have carried"
    );
}

/// The regression guard that matters: a client which declares nothing (the
/// browser host sends exactly that today) must keep receiving pushes.
#[tokio::test]
async fn a_client_that_declares_nothing_still_receives_pushes() {
    let (core, notifier, _) = common::core_with(Default::default(), Default::default());
    core.initialize(InitializeParams::default()).await;
    core.apply_settings(settings_with_debounce(0)).await;

    let before = notifier.published().len();
    open(&core, "push.surql", "SELECT * FROM;").await;

    assert!(
        notifier.published().len() > before,
        "an absent capability must never turn a working behaviour off"
    );
}

/// The advertisement follows the same answer as the suppression, so the two
/// cannot drift apart.
#[tokio::test]
async fn the_diagnostic_provider_is_only_advertised_to_a_pulling_client() {
    let quiet = common::TestCore::server_capabilities(Default::default());
    assert!(
        quiet.diagnostic_provider.is_none(),
        "a client that did not ask must not be offered pulls"
    );

    let profile = surrealql_language_server::core::state::ClientProfile::from_capabilities(
        &pulling_client().capabilities,
    );
    assert!(profile.pull_diagnostics);
    assert!(
        common::TestCore::server_capabilities(profile)
            .diagnostic_provider
            .is_some(),
        "a client that asked must be offered pulls"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Files changed outside the editor
// ──────────────────────────────────────────────────────────────────────

/// A client that supports dynamic registration.
fn watching_client() -> InitializeParams {
    InitializeParams {
        capabilities: tower_lsp_server::ls_types::ClientCapabilities {
            workspace: Some(tower_lsp_server::ls_types::WorkspaceClientCapabilities {
                did_change_watched_files: Some(
                    tower_lsp_server::ls_types::DidChangeWatchedFilesClientCapabilities {
                        dynamic_registration: Some(true),
                        relative_pattern_support: None,
                    },
                ),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..InitializeParams::default()
    }
}

/// The watcher is registered only for a client that can take it.
#[tokio::test]
async fn a_file_watcher_is_registered_when_the_client_supports_it() {
    let (core, notifier, _) = common::core_with(Default::default(), Default::default());
    core.initialize(watching_client()).await;
    core.initialized().await;
    assert!(
        notifier
            .registrations()
            .contains(&"workspace/didChangeWatchedFiles".to_string()),
        "a capable client must be asked to watch .surql files"
    );

    let (quiet, quiet_notifier, _) = common::core_with(Default::default(), Default::default());
    quiet.initialize(InitializeParams::default()).await;
    quiet.initialized().await;
    assert!(
        quiet_notifier.registrations().is_empty(),
        "a client that cannot register must not be asked"
    );
}

/// A schema file deleted outside the editor must stop contributing.
///
/// This is the `git checkout` case. Nothing picked such a change up short of a
/// restart, and the symptom was a diagnostic that disagreed with the files on
/// disk, which reads as a language-server bug rather than a missed
/// notification.
///
/// The probe is a typo: `persn` is only reportable *while* `person` is defined
/// somewhere to be a typo of. Delete the definition and the report must go.
#[tokio::test]
async fn deleting_a_schema_file_updates_the_open_buffer() {
    let mut workspace = surrealql_language_server::semantic::types::WorkspaceIndex::default();
    let schema_uri = uri("schema.surql");
    let analysis = surrealql_language_server::semantic::analyzer::analyze_document(
        schema_uri.clone(),
        "DEFINE TABLE person SCHEMAFULL;",
        surrealql_language_server::semantic::types::SymbolOrigin::Local,
    )
    .expect("analysed");
    workspace
        .documents
        .insert(schema_uri.clone(), std::sync::Arc::new(analysis));

    let (core, notifier, _) = common::core_with(workspace, Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(&core, "query.surql", "SELECT * FROM persn;").await;

    let latest = |notifier: &common::RecordingNotifier| {
        notifier
            .published()
            .into_iter()
            .rev()
            .find(|(published, _)| *published == uri("query.surql"))
            .map(|(_, diagnostics)| diagnostics)
            .unwrap_or_default()
    };

    assert!(
        latest(&notifier)
            .iter()
            .any(|d| has_code(d, "unknown-table")),
        "`persn` is a typo of a defined table, so it must be reported"
    );

    core.did_change_watched_files(tower_lsp_server::ls_types::DidChangeWatchedFilesParams {
        changes: vec![tower_lsp_server::ls_types::FileEvent {
            uri: schema_uri,
            typ: tower_lsp_server::ls_types::FileChangeType::DELETED,
        }],
    })
    .await;

    assert!(
        !latest(&notifier)
            .iter()
            .any(|d| has_code(d, "unknown-table")),
        "with the definition gone there is nothing for `persn` to be a typo of, \
         and the open buffer must be told"
    );
}

/// A file changed on disk *under an open buffer* must not overwrite what the
/// user is editing: the buffer is the authority for its own text.
#[tokio::test]
async fn a_disk_change_does_not_clobber_an_open_buffer() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(&core, "live.surql", "DEFINE TABLE edited SCHEMAFULL;").await;

    core.did_change_watched_files(tower_lsp_server::ls_types::DidChangeWatchedFilesParams {
        changes: vec![tower_lsp_server::ls_types::FileEvent {
            uri: uri("live.surql"),
            typ: tower_lsp_server::ls_types::FileChangeType::CHANGED,
        }],
    })
    .await;

    assert_eq!(
        core.buffer_snapshot(&uri("live.surql")).as_deref(),
        Some("DEFINE TABLE edited SCHEMAFULL;"),
        "the editor's unsaved text must survive a change notification"
    );
}

fn has_code(diagnostic: &Diagnostic, code: &str) -> bool {
    matches!(
        &diagnostic.code,
        Some(tower_lsp_server::ls_types::NumberOrString::String(value)) if value == code
    )
}

// ──────────────────────────────────────────────────────────────────────
// Navigation beyond functions
// ──────────────────────────────────────────────────────────────────────

async fn references_at(
    core: &common::TestCore,
    path: &str,
    line: u32,
    character: u32,
    include_declaration: bool,
) -> Vec<(u32, u32)> {
    let mut found: Vec<_> = core
        .references(tower_lsp_server::ls_types::ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri(path) },
                position: Position::new(line, character),
            },
            context: tower_lsp_server::ls_types::ReferenceContext {
                include_declaration,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .into_iter()
        .map(|location| (location.range.start.line, location.range.start.character))
        .collect();
    found.sort();
    found
}

/// "Where else is this table used?" is the most-asked navigation question in a
/// `.surql` workspace, and the answer used to be an empty list.
#[tokio::test]
async fn references_finds_every_use_of_a_table() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(
        &core,
        "refs.surql",
        "DEFINE TABLE person SCHEMAFULL;\n\
         SELECT * FROM person;\n\
         DELETE person;\n",
    )
    .await;

    let without = references_at(&core, "refs.surql", 1, 15, false).await;
    assert_eq!(without.len(), 2, "both queries name the table: {without:?}");

    let with = references_at(&core, "refs.surql", 1, 15, true).await;
    assert_eq!(
        with.len(),
        3,
        "include_declaration adds the DEFINE: {with:?}"
    );
    assert_eq!(with[0].0, 0, "the declaration sorts first");
}

/// A field *written* by a query is reachable the same way.
///
/// Writes only, and that is a limitation of the extractor rather than of
/// references: `QueryFact.field_refs` records assignment targets, so
/// `UPDATE … SET email` is indexed and `SELECT email` is not. Finding the writes
/// is still worth having (it is the "what touches this column?" question), and
/// an empty list, which is what this returned before, helps nobody. The README
/// says so rather than implying more.
#[tokio::test]
async fn references_finds_a_written_field() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(
        &core,
        "field.surql",
        "DEFINE TABLE person SCHEMAFULL;\n\
         DEFINE FIELD email ON person TYPE string;\n\
         UPDATE person SET email = 'a';\n\
         UPDATE person SET email = 'b';\n",
    )
    .await;

    let found = references_at(&core, "field.surql", 2, 18, false).await;
    assert_eq!(found.len(), 2, "both assignments name the field: {found:?}");

    let with_declaration = references_at(&core, "field.surql", 2, 18, true).await;
    assert_eq!(
        with_declaration.len(),
        3,
        "include_declaration adds the DEFINE FIELD: {with_declaration:?}"
    );
}

/// `TYPE record<person>` on a field takes you to `DEFINE TABLE person`: the one
/// place type-definition means something different from definition here.
#[tokio::test]
async fn type_definition_follows_a_record_type_to_its_table() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(
        &core,
        "typed.surql",
        "DEFINE TABLE person SCHEMAFULL;\n\
         DEFINE TABLE post SCHEMAFULL;\n\
         DEFINE FIELD author ON post TYPE record<person>;\n",
    )
    .await;

    let response = core
        .goto_type_definition(tower_lsp_server::ls_types::GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri("typed.surql"),
                },
                // On `author`.
                position: Position::new(2, 13),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .expect("a record-typed field has a type definition");

    let tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(location) = response else {
        panic!("expected one location");
    };
    assert_eq!(
        location.range.start.line, 0,
        "must land on DEFINE TABLE person, not on the field"
    );
}

/// A client that understands `LocationLink` gets the origin range, so the editor
/// underlines the token rather than guessing at its extent.
#[tokio::test]
async fn definition_answers_with_a_link_when_the_client_supports_it() {
    let linking = InitializeParams {
        capabilities: tower_lsp_server::ls_types::ClientCapabilities {
            text_document: Some(tower_lsp_server::ls_types::TextDocumentClientCapabilities {
                definition: Some(tower_lsp_server::ls_types::GotoCapability {
                    link_support: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..InitializeParams::default()
    };

    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.initialize(linking).await;
    core.apply_settings(settings_with_debounce(0)).await;
    open(
        &core,
        "link.surql",
        "DEFINE TABLE person SCHEMAFULL;\nSELECT * FROM person;\n",
    )
    .await;

    let response = core
        .goto_definition(tower_lsp_server::ls_types::GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: uri("link.surql"),
                },
                position: Position::new(1, 15),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .expect("definition");

    let tower_lsp_server::ls_types::GotoDefinitionResponse::Link(links) = response else {
        panic!("a link-capable client must get links");
    };
    assert_eq!(links.len(), 1);
    assert!(
        links[0].origin_selection_range.is_some(),
        "the origin range is the reason to use a link at all"
    );
}

/// Rename stays functions-only, **deliberately**.
///
/// A table name appears in record-id literals, `RELATE` arrows, permission
/// clauses and strings that the reference index does not cover, so a rename
/// would miss occurrences and leave a workspace that parses and is wrong. The
/// decline is pinned here so it cannot quietly become partial coverage.
#[tokio::test]
async fn rename_declines_on_a_table() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;
    open(
        &core,
        "rename.surql",
        "DEFINE TABLE person SCHEMAFULL;\nSELECT * FROM person;\n",
    )
    .await;

    let response = core
        .prepare_rename(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: uri("rename.surql"),
            },
            position: Position::new(1, 15),
        })
        .await;

    assert!(
        response.is_none(),
        "renaming a table is not supported, and offering it would be worse than not"
    );
}

// ──────────────────────────────────────────────────────────────────────
// One-shot validation
// ──────────────────────────────────────────────────────────────────────

/// The logic behind the browser's `validateQuery`, tested here because CI does
/// not build wasm on the pull-request path: a wasm-only implementation would
/// have no coverage at all.
#[tokio::test]
async fn validate_text_checks_a_snippet_against_the_workspace() {
    let mut workspace = surrealql_language_server::semantic::types::WorkspaceIndex::default();
    let schema_uri = uri("schema.surql");
    let analysis = surrealql_language_server::semantic::analyzer::analyze_document(
        schema_uri.clone(),
        "DEFINE TABLE person SCHEMAFULL;",
        surrealql_language_server::semantic::types::SymbolOrigin::Local,
    )
    .expect("analysed");
    workspace
        .documents
        .insert(schema_uri, std::sync::Arc::new(analysis));

    let (core, notifier, _) = common::core_with(workspace, Default::default());
    core.apply_settings(ServerSettings::default()).await;
    let published_before = notifier.published().len();

    // A typo of a table the workspace defines. Against an *empty* model this
    // would say nothing useful, which is why the model matters.
    let problems = core.validate_text("SELECT * FROM persn;", Vec::new()).await;
    assert!(
        problems.iter().any(|d| has_code(d, "unknown-table")),
        "a snippet must be checked against the pushed workspace: {problems:?}"
    );

    // Nothing is opened, published or remembered.
    assert_eq!(
        notifier.published().len(),
        published_before,
        "validation must not publish"
    );
    assert!(
        core.buffer_snapshot(&uri("validate")).is_none(),
        "validation must not open a document"
    );
}

/// A caller-bound variable is declared, not reported: the same contract
/// `check --param` and `analysis.externalParams` have.
#[tokio::test]
async fn validate_text_accepts_caller_bound_variables() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(ServerSettings::default()).await;

    let without = core
        .validate_text("SELECT * FROM person WHERE id = $id;", Vec::new())
        .await;
    assert!(
        without.iter().any(|d| has_code(d, "undefined-variable")),
        "an unbound variable is a real problem: {without:?}"
    );

    let with = core
        .validate_text(
            "SELECT * FROM person WHERE id = $id;",
            vec!["id".to_string()],
        )
        .await;
    assert!(
        !with.iter().any(|d| has_code(d, "undefined-variable")),
        "a declared variable must not be reported: {with:?}"
    );
}

/// The diagnostics are the same objects the LSP publishes, links included.
#[tokio::test]
async fn validate_text_returns_the_same_diagnostics_the_server_publishes() {
    let (core, _notifier, _) = common::core_with(Default::default(), Default::default());
    core.apply_settings(settings_with_debounce(0)).await;

    let snippet = "RETURN type::thing('person', '1');";
    let validated = core.validate_text(snippet, Vec::new()).await;
    assert_eq!(validated.len(), 1);
    assert!(has_code(&validated[0], "renamed-function"));
    assert!(
        validated[0].code_description.is_some(),
        "a one-shot check must carry the documentation link too"
    );
}
