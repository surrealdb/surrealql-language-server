use std::sync::Arc;

use tower_lsp_server::ls_types::{Location, Position, Range, Uri};

use surrealql_language_server::config::{AuthContext, ServerSettings};
use surrealql_language_server::semantic::analyzer::analyze_document;
use surrealql_language_server::semantic::model::{
    function_signature, is_record_type_context, param_label,
};
use surrealql_language_server::semantic::type_expr::TypeExpr;
use surrealql_language_server::semantic::types::{
    DocumentAnalysis, FieldDef, FunctionDef, FunctionLanguage, MergedSemanticModel, PermissionMode,
    PermissionRule, QueryAction, QueryFact, SymbolOrigin, TableDef, TargetResolution,
    WorkspaceIndex,
};

fn uri(path: &str) -> Uri {
    format!("file:///workspace/{path}")
        .parse()
        .expect("valid uri")
}

fn empty_range() -> Range {
    Range::default()
}

fn empty_location(path: &str) -> Location {
    Location::new(uri(path), empty_range())
}

#[allow(dead_code)]
fn workspace_from(analyses: Vec<DocumentAnalysis>) -> WorkspaceIndex {
    let mut ws = WorkspaceIndex::default();
    for a in analyses {
        ws.documents.insert(a.uri.clone(), Arc::new(a));
    }
    ws
}

#[test]
fn extracts_js_function_via_scripting_function() {
    let u = uri("functions.surql");
    let text = r#"
        DEFINE FUNCTION fn::slugify($text: string) -> string {
            RETURN function(text) {
                return text.toLowerCase().replace(/\s+/g, '-');
            };
        };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert_eq!(analysis.functions.len(), 1);
    let func = &analysis.functions[0];
    assert_eq!(func.name, "fn::slugify");
    assert_eq!(func.language, FunctionLanguage::JavaScript);
    assert_eq!(func.params.len(), 1);
    assert_eq!(func.params[0].name, "$text");
    assert!(
        func.called_functions.is_empty(),
        "JS inside scripting_function body is opaque — no SurrealQL callees"
    );
}

#[test]
fn js_function_no_false_positive_surql_calls() {
    let u = uri("functions.surql");
    let text = r#"
        DEFINE FUNCTION fn::util($x: string) {
            RETURN function(x) {
                const result = fn_helper(x);
                return result;
            };
        };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    let func = &analysis.functions[0];
    assert_eq!(func.language, FunctionLanguage::JavaScript);
    assert!(func.called_functions.is_empty());
}

#[test]
fn surql_function_without_language_clause_defaults_to_surrealql() {
    let u = uri("functions.surql");
    let text = r#"
        DEFINE FUNCTION fn::greet($name: string) { RETURN "Hello, " + $name; };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert_eq!(analysis.functions.len(), 1);
    assert_eq!(analysis.functions[0].language, FunctionLanguage::SurrealQL);
}

#[test]
fn extracts_function_return_type_annotation() {
    let u = uri("functions.surql");
    let text = r#"
        DEFINE FUNCTION fn::double($n: number) -> number { RETURN $n * 2; };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    let func = &analysis.functions[0];
    assert_eq!(func.name, "fn::double");
    assert_eq!(func.params[0].name, "$n");
    assert_eq!(
        func.params[0].type_expr,
        Some(TypeExpr::Scalar("number".to_string()))
    );
    assert_eq!(
        func.return_type,
        Some(TypeExpr::Scalar("number".to_string())),
        "`-> number` is `LookupRight` + a type node on the DefineStatement"
    );
}

#[test]
fn function_without_return_type_has_none() {
    let u = uri("functions.surql");
    let text = r#"
        DEFINE FUNCTION fn::noop() { RETURN NONE; };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert_eq!(analysis.functions.len(), 1);
    assert_eq!(
        analysis.functions[0].return_type, None,
        "no `->` annotation means no declared return type"
    );
}

#[test]
fn extracts_surrealql_function_with_params_and_permissions() {
    let u = uri("schema.surql");
    let text = r#"
        DEFINE FUNCTION fn::check_role($role: string) -> bool {
            RETURN $auth.roles CONTAINS $role;
        } PERMISSIONS FULL;
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert_eq!(analysis.functions.len(), 1);
    let func = &analysis.functions[0];
    assert_eq!(func.name, "fn::check_role");
    assert_eq!(func.language, FunctionLanguage::SurrealQL);
    assert_eq!(func.params.len(), 1);
    assert!(!func.permissions.is_empty());
}

#[test]
fn extracts_function_call_references_from_body() {
    let u = uri("schema.surql");
    let text = r#"
        DEFINE FUNCTION fn::outer($x: number) { RETURN fn::inner($x); };
        DEFINE FUNCTION fn::inner($x: number) { RETURN $x * 2; };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    let outer = analysis
        .functions
        .iter()
        .find(|f| f.name == "fn::outer")
        .expect("outer");
    assert!(
        outer.called_functions.contains(&"fn::inner".to_string()),
        "outer should call inner"
    );
}

#[test]
fn multiple_js_and_surql_functions_in_one_file() {
    let u = uri("mixed.surql");
    let text = r#"
        DEFINE FUNCTION fn::js_util($s: string) {
            RETURN function(s) { return s.trim(); };
        };
        DEFINE FUNCTION fn::surql_util($s: string) { RETURN string::trim($s); };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert_eq!(analysis.functions.len(), 2);
    let js = analysis
        .functions
        .iter()
        .find(|f| f.name == "fn::js_util")
        .expect("js");
    let sq = analysis
        .functions
        .iter()
        .find(|f| f.name == "fn::surql_util")
        .expect("surql");
    assert_eq!(js.language, FunctionLanguage::JavaScript);
    assert_eq!(sq.language, FunctionLanguage::SurrealQL);
}

#[test]
fn extracts_table_fields_events_indexes() {
    let u = uri("schema.surql");
    let text = r#"
        DEFINE TABLE order SCHEMAFULL;
        DEFINE FIELD amount ON TABLE order TYPE number;
        DEFINE FIELD status ON TABLE order TYPE string;
        DEFINE EVENT order_created ON TABLE order WHEN $event = 'CREATE' THEN (RETURN NONE);
        DEFINE INDEX order_status ON TABLE order FIELDS status;
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert_eq!(analysis.tables.len(), 1);
    assert_eq!(analysis.fields.iter().filter(|f| f.explicit).count(), 2);
    assert_eq!(analysis.events.len(), 1);
    assert_eq!(analysis.indexes.len(), 1);
}

#[test]
fn parse_define_param() {
    let u = uri("params.surql");
    let text = r#"
        DEFINE PARAM $page_size VALUE 20;
        DEFINE PARAM $default_lang VALUE "en";
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert_eq!(analysis.params.len(), 2);
    let names: Vec<_> = analysis.params.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"$page_size"));
    assert!(names.contains(&"$default_lang"));
}

#[test]
fn syntax_errors_produce_diagnostics() {
    let u = uri("bad.surql");
    let text = "DEFINE TABLE @@@invalid@@@;";
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        !analysis.syntax_diagnostics.is_empty(),
        "broken surql should produce diagnostics"
    );
    // Wire-compat tripwires: these identity fields are observable by
    // every LSP client and must never drift.
    for diagnostic in &analysis.syntax_diagnostics {
        assert_eq!(
            diagnostic.source.as_deref(),
            Some("surreal-language-server")
        );
        assert_eq!(
            diagnostic.code,
            Some(tower_lsp_server::ls_types::NumberOrString::String(
                "parse".to_string()
            ))
        );
        assert_eq!(
            diagnostic.severity,
            Some(tower_lsp_server::ls_types::DiagnosticSeverity::ERROR)
        );
    }
}

#[test]
fn syntax_error_suggests_keyword_for_typo() {
    let u = uri("typo.surql");
    let analysis =
        analyze_document(u, "SELECT * FRO person;", SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis
            .syntax_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("Did you mean `FROM`?")),
        "expected a FROM suggestion: {:?}",
        analysis.syntax_diagnostics
    );

    let u = uri("typo2.surql");
    let analysis = analyze_document(
        u,
        "DEFINE TABLE person SCHEMAFULL PERMISSION FOR select FULL;",
        SymbolOrigin::Local,
    )
    .expect("analysis");
    assert!(
        analysis
            .syntax_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("Did you mean `PERMISSIONS`?")),
        "expected a PERMISSIONS suggestion: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn keyword_typo_hint_skips_identifiers_defined_in_the_document() {
    let u = uri("orders.surql");
    // `orders` is a defined table, so the broken statement must
    // suggest WHERE (the actual typo), never `ORDER` for `orders`.
    let text = "DEFINE TABLE orders SCHEMALESS;\nSELECT * FROM orders WHRE id = 1;";
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis
            .syntax_diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.message.contains("`ORDER`")),
        "defined identifier must not be treated as a keyword typo: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn missing_token_diagnostic_names_the_expected_token() {
    let u = uri("missing.surql");
    let analysis = analyze_document(
        u,
        "SELECT * FROM (SELECT * FROM person;",
        SymbolOrigin::Local,
    )
    .expect("analysis");
    let missing = analysis
        .syntax_diagnostics
        .iter()
        .find(|diagnostic| diagnostic.message.starts_with("Expected"))
        .expect("missing-token diagnostic");
    assert_eq!(missing.message, "Expected `)`.");
    assert!(
        missing.range.end.character > missing.range.start.character,
        "missing-token squiggles must be visible (non-zero width): {:?}",
        missing.range
    );
}

#[test]
fn multi_line_error_is_clamped_to_first_line_with_related_info() {
    let u = uri("multiline.surql");
    let text = "SELECT * FROM person\nWHERE broken (\nmore garbage here\nDEFINE TABLE other;";
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    let clamped = analysis
        .syntax_diagnostics
        .iter()
        .find(|diagnostic| diagnostic.related_information.is_some())
        .expect("a clamped multi-line error with related info");
    assert_eq!(
        clamped.range.start.line, clamped.range.end.line,
        "clamped error range must stay on one line: {:?}",
        clamped.range
    );
    let related = clamped.related_information.as_ref().unwrap();
    assert!(related[0].message.contains("continues to line"));
    assert!(
        related[0].location.range.end.line > clamped.range.end.line,
        "related info must carry the full span"
    );
}

#[test]
fn pathological_input_caps_syntax_diagnostics() {
    let u = uri("pathological.surql");
    // Hundreds of broken statements — the cap keeps the problems
    // panel usable instead of publishing thousands of entries.
    let text = "@@@ ;\n".repeat(500);
    let analysis = analyze_document(u, &text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.len() <= 100,
        "syntax diagnostics must be capped at 100, got {}",
        analysis.syntax_diagnostics.len()
    );
}

#[test]
fn clean_surql_produces_no_syntax_diagnostics() {
    let u = uri("clean.surql");
    let text = r#"
        DEFINE TABLE person SCHEMAFULL PERMISSIONS FOR select FULL;
        DEFINE FIELD name ON TABLE person TYPE string;
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "clean SurrealQL should not produce syntax diagnostics: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn optional_chaining_produces_no_syntax_diagnostics() {
    let u = uri("optional-chain.surql");
    let text = r#"
        DEFINE FIELD firstName ON user TYPE option<string> VALUE $value.?.trim();
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "optional chaining should not produce syntax diagnostics: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn hover_for_js_function_shows_javascript_badge() {
    let u = uri("functions.surql");
    let mut ws = WorkspaceIndex::default();
    let analysis = DocumentAnalysis {
        uri: u.clone(),
        text: String::new(),
        tree: tree_of(""),
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: vec![FunctionDef {
            name: "fn::slugify".to_string(),
            params: vec![surrealql_language_server::semantic::types::FunctionParam {
                name: "$text".to_string(),
                type_expr: Some(TypeExpr::Scalar("string".to_string())),
            }],
            return_type: Some(TypeExpr::Scalar("string".to_string())),
            language: FunctionLanguage::JavaScript,
            comment: Some("Converts text to a URL-safe slug.".to_string()),
            permissions: Vec::new(),
            origin: SymbolOrigin::Local,
            explicit: true,
            inference: None,
            location: empty_location("functions.surql"),
            selection_range: empty_range(),
            body_range: None,
            called_functions: Vec::new(),
        }],
        params: Vec::new(),
        accesses: Vec::new(),
        analyzers: Vec::new(),
        query_facts: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    ws.documents.insert(u, Arc::new(analysis));
    let model = MergedSemanticModel::build(&ws, &Default::default());
    let hover = model
        .hover_markdown_for_token("fn::slugify", None)
        .expect("hover");
    assert!(
        hover.contains("JavaScript"),
        "hover should mention JavaScript language"
    );
    assert!(
        hover.contains("fn::slugify"),
        "hover should include function name"
    );
    assert!(
        hover.contains("Converts text to a URL-safe slug"),
        "hover should include comment"
    );
}

#[test]
fn hover_for_surql_function_with_return_type_shows_arrow() {
    let u = uri("functions.surql");
    let mut ws = WorkspaceIndex::default();
    let analysis = DocumentAnalysis {
        uri: u.clone(),
        text: String::new(),
        tree: tree_of(""),
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: vec![FunctionDef {
            name: "fn::double".to_string(),
            params: vec![surrealql_language_server::semantic::types::FunctionParam {
                name: "$n".to_string(),
                type_expr: Some(TypeExpr::Scalar("number".to_string())),
            }],
            return_type: Some(TypeExpr::Scalar("number".to_string())),
            language: FunctionLanguage::SurrealQL,
            comment: None,
            permissions: Vec::new(),
            origin: SymbolOrigin::Local,
            explicit: true,
            inference: None,
            location: empty_location("functions.surql"),
            selection_range: empty_range(),
            body_range: None,
            called_functions: Vec::new(),
        }],
        params: Vec::new(),
        accesses: Vec::new(),
        analyzers: Vec::new(),
        query_facts: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    ws.documents.insert(u, Arc::new(analysis));
    let model = MergedSemanticModel::build(&ws, &Default::default());
    let hover = model
        .hover_markdown_for_token("fn::double", None)
        .expect("hover");
    assert!(
        hover.contains("->"),
        "hover signature should include return type arrow"
    );
    assert!(hover.contains("number"), "hover should show return type");
    assert!(
        !hover.contains("JavaScript"),
        "SurrealQL function should not show JS badge"
    );
}

#[test]
fn hover_for_table_shows_schema_and_permissions() {
    let u = uri("schema.surql");
    let mut ws = WorkspaceIndex::default();
    ws.documents.insert(
        u.clone(),
        Arc::new(DocumentAnalysis {
            uri: u.clone(),
            text: String::new(),
            tree: tree_of(""),
            tables: vec![TableDef {
                name: "account".to_string(),
                schema_mode: Some("schemafull".to_string()),
                comment: Some("User accounts".to_string()),
                permissions: vec![PermissionRule {
                    actions: vec![QueryAction::Select],
                    mode: PermissionMode::Full,
                    raw: "PERMISSIONS FOR select FULL".to_string(),
                    origin: SymbolOrigin::Local,
                    location: None,
                }],
                origin: SymbolOrigin::Local,
                explicit: true,
                inference: None,
                location: empty_location("schema.surql"),
            }],
            events: Vec::new(),
            indexes: Vec::new(),
            fields: Vec::new(),
            functions: Vec::new(),
            params: Vec::new(),
            accesses: Vec::new(),
            analyzers: Vec::new(),
            query_facts: Vec::new(),
            references: Vec::new(),
            syntax_diagnostics: Vec::new(),
            document_symbols: Vec::new(),
        }),
    );
    let model = MergedSemanticModel::build(&ws, &Default::default());
    let hover = model
        .hover_markdown_for_token("account", None)
        .expect("hover");
    assert!(hover.contains("schemafull"));
    assert!(hover.contains("User accounts"));
    assert!(hover.contains("public")); // FULL → "public" posture
}

#[test]
fn hover_for_unknown_token_returns_none() {
    let model = MergedSemanticModel::default();
    assert!(
        model
            .hover_markdown_for_token("nonexistent_table_xyz", None)
            .is_none()
    );
}

#[test]
fn hover_for_builtin_function_includes_docs_link() {
    let model = MergedSemanticModel::default();
    let hover = model
        .hover_markdown_for_token("string::lowercase", None)
        .expect("hover");
    assert!(hover.contains("SurrealDB reference") || hover.contains("surrealdb.com"));
}

#[test]
fn hover_for_special_variable() {
    let model = MergedSemanticModel::default();
    let hover = model
        .hover_markdown_for_token("$auth", None)
        .expect("hover");
    assert!(!hover.is_empty());
}

#[test]
fn completion_includes_user_js_function() {
    let u = uri("functions.surql");
    let mut ws = WorkspaceIndex::default();
    ws.documents.insert(
        u.clone(),
        Arc::new(DocumentAnalysis {
            uri: u.clone(),
            text: String::new(),
            tree: tree_of(""),
            tables: Vec::new(),
            events: Vec::new(),
            indexes: Vec::new(),
            fields: Vec::new(),
            functions: vec![FunctionDef {
                name: "fn::slugify".to_string(),
                params: Vec::new(),
                return_type: None,
                language: FunctionLanguage::JavaScript,
                comment: None,
                permissions: Vec::new(),
                origin: SymbolOrigin::Local,
                explicit: true,
                inference: None,
                location: empty_location("functions.surql"),
                selection_range: empty_range(),
                body_range: None,
                called_functions: Vec::new(),
            }],
            params: Vec::new(),
            accesses: Vec::new(),
            analyzers: Vec::new(),
            query_facts: Vec::new(),
            references: Vec::new(),
            syntax_diagnostics: Vec::new(),
            document_symbols: Vec::new(),
        }),
    );
    let model = MergedSemanticModel::build(&ws, &Default::default());
    let items = model.completion_items("fn::sl", false, None, None, None);
    assert!(
        items.iter().any(|item| item.label == "fn::slugify"),
        "JS function should appear in completions"
    );
}

#[test]
fn completion_includes_keywords_and_builtins() {
    let model = MergedSemanticModel::default();
    let items = model.completion_items("SEL", false, None, None, None);
    assert!(items.iter().any(|i| i.label == "SELECT"));

    let items = model.completion_items("string::lo", false, None, None, None);
    assert!(items.iter().any(|i| i.label == "string::lowercase"));
}

#[test]
fn completion_in_record_type_context_shows_only_tables() {
    let mut model = MergedSemanticModel::default();
    model.tables.insert(
        "person".to_string(),
        TableDef {
            name: "person".to_string(),
            schema_mode: Some("schemafull".to_string()),
            comment: None,
            permissions: Vec::new(),
            origin: SymbolOrigin::Local,
            explicit: true,
            inference: None,
            location: empty_location("schema.surql"),
        },
    );
    let items = model.completion_items("per", true, None, None, None);
    assert!(items.iter().any(|i| i.label == "person"));
    // Keywords should not appear in record type context
    assert!(
        !items
            .iter()
            .any(|i| i.label == "SELECT" || i.label == "CREATE")
    );
}

#[test]
fn completion_for_fields_scoped_to_statement_target_table() {
    let mut model = MergedSemanticModel::default();
    model.fields.insert(
        ("product".to_string(), "price".to_string()),
        FieldDef {
            table: "product".to_string(),
            name: "price".to_string(),
            type_expr: Some(TypeExpr::Scalar("number".to_string())),
            comment: None,
            permissions: Vec::new(),
            origin: SymbolOrigin::Local,
            explicit: true,
            inference: None,
            location: empty_location("schema.surql"),
        },
    );
    let fact = QueryFact {
        action: QueryAction::Select,
        target_tables: vec!["product".to_string()],
        touched_fields: Vec::new(),
        dynamic: false,
        location: empty_location("schema.surql"),
        source_preview: "SELECT price FROM product".to_string(),
        target_refs: Vec::new(),
        field_refs: Vec::new(),
        target_resolution: TargetResolution::Static,
    };
    let items = model.completion_items("pr", false, None, Some(&fact), None);
    assert!(items.iter().any(|i| i.label == "price"));
}

#[test]
fn no_diagnostics_for_allowed_permission() {
    let u = uri("query.surql");
    let table = TableDef {
        name: "thing".to_string(),
        schema_mode: None,
        comment: None,
        permissions: vec![PermissionRule {
            actions: vec![QueryAction::Select],
            mode: PermissionMode::Full,
            raw: "PERMISSIONS FOR select FULL".to_string(),
            origin: SymbolOrigin::Local,
            location: None,
        }],
        origin: SymbolOrigin::Local,
        explicit: true,
        inference: None,
        location: empty_location("schema.surql"),
    };
    let mut model = MergedSemanticModel::default();
    model.tables.insert("thing".to_string(), table);
    let analysis = DocumentAnalysis {
        uri: u.clone(),
        text: String::new(),
        tree: tree_of(""),
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: Vec::new(),
        params: Vec::new(),
        accesses: Vec::new(),
        analyzers: Vec::new(),
        query_facts: vec![QueryFact {
            action: QueryAction::Select,
            target_tables: vec!["thing".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: empty_location("query.surql"),
            source_preview: "SELECT * FROM thing".to_string(),
            target_refs: Vec::new(),
            field_refs: Vec::new(),
            target_resolution: TargetResolution::Static,
        }],
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    let diagnostics = model.semantic_diagnostics(&analysis, &ServerSettings::default());
    assert!(diagnostics.is_empty());
}

#[test]
fn error_diagnostic_for_denied_permission() {
    // SELECT and RELATE are exempt from static permission checks, so
    // use CREATE to exercise the denied-permission diagnostic path.
    let u = uri("query.surql");
    let table = TableDef {
        name: "secret".to_string(),
        schema_mode: None,
        comment: None,
        permissions: vec![PermissionRule {
            actions: vec![QueryAction::Create],
            mode: PermissionMode::None,
            raw: "PERMISSIONS FOR create NONE".to_string(),
            origin: SymbolOrigin::Local,
            location: None,
        }],
        origin: SymbolOrigin::Local,
        explicit: true,
        inference: None,
        location: empty_location("schema.surql"),
    };
    let mut model = MergedSemanticModel::default();
    model.tables.insert("secret".to_string(), table);
    let analysis = DocumentAnalysis {
        uri: u.clone(),
        text: String::new(),
        tree: tree_of(""),
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: Vec::new(),
        params: Vec::new(),
        accesses: Vec::new(),
        analyzers: Vec::new(),
        query_facts: vec![QueryFact {
            action: QueryAction::Create,
            target_tables: vec!["secret".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: empty_location("query.surql"),
            source_preview: "CREATE secret".to_string(),
            target_refs: Vec::new(),
            field_refs: Vec::new(),
            target_resolution: TargetResolution::Static,
        }],
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    let diagnostics = model.semantic_diagnostics(&analysis, &ServerSettings::default());
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].severity,
        Some(tower_lsp_server::ls_types::DiagnosticSeverity::ERROR)
    );
    assert_eq!(
        diagnostics[0].source.as_deref(),
        Some("surreal-language-server")
    );
}

#[test]
fn warning_for_unknown_table_in_query() {
    let model = MergedSemanticModel::default();
    let u = uri("query.surql");
    let analysis = DocumentAnalysis {
        uri: u.clone(),
        text: String::new(),
        tree: tree_of(""),
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: Vec::new(),
        params: Vec::new(),
        accesses: Vec::new(),
        analyzers: Vec::new(),
        query_facts: vec![QueryFact {
            action: QueryAction::Select,
            target_tables: vec!["totally_unknown_table".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: empty_location("query.surql"),
            source_preview: "SELECT * FROM totally_unknown_table".to_string(),
            target_refs: Vec::new(),
            field_refs: Vec::new(),
            target_resolution: TargetResolution::Static,
        }],
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    let diagnostics = model.semantic_diagnostics(&analysis, &ServerSettings::default());
    assert!(!diagnostics.is_empty());
    assert!(diagnostics[0].message.contains("Unknown table"));
    assert!(diagnostics[0].message.contains("totally_unknown_table"));
    assert_eq!(
        diagnostics[0].severity,
        Some(tower_lsp_server::ls_types::DiagnosticSeverity::WARNING)
    );
    assert_eq!(
        diagnostics[0].source.as_deref(),
        Some("surreal-language-server")
    );
    assert_eq!(
        diagnostics[0].code,
        Some(tower_lsp_server::ls_types::NumberOrString::String(
            "unknown-table".to_string()
        ))
    );
    assert_eq!(
        diagnostics[0]
            .data
            .as_ref()
            .and_then(|data| data.get("table"))
            .and_then(|table| table.as_str()),
        Some("totally_unknown_table")
    );
}

#[test]
fn role_based_permission_allowed_for_matching_context() {
    let u = uri("schema.surql");
    let context = AuthContext {
        name: "admin".to_string(),
        roles: vec!["admin".to_string()],
        auth_record: None,
        claims: serde_json::Value::Object(Default::default()),
        session: serde_json::Value::Object(Default::default()),
        variables: serde_json::Value::Object(Default::default()),
    };
    let settings = ServerSettings {
        auth_contexts: vec![context],
        active_auth_context: Some("admin".to_string()),
        ..ServerSettings::default()
    };
    let table = TableDef {
        name: "orders".to_string(),
        schema_mode: None,
        comment: None,
        permissions: vec![PermissionRule {
            actions: vec![QueryAction::Select],
            mode: PermissionMode::Expression("WHERE $auth.roles CONTAINS 'admin'".to_string()),
            raw: "FOR select WHERE $auth.roles CONTAINS 'admin'".to_string(),
            origin: SymbolOrigin::Local,
            location: None,
        }],
        origin: SymbolOrigin::Local,
        explicit: true,
        inference: None,
        location: empty_location("schema.surql"),
    };
    let mut model = MergedSemanticModel::default();
    model.tables.insert("orders".to_string(), table);
    let analysis = DocumentAnalysis {
        uri: u,
        text: String::new(),
        tree: tree_of(""),
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: Vec::new(),
        params: Vec::new(),
        accesses: Vec::new(),
        analyzers: Vec::new(),
        query_facts: vec![QueryFact {
            action: QueryAction::Select,
            target_tables: vec!["orders".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: empty_location("query.surql"),
            source_preview: "SELECT * FROM orders".to_string(),
            target_refs: Vec::new(),
            field_refs: Vec::new(),
            target_resolution: TargetResolution::Static,
        }],
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    let diagnostics = model.semantic_diagnostics(&analysis, &settings);
    assert!(
        diagnostics.is_empty(),
        "admin role should satisfy the permission"
    );
}

#[test]
fn definition_resolves_local_function() {
    let u = uri("fn.surql");
    let def_range = Range {
        start: Position::new(1, 16),
        end: Position::new(1, 30),
    };
    let mut model = MergedSemanticModel::default();
    model.functions.insert(
        "fn::compute".to_string(),
        FunctionDef {
            name: "fn::compute".to_string(),
            params: Vec::new(),
            return_type: None,
            language: FunctionLanguage::SurrealQL,
            comment: None,
            permissions: Vec::new(),
            origin: SymbolOrigin::Local,
            explicit: true,
            inference: None,
            location: Location::new(u.clone(), def_range),
            selection_range: def_range,
            body_range: None,
            called_functions: Vec::new(),
        },
    );
    let def = model
        .definition_for_token("fn::compute")
        .expect("definition");
    assert_eq!(def.uri, u);
    assert_eq!(def.range, def_range);
}

#[test]
fn definition_of_remote_function_returns_none() {
    let mut model = MergedSemanticModel::default();
    model.functions.insert(
        "fn::remote".to_string(),
        FunctionDef {
            name: "fn::remote".to_string(),
            params: Vec::new(),
            return_type: None,
            language: FunctionLanguage::SurrealQL,
            comment: None,
            permissions: Vec::new(),
            origin: SymbolOrigin::Remote,
            explicit: true,
            inference: None,
            location: empty_location("fn.surql"),
            selection_range: empty_range(),
            body_range: None,
            called_functions: Vec::new(),
        },
    );
    assert!(model.definition_for_token("fn::remote").is_none());
}

#[test]
fn rename_produces_edits_for_all_references() {
    let u = uri("fn.surql");
    let call_range = Range {
        start: Position::new(5, 0),
        end: Position::new(5, 11),
    };
    let def_range = Range {
        start: Position::new(1, 16),
        end: Position::new(1, 27),
    };
    let mut model = MergedSemanticModel::default();
    model.functions.insert(
        "fn::old_name".to_string(),
        FunctionDef {
            name: "fn::old_name".to_string(),
            params: Vec::new(),
            return_type: None,
            language: FunctionLanguage::SurrealQL,
            comment: None,
            permissions: Vec::new(),
            origin: SymbolOrigin::Local,
            explicit: true,
            inference: None,
            location: Location::new(u.clone(), def_range),
            selection_range: def_range,
            body_range: None,
            called_functions: Vec::new(),
        },
    );
    model.function_references.insert(
        "fn::old_name".to_string(),
        vec![Location::new(u.clone(), call_range)],
    );

    let edits = model
        .rename_edits("fn::old_name", "fn::new_name")
        .expect("edits");
    let all_edits: Vec<_> = edits.values().flatten().collect();
    assert_eq!(
        all_edits.len(),
        2,
        "should produce one edit for definition and one for reference"
    );
    assert!(all_edits.iter().all(|e| e.new_text == "fn::new_name"));
}

#[test]
fn rename_of_remote_function_returns_none() {
    let mut model = MergedSemanticModel::default();
    model.functions.insert(
        "fn::remote".to_string(),
        FunctionDef {
            name: "fn::remote".to_string(),
            params: Vec::new(),
            return_type: None,
            language: FunctionLanguage::SurrealQL,
            comment: None,
            permissions: Vec::new(),
            origin: SymbolOrigin::Remote,
            explicit: true,
            inference: None,
            location: empty_location("fn.surql"),
            selection_range: empty_range(),
            body_range: None,
            called_functions: Vec::new(),
        },
    );
    assert!(model.rename_edits("fn::remote", "fn::new_name").is_none());
}

#[test]
fn local_function_overrides_remote() {
    let u = uri("fn.surql");
    let remote = DocumentAnalysis {
        uri: uri("remote.surql"),
        text: String::new(),
        tree: tree_of(""),
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: vec![FunctionDef {
            name: "fn::util".to_string(),
            params: Vec::new(),
            return_type: None,
            language: FunctionLanguage::SurrealQL,
            comment: Some("remote version".to_string()),
            permissions: Vec::new(),
            origin: SymbolOrigin::Remote,
            explicit: true,
            inference: None,
            location: empty_location("remote.surql"),
            selection_range: empty_range(),
            body_range: None,
            called_functions: Vec::new(),
        }],
        params: Vec::new(),
        accesses: Vec::new(),
        analyzers: Vec::new(),
        query_facts: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    let local = DocumentAnalysis {
        uri: u.clone(),
        text: String::new(),
        tree: tree_of(""),
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: vec![FunctionDef {
            name: "fn::util".to_string(),
            params: Vec::new(),
            return_type: None,
            language: FunctionLanguage::SurrealQL,
            comment: Some("local version".to_string()),
            permissions: Vec::new(),
            origin: SymbolOrigin::Local,
            explicit: true,
            inference: None,
            location: empty_location("fn.surql"),
            selection_range: empty_range(),
            body_range: None,
            called_functions: Vec::new(),
        }],
        params: Vec::new(),
        accesses: Vec::new(),
        analyzers: Vec::new(),
        query_facts: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    let mut ws = WorkspaceIndex::default();
    ws.documents.insert(remote.uri.clone(), Arc::new(remote));
    ws.documents.insert(local.uri.clone(), Arc::new(local));
    let model = MergedSemanticModel::build(&ws, &Default::default());
    assert_eq!(
        model.functions["fn::util"].comment.as_deref(),
        Some("local version"),
        "local definition should override remote"
    );
}

#[test]
fn record_type_context_detected_mid_expression() {
    let source = "DEFINE FIELD owner ON TABLE event TYPE option<record<us";
    let pos = Position::new(0, source.len() as u32);
    assert!(is_record_type_context(source, pos));
}

#[test]
fn record_type_context_not_detected_after_closing_angle() {
    let source = "DEFINE FIELD owner ON TABLE event TYPE option<record<user>> SELECT";
    let pos = Position::new(0, source.len() as u32);
    assert!(!is_record_type_context(source, pos));
}

#[test]
fn workspace_symbols_search_covers_tables_fields_functions() {
    let u = uri("schema.surql");
    let mut ws = WorkspaceIndex::default();
    ws.documents.insert(
        u.clone(),
        Arc::new(DocumentAnalysis {
            uri: u.clone(),
            text: String::new(),
            tree: tree_of(""),
            tables: vec![TableDef {
                name: "invoice".to_string(),
                schema_mode: None,
                comment: None,
                permissions: Vec::new(),
                origin: SymbolOrigin::Local,
                explicit: true,
                inference: None,
                location: empty_location("schema.surql"),
            }],
            events: Vec::new(),
            indexes: Vec::new(),
            fields: vec![FieldDef {
                table: "invoice".to_string(),
                name: "total".to_string(),
                type_expr: Some(TypeExpr::Scalar("number".to_string())),
                comment: None,
                permissions: Vec::new(),
                origin: SymbolOrigin::Local,
                explicit: true,
                inference: None,
                location: empty_location("schema.surql"),
            }],
            functions: vec![FunctionDef {
                name: "fn::calc_tax".to_string(),
                params: Vec::new(),
                return_type: None,
                language: FunctionLanguage::SurrealQL,
                comment: None,
                permissions: Vec::new(),
                origin: SymbolOrigin::Local,
                explicit: true,
                inference: None,
                location: empty_location("schema.surql"),
                selection_range: empty_range(),
                body_range: None,
                called_functions: Vec::new(),
            }],
            params: Vec::new(),
            accesses: Vec::new(),
            analyzers: Vec::new(),
            query_facts: Vec::new(),
            references: Vec::new(),
            syntax_diagnostics: Vec::new(),
            document_symbols: Vec::new(),
        }),
    );
    let model = MergedSemanticModel::build(&ws, &Default::default());
    let results = model.workspace_symbol_items("invoice");
    assert!(
        results.iter().any(|s| s.name == "invoice"),
        "table should be in results"
    );
    assert!(
        results.iter().any(|s| s.name == "invoice.total"),
        "field should be in results"
    );

    let fn_results = model.workspace_symbol_items("calc_tax");
    assert!(fn_results.iter().any(|s| s.name == "fn::calc_tax"));
}

#[test]
fn code_action_suggests_add_permissions_for_table_without_rules() {
    let u = uri("schema.surql");
    let analysis = DocumentAnalysis {
        uri: u.clone(),
        text: String::new(),
        tree: tree_of(""),
        tables: vec![TableDef {
            name: "widget".to_string(),
            schema_mode: Some("schemafull".to_string()),
            comment: None,
            permissions: Vec::new(),
            origin: SymbolOrigin::Local,
            explicit: true,
            inference: None,
            location: empty_location("schema.surql"),
        }],
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: Vec::new(),
        params: Vec::new(),
        accesses: Vec::new(),
        analyzers: Vec::new(),
        query_facts: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    let model = MergedSemanticModel::default();
    let actions = model.code_actions(&u, &analysis, &[]);
    assert!(
        actions.iter().any(|a| {
            if let tower_lsp_server::ls_types::CodeActionOrCommand::CodeAction(ca) = a {
                ca.title.contains("widget") && ca.title.to_lowercase().contains("permissions")
            } else {
                false
            }
        }),
        "should suggest adding PERMISSIONS clause to widget"
    );
}

#[test]
fn full_analysis_pipeline_js_and_surql_mixed() {
    let u = uri("all.surql");
    let text = r#"
        DEFINE TABLE product SCHEMAFULL PERMISSIONS FOR select FULL;
        DEFINE FIELD name ON TABLE product TYPE string;
        DEFINE FIELD price ON TABLE product TYPE number;
        DEFINE INDEX product_name ON TABLE product FIELDS name UNIQUE;
        DEFINE FUNCTION fn::format_price($amount: number) {
            RETURN function(amount) {
                return '$' + amount.toFixed(2);
            };
        };
        DEFINE FUNCTION fn::discounted($amount: number, $pct: number) {
            RETURN $amount * (1 - ($pct / 100));
        };
        SELECT name, fn::format_price(price) FROM product;
    "#;
    let analysis = analyze_document(u.clone(), text, SymbolOrigin::Local).expect("analysis");

    assert_eq!(analysis.tables.iter().filter(|t| t.explicit).count(), 1);
    assert_eq!(analysis.fields.iter().filter(|f| f.explicit).count(), 2);
    assert_eq!(analysis.indexes.len(), 1);
    assert_eq!(analysis.functions.len(), 2);

    let js_fn = analysis
        .functions
        .iter()
        .find(|f| f.name == "fn::format_price")
        .expect("js fn");
    assert_eq!(js_fn.language, FunctionLanguage::JavaScript);
    assert!(js_fn.called_functions.is_empty());

    let sq_fn = analysis
        .functions
        .iter()
        .find(|f| f.name == "fn::discounted")
        .expect("sq fn");
    assert_eq!(sq_fn.language, FunctionLanguage::SurrealQL);

    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "valid SurrealDB v3 syntax should produce no diagnostics, got: {:?}",
        analysis.syntax_diagnostics
    );
}

// Smoke tests for scripting functions in non-FUNCTION contexts. The
// parity-branch grammar mirrors lezer's `ThenClause` body, which is
// `commaSep<(SubQuery | Block)>` — a bare `function() { ... }` must be
// wrapped in `{ ... }` or `( ... )` to satisfy the grammar.
#[test]
fn scripting_function_in_define_event() {
    let u = uri("events.surql");
    let text = r#"
        DEFINE EVENT score ON TABLE person WHEN $event = 'CREATE'
            THEN { RETURN function() { return { ok: true }; }; };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "got: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn scripting_function_in_define_api() {
    let u = uri("api.surql");
    let text = r#"
        DEFINE API '/test' FOR get THEN { RETURN function() { return { status: 200 }; }; };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "got: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn scripting_function_nested_braces_in_event() {
    let u = uri("events.surql");
    let text = r#"
        DEFINE EVENT complex ON TABLE t THEN {
            RETURN function() {
                const obj = { a: 1, b: { c: 2 } };
                return obj;
            };
        };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "got: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn scripting_function_as_value_in_create() {
    let u = uri("create.surql");
    let text = r#"
        CREATE person SET scores = function() { return [1, 2, 3].map(v => v * 10); };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "got: {:?}",
        analysis.syntax_diagnostics
    );
}

// --- Trailing comma tests (issue #2) ---

/// Grammar v3 accepts trailing commas in `DEFINE FUNCTION` parameter
/// lists (matching SurrealQL, which permits them). A trailing comma is
/// therefore valid and must not produce a syntax diagnostic.
#[test]
fn trailing_comma_in_define_function_params_no_diagnostic() {
    let u = uri("trailing.surql");
    let text = r#"
        DEFINE FUNCTION fn::greet($name: string,) {
            RETURN "hi";
        };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "trailing comma in DEFINE FUNCTION params should be valid: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn trailing_comma_in_define_function_multiple_params_no_diagnostic() {
    let u = uri("trailing.surql");
    let text = r#"
        DEFINE FUNCTION fn::add($a: number, $b: number,) -> number {
            RETURN $a + $b;
        };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "trailing comma in multi-param DEFINE FUNCTION should be valid: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn trailing_comma_in_function_call_args_no_diagnostic() {
    let u = uri("trailing.surql");
    let text = r#"
        SELECT fn::add(1, 2,) FROM person;
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "trailing comma in function call arguments should be valid: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn trailing_comma_in_object_literal_no_diagnostic() {
    let u = uri("trailing.surql");
    let text = r#"
        LET $p = { name: "Alice", age: 30, };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "trailing comma in object literal should be valid: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn trailing_comma_in_object_single_property_no_diagnostic() {
    let u = uri("trailing.surql");
    let text = r#"
        LET $p = { name: "Alice", };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "trailing comma in single-property object should be valid: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn no_regression_define_function_without_trailing_comma() {
    let u = uri("no_trailing.surql");
    let text = r#"
        DEFINE FUNCTION fn::check($role: string, $level: number) -> bool {
            RETURN $level > 0;
        };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "standard DEFINE FUNCTION (no trailing comma) must still be valid: {:?}",
        analysis.syntax_diagnostics
    );
    assert_eq!(analysis.functions[0].params.len(), 2);
}

#[test]
fn no_regression_object_without_trailing_comma() {
    let u = uri("no_trailing.surql");
    let text = r#"
        LET $x = { a: 1, b: 2 };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "standard object (no trailing comma) must still be valid: {:?}",
        analysis.syntax_diagnostics
    );
}

#[test]
fn prefix_not_in_where_clause_no_diagnostic() {
    let u = uri("prefix_not.surql");
    let text = r#"
        SELECT VALUE slug
        FROM event
        WHERE !external_url;

        SELECT VALUE slug
        FROM event
        WHERE !$external_url;
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert!(
        analysis.syntax_diagnostics.is_empty(),
        "prefix ! on fields and parameters should be valid: {:?}",
        analysis.syntax_diagnostics
    );
}

// ── Semantic tokens ────────────────────────────────────────────────────────

use surrealql_language_server::semantic::highlight::{
    collect_semantic_tokens, collect_semantic_tokens_range, legend,
};
use surrealql_language_server::semantic::text::position_to_offset;
use tower_lsp_server::ls_types::SemanticToken;
use tree_sitter::Tree;

/// The cached parse tree for `source`, via the same path the server uses.
fn tree_of(source: &str) -> Tree {
    analyze_document(uri("highlight.surql"), source, SymbolOrigin::Local)
        .expect("analysis")
        .tree
}

/// One decoded token: its source text, type index, and modifier bitset.
struct Tok {
    text: String,
    ty: u32,
    mods: u32,
}

/// Decode an encoded token stream back into absolute `Tok`s so
/// assertions can talk about real spans instead of offsets.
fn decode(tokens: Vec<SemanticToken>, source: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let (mut line, mut start) = (0u32, 0u32);
    for token in tokens {
        line += token.delta_line;
        start = if token.delta_line == 0 {
            start + token.delta_start
        } else {
            token.delta_start
        };
        let begin = position_to_offset(source, Position::new(line, start));
        let end = position_to_offset(source, Position::new(line, start + token.length));
        out.push(Tok {
            text: source[begin..end].to_string(),
            ty: token.token_type,
            mods: token.token_modifiers_bitset,
        });
    }
    out
}

fn decode_tokens(source: &str) -> Vec<Tok> {
    decode(collect_semantic_tokens(&tree_of(source), source), source)
}

/// The first token whose text equals `needle`.
fn find<'a>(tokens: &'a [Tok], needle: &str) -> Option<&'a Tok> {
    tokens.iter().find(|tok| tok.text == needle)
}

/// Type index of the first token whose text equals `needle`.
fn type_of(tokens: &[Tok], needle: &str) -> Option<u32> {
    find(tokens, needle).map(|tok| tok.ty)
}

#[test]
fn semantic_tokens_use_standard_legend_order() {
    let legend = legend();
    let types: Vec<&str> = legend.token_types.iter().map(|t| t.as_str()).collect();
    assert_eq!(
        types,
        vec![
            "keyword",
            "function",
            "parameter",
            "type",
            "string",
            "number",
            "comment",
            "variable",
        ],
        "legend order is part of the wire protocol and must not drift"
    );
    let mods: Vec<&str> = legend.token_modifiers.iter().map(|m| m.as_str()).collect();
    assert_eq!(
        mods,
        vec!["declaration", "defaultLibrary"],
        "modifier bit order is part of the wire protocol and must not drift"
    );
}

#[test]
fn semantic_tokens_classify_core_node_kinds() {
    // keyword=0 function=1 parameter=2 type=3 string=4 number=5 comment=6 variable=7
    let source = "-- pick a user\nLET $count = math::abs(-3) + duration::secs(5s);\nSELECT name FROM user:tobie WHERE age > 18 AND note = \"hi\";";
    let tokens = decode_tokens(source);

    assert_eq!(type_of(&tokens, "LET"), Some(0), "LET is a keyword");
    assert_eq!(type_of(&tokens, "SELECT"), Some(0), "SELECT is a keyword");
    assert_eq!(type_of(&tokens, "$count"), Some(2), "$count is a parameter");
    assert_eq!(
        type_of(&tokens, "math::abs"),
        Some(1),
        "math::abs is a function name"
    );
    assert_eq!(
        type_of(&tokens, "5s"),
        Some(5),
        "duration is number-coloured"
    );
    assert_eq!(type_of(&tokens, "18"), Some(5), "18 is a number");
    assert_eq!(type_of(&tokens, "\"hi\""), Some(4), "\"hi\" is a string");
    assert_eq!(
        type_of(&tokens, "user:tobie"),
        Some(7),
        "record id is variable-coloured"
    );
    assert_eq!(
        type_of(&tokens, "-- pick a user"),
        Some(6),
        "line comment is a comment"
    );
}

#[test]
fn semantic_tokens_split_multiline_block_comment_per_line() {
    let source = "/* line one\nline two */\nRETURN 1;";
    let tokens = decode_tokens(source);
    let comment_lines: Vec<&str> = tokens
        .iter()
        .filter(|tok| tok.ty == 6)
        .map(|tok| tok.text.as_str())
        .collect();
    assert_eq!(
        comment_lines,
        vec!["/* line one", "line two */"],
        "a block comment must yield one single-line token per line"
    );
}

#[test]
fn semantic_tokens_empty_for_blank_document() {
    assert!(collect_semantic_tokens(&tree_of(""), "").is_empty());
}

// keyword=0 function=1 parameter=2 type=3 string=4 number=5 comment=6 variable=7
// modifier bits: declaration=1<<0, defaultLibrary=1<<1
const MOD_DECLARATION: u32 = 1 << 0;
const MOD_DEFAULT_LIBRARY: u32 = 1 << 1;

#[test]
fn semantic_tokens_mark_declarations() {
    let source = "DEFINE FUNCTION fn::double($n: number) { RETURN fn::double($n); };\nDEFINE PARAM $page VALUE 20;";
    let tokens = decode_tokens(source);

    // The defining occurrence carries `declaration`; the call site does not.
    let defs: Vec<&Tok> = tokens.iter().filter(|t| t.text == "fn::double").collect();
    assert_eq!(defs.len(), 2, "one definition + one call");
    assert_eq!(defs[0].mods, MOD_DECLARATION, "definition is a declaration");
    assert_eq!(defs[1].mods, 0, "call site is a plain reference");

    // `$n` is declared in the param list, referenced in the body.
    let n_uses: Vec<&Tok> = tokens.iter().filter(|t| t.text == "$n").collect();
    assert_eq!(n_uses[0].mods, MOD_DECLARATION, "param binding declares $n");
    assert_eq!(n_uses[1].mods, 0, "body use of $n is a reference");

    // DEFINE PARAM binds its var directly under the DefineStatement.
    assert_eq!(find(&tokens, "$page").unwrap().mods, MOD_DECLARATION);
}

#[test]
fn semantic_tokens_mark_builtin_functions() {
    let source = "RETURN math::abs(-3) + fn::custom();";
    let tokens = decode_tokens(source);
    assert_eq!(
        find(&tokens, "math::abs").unwrap().mods,
        MOD_DEFAULT_LIBRARY,
        "builtins carry defaultLibrary"
    );
    assert_eq!(
        find(&tokens, "fn::custom").unwrap().mods,
        0,
        "user fn:: calls do not"
    );
}

#[test]
fn semantic_tokens_split_param_name_and_type() {
    // Regression: `ParamDefinition` wraps the name AND its type
    // annotation; they must be coloured separately, not swallowed.
    let source = "DEFINE FUNCTION fn::f($n: number) { RETURN $n; };";
    let tokens = decode_tokens(source);
    assert_eq!(type_of(&tokens, "$n"), Some(2), "$n is a parameter");
    assert_eq!(
        type_of(&tokens, "number"),
        Some(3),
        "number annotation is a type"
    );
}

#[test]
fn semantic_tokens_range_limits_to_viewport() {
    let source = "SELECT 1;\nSELECT 2;\nSELECT 3;";
    // Request only the middle line.
    let range = Range::new(Position::new(1, 0), Position::new(1, 9));
    let tokens = decode(
        collect_semantic_tokens_range(&tree_of(source), source, range),
        source,
    );
    let keywords: Vec<&str> = tokens
        .iter()
        .filter(|t| t.ty == 0)
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(
        keywords,
        vec!["SELECT"],
        "only the in-range SELECT is emitted"
    );
    assert_eq!(
        type_of(&tokens, "2"),
        Some(5),
        "the in-range number is present"
    );
    assert!(
        find(&tokens, "1").is_none() && find(&tokens, "3").is_none(),
        "tokens outside the range are excluded"
    );
}

// ---------------------------------------------------------------------------
// Real-world regression fixture
// ---------------------------------------------------------------------------
//
// `tests/fixtures/adversarial.surql` is a 400-line production schema. It is the
// best adversarial input available: densely typed `DEFINE FUNCTION`s, literal
// union and array-literal field types, `record<a | b>`, inline object parameter
// types, optional-chained method idioms, and `LET`s bound to queries nested
// inside event blocks. Anything the analyzer gets wrong tends to show up here
// first.

fn adversarial_analysis() -> DocumentAnalysis {
    let source = include_str!("fixtures/adversarial.surql");
    analyze_document(uri("real_world.surql"), source, SymbolOrigin::Local)
        .expect("fixture analyzes")
}

#[test]
fn adversarial_fixture_syntax_error_budget() {
    let analysis = adversarial_analysis();
    let reported: Vec<String> = analysis
        .syntax_diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "line {}: {}",
                diagnostic.range.start.line + 1,
                diagnostic.message
            )
        })
        .collect();

    // These are *grammar* gaps, not analyzer bugs: a handful of
    // constructs in this file the pinned tree-sitter grammar cannot
    // parse. They truncate their enclosing statement, which is why not
    // every `DEFINE FUNCTION` in the file is indexed. The budget exists
    // so the number can only go down; drop it when the grammar catches
    // up. Newer grammar revisions already fix most of them.
    assert!(
        reported.len() <= 6,
        "syntax error budget exceeded ({}): {reported:#?}",
        reported.len()
    );
}

#[test]
fn adversarial_fixture_extracts_declared_parameter_types() {
    let analysis = adversarial_analysis();

    let typed = analysis
        .functions
        .iter()
        .flat_map(|function| &function.params)
        .filter(|param| param.type_expr.is_some())
        .count();
    let total = analysis
        .functions
        .iter()
        .map(|function| function.params.len())
        .sum::<usize>();

    assert!(
        analysis.functions.len() >= 20,
        "expected the fixture's fn:: definitions, got {}",
        analysis.functions.len()
    );
    // Every parameter in this file is annotated, and extraction is
    // all-or-nothing per parameter — so anything below 100% means the
    // `ParamDefinition(VariableName, Colon, Type)` walk has regressed.
    // Before the fix this was 0/49.
    assert_eq!(
        typed, total,
        "every annotated parameter should carry a type"
    );
    assert!(
        total >= 40,
        "expected the fixture's annotated params, got {total}"
    );
}

#[test]
fn adversarial_fixture_registers_no_phantom_union_table() {
    let analysis = adversarial_analysis();

    // `record<orderData | project>` must register two tables, never one
    // called "orderData | project".
    let phantom: Vec<&str> = analysis
        .tables
        .iter()
        .map(|table| table.name.as_str())
        .filter(|name| name.contains('|') || name.contains(' '))
        .collect();
    assert!(phantom.is_empty(), "phantom tables: {phantom:?}");
}

#[test]
fn adversarial_fixture_resolves_select_targets() {
    let analysis = adversarial_analysis();

    let selects: Vec<_> = analysis
        .query_facts
        .iter()
        .filter(|fact| fact.action == QueryAction::Select)
        .collect();
    let resolved = selects.iter().filter(|fact| !fact.dynamic).count();

    assert!(!selects.is_empty(), "fixture contains SELECT statements");
    assert!(
        resolved > 0,
        "no SELECT resolved a static target out of {} — FromClause regression?",
        selects.len()
    );
}

// ---------------------------------------------------------------------------
// Argument type checking
// ---------------------------------------------------------------------------

/// Analyze `source`, build a one-document model, and return its semantic
/// diagnostics.
fn diagnostics_for(source: &str) -> Vec<tower_lsp_server::ls_types::Diagnostic> {
    let analysis =
        analyze_document(uri("check.surql"), source, SymbolOrigin::Local).expect("analysis");
    let workspace = workspace_from(vec![analysis.clone()]);
    let model = MergedSemanticModel::build(&workspace, &Default::default());
    model.semantic_diagnostics(&analysis, &ServerSettings::default())
}

fn codes_of(diagnostics: &[tower_lsp_server::ls_types::Diagnostic]) -> Vec<String> {
    diagnostics
        .iter()
        .filter_map(|d| match &d.code {
            Some(tower_lsp_server::ls_types::NumberOrString::String(code)) => Some(code.clone()),
            _ => None,
        })
        .collect()
}

const DOC_ADD: &str = r#"
DEFINE FUNCTION fn::doc::add($user: record<user>, $doc: {
  line: record<orderLine>,
  asset: record<asset>
}) {
  RETURN $doc;
};
"#;

#[test]
fn reports_a_string_literal_passed_to_a_record_parameter() {
    // The motivating case: `fn::doc::add("", 9)`.
    let source = format!("{DOC_ADD}\nfn::doc::add(\"\", 9);");
    let diagnostics = diagnostics_for(&source);

    let argument_errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| {
            matches!(&d.code,
            Some(tower_lsp_server::ls_types::NumberOrString::String(c)) if c == "argument-type")
        })
        .collect();

    let messages: Vec<&str> = argument_errors.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages,
        vec![
            "Argument 1 of `fn::doc::add` expects `record<user>`, found `string`.",
            "Argument 2 of `fn::doc::add` expects `{ line: record<orderLine>, asset: record<asset> }`, found `int`.",
        ],
        "both arguments are definite mismatches"
    );
    assert!(
        argument_errors
            .iter()
            .all(|d| d.severity == Some(tower_lsp_server::ls_types::DiagnosticSeverity::ERROR))
    );

    // The squiggle must cover only the offending argument, not the whole
    // statement — every other diagnostic in this server uses a
    // statement-wide range, so this is the likeliest thing to regress.
    let range = argument_errors[0].range;
    assert_eq!(range.start.line, range.end.line);
    assert_eq!(
        range.end.character - range.start.character,
        2,
        "range should span exactly the `\"\"` literal"
    );
}

#[test]
fn reports_a_number_passed_to_a_record_parameter() {
    let source = r#"
        DEFINE FUNCTION fn::canUserEdit($user: record<user>, $target: record<orderLine>) -> bool {
            RETURN true;
        };
        RETURN fn::canUserEdit(user:tobie, 42);
    "#;
    let diagnostics = diagnostics_for(source);
    let messages: Vec<_> = diagnostics.iter().map(|d| d.message.as_str()).collect();

    assert!(
        messages
            .contains(&"Argument 2 of `fn::canUserEdit` expects `record<orderLine>`, found `int`."),
        "got {messages:?}"
    );
    // A correct `record<user>` argument must not be flagged.
    assert!(
        !messages.iter().any(|m| m.starts_with("Argument 1 of")),
        "argument 1 is a valid record<user>, got {messages:?}"
    );
}

#[test]
fn reports_wrong_argument_count() {
    let source = format!("{DOC_ADD}\nfn::doc::add(user:tobie);");
    let diagnostics = diagnostics_for(&source);
    let messages: Vec<_> = diagnostics.iter().map(|d| d.message.as_str()).collect();

    assert!(
        messages.contains(&"`fn::doc::add` expects 2 arguments, found 1."),
        "got {messages:?}"
    );
}

#[test]
fn trailing_optional_parameters_may_be_omitted() {
    let source = r#"
        DEFINE FUNCTION fn::update($id: record<orderLine>, $extra: option<object>) {
            RETURN $id;
        };
        RETURN fn::update(orderLine:one);
    "#;
    assert!(
        !codes_of(&diagnostics_for(source)).contains(&"argument-count".to_string()),
        "a trailing option<T> parameter is not required"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Builtin argument count, against the generated catalogue
// ──────────────────────────────────────────────────────────────────────

fn messages_of(diagnostics: &[tower_lsp_server::ls_types::Diagnostic]) -> Vec<String> {
    diagnostics.iter().map(|d| d.message.clone()).collect()
}

#[test]
fn too_many_arguments_to_a_builtin_are_reported() {
    // `string::len(string)` takes one. SurrealDB rejects this outright, so the
    // check cannot fire on a query that runs.
    let diagnostics = diagnostics_for("RETURN string::len('a', 'b');");
    assert!(
        codes_of(&diagnostics).contains(&"argument-count".to_string()),
        "expected argument-count, got {:?}",
        messages_of(&diagnostics)
    );
}

#[test]
fn the_count_message_uses_the_engines_wording() {
    let diagnostics = diagnostics_for("RETURN string::len('a', 'b');");
    assert!(
        messages_of(&diagnostics)
            .iter()
            .any(|message| message.contains("expects 1 argument, found 2")),
        "got {:?}",
        messages_of(&diagnostics)
    );
}

#[test]
fn an_optional_parameter_widens_the_accepted_count() {
    // `string::slice(string, from?, to?)` — one, two and three all legal.
    for source in [
        "RETURN string::slice('abc');",
        "RETURN string::slice('abc', 1);",
        "RETURN string::slice('abc', 1, 2);",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.contains(&"argument-count".to_string()),
            "{source} is legal, got {codes:?}"
        );
    }
    let codes = codes_of(&diagnostics_for("RETURN string::slice('a', 1, 2, 3);"));
    assert!(
        codes.contains(&"argument-count".to_string()),
        "four arguments exceeds the maximum"
    );
}

#[test]
fn a_variadic_builtin_accepts_any_number_of_arguments() {
    for source in [
        "RETURN string::concat('a');",
        "RETURN string::concat('a', 'b', 'c', 'd', 'e', 'f');",
        "RETURN array::concat([1], [2], [3]);",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.contains(&"argument-count".to_string()),
            "{source} is legal, got {codes:?}"
        );
    }
}

#[test]
fn an_empty_argument_list_is_never_reported() {
    // `ArgumentList` is `seq('(', optional(…), ')')`, so this parses clean —
    // and every editor that closes brackets produces it on the `(` keystroke.
    // With no debounce and ERROR severity, reporting it would squiggle every
    // call as it is typed.
    for source in [
        "RETURN string::len();",
        "RETURN math::clamp();",
        "RETURN array::at();",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.contains(&"argument-count".to_string()),
            "{source} is a transient typing state, got {codes:?}"
        );
    }
}

#[test]
fn a_zero_argument_builtin_still_rejects_arguments() {
    // `time::now()` takes none, and its signature *is* known — so unlike an
    // unread signature, a wrong count here is real information.
    let codes = codes_of(&diagnostics_for("RETURN time::now('extra');"));
    assert!(
        codes.contains(&"argument-count".to_string()),
        "time::now takes no arguments, got {codes:?}"
    );
}

#[test]
fn a_name_that_parses_but_cannot_be_called_is_reported() {
    // Nine names sit in the parser's table with no implementation behind them,
    // so the query parses and then fails at run time.
    let diagnostics = diagnostics_for("RETURN duration::set_day(1d, 3);");
    let codes = codes_of(&diagnostics);
    assert!(
        codes.contains(&"not-callable".to_string()),
        "got {:?}",
        messages_of(&diagnostics)
    );
    // A warning, not an error: the claim rests on reading the engine's dispatch
    // tables rather than on the language definition.
    assert!(
        diagnostics
            .iter()
            .filter(|d| matches!(
                &d.code,
                Some(tower_lsp_server::ls_types::NumberOrString::String(code))
                    if code == "not-callable"
            ))
            .all(|d| d.severity == Some(tower_lsp_server::ls_types::DiagnosticSeverity::WARNING)),
        "not-callable must be a warning"
    );
}

#[test]
fn a_callable_function_is_never_reported_as_not_callable() {
    for source in [
        "RETURN time::day(d'2024-01-01T00:00:00Z');",
        "RETURN duration::days(3w);",
        "RETURN object::keys({ a: 1 });",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.contains(&"not-callable".to_string()),
            "{source} is callable, got {codes:?}"
        );
    }
}

#[test]
fn a_function_whose_signature_was_not_read_is_never_flagged() {
    // The nine names the parser accepts and no dispatch arm implements. Their
    // signatures are unknown, so any count must stay silent.
    for source in [
        "RETURN duration::set_day(1d, 3);",
        "RETURN object::matches({}, {});",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.contains(&"argument-count".to_string()),
            "{source} has an unknown signature, got {codes:?}"
        );
    }
}

#[test]
fn a_correct_builtin_call_is_never_flagged() {
    // A sweep across namespaces the hand-written catalogue never covered.
    for source in [
        "RETURN math::clamp(5, 1, 10);",
        "RETURN array::at([1, 2, 3], 0);",
        "RETURN string::replace('a', 'b', 'c');",
        "RETURN time::now();",
        "RETURN rand::uuid();",
        "RETURN crypto::sha256('x');",
        "RETURN vector::add([1], [2]);",
        "RETURN type::record('user', 'x');",
        "RETURN object::keys({ a: 1 });",
        "RETURN encoding::base64::encode('x');",
        "RETURN duration::days(3w);",
        "RETURN session::db();",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.iter().any(|code| code.starts_with("argument-")),
            "{source} is valid SurrealQL, got {codes:?}"
        );
    }
}

#[test]
fn an_unknown_function_name_is_not_an_argument_error() {
    // Unrecognised names are somebody else's diagnostic, not this one's.
    let codes = codes_of(&diagnostics_for("RETURN string::not_a_function('a', 'b');"));
    assert!(
        !codes.iter().any(|code| code.starts_with("argument-")),
        "got {codes:?}"
    );
}

// ──────────────────────────────────────────────────────────────────────
// Declared function return types
// ──────────────────────────────────────────────────────────────────────

#[test]
fn a_return_that_cannot_satisfy_the_declared_type_is_reported() {
    // The reported case. The engine coerces a function's result to its declared
    // return type and fails with `Couldn't coerce return value from function`.
    let source = r#"
        DEFINE FUNCTION fn::beau::number($input: int) -> int {
            RETURN "";
        };
    "#;
    let diagnostics = diagnostics_for(source);
    assert!(
        codes_of(&diagnostics).contains(&"return-type".to_string()),
        "got {:?}",
        messages_of(&diagnostics)
    );
    assert!(
        messages_of(&diagnostics).iter().any(|message| message
            .contains("`fn::beau::number` returns `int`, but this value is `string`")),
        "got {:?}",
        messages_of(&diagnostics)
    );
}

#[test]
fn the_reported_return_range_covers_the_value_not_the_statement() {
    // The squiggle belongs under `""`, not under the whole `RETURN "";`.
    let source = "DEFINE FUNCTION fn::x() -> int { RETURN \"\"; };";
    let diagnostic = diagnostics_for(source)
        .into_iter()
        .find(|diagnostic| {
            diagnostic.code
                == Some(tower_lsp_server::ls_types::NumberOrString::String(
                    "return-type".to_string(),
                ))
        })
        .expect("a return-type diagnostic");
    let start = diagnostic.range.start.character as usize;
    let end = diagnostic.range.end.character as usize;
    assert_eq!(&source[start..end], "\"\"", "range should cover the value");
}

#[test]
fn a_return_that_satisfies_the_declared_type_is_silent() {
    for source in [
        "DEFINE FUNCTION fn::x() -> int { RETURN 1; };",
        // `int` widens into `number`.
        "DEFINE FUNCTION fn::x() -> number { RETURN 1; };",
        "DEFINE FUNCTION fn::x() -> string { RETURN 'ok'; };",
        "DEFINE FUNCTION fn::x() -> array<int> { RETURN [1, 2]; };",
        "DEFINE FUNCTION fn::x() -> option<int> { RETURN NONE; };",
        // No declared type: nothing to check against.
        "DEFINE FUNCTION fn::x() { RETURN ''; };",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.contains(&"return-type".to_string()),
            "{source} is valid, got {codes:?}"
        );
    }
}

#[test]
fn every_return_in_a_body_is_checked() {
    let source = r#"
        DEFINE FUNCTION fn::x($flag: bool) -> int {
            RETURN 1;
            RETURN 'nope';
            RETURN 2;
        };
    "#;
    let count = codes_of(&diagnostics_for(source))
        .iter()
        .filter(|code| *code == "return-type")
        .count();
    assert_eq!(count, 1, "only the string return is wrong");
}

#[test]
fn a_body_ending_in_a_bare_expression_returns_it() {
    // A function without a `RETURN` yields its trailing expression.
    assert!(
        codes_of(&diagnostics_for("DEFINE FUNCTION fn::x() -> int { '' };"))
            .contains(&"return-type".to_string()),
        "the trailing expression is the result"
    );
    assert!(
        !codes_of(&diagnostics_for("DEFINE FUNCTION fn::x() -> int { 1 };"))
            .contains(&"return-type".to_string()),
    );
}

#[test]
fn an_uncertain_return_value_stays_silent() {
    // The rule that governs everything here: report only what is provably
    // wrong. A field access, a call whose type is unmodellable, and a
    // string against a stringly type are all runtime questions.
    for source in [
        "DEFINE FUNCTION fn::x($r: record<person>) -> string { RETURN $r.name; };",
        "DEFINE FUNCTION fn::x() -> string { RETURN type::field('name'); };",
        "DEFINE FUNCTION fn::x() -> datetime { RETURN '2024-01-01T00:00:00Z'; };",
        "DEFINE FUNCTION fn::x() -> int { RETURN SELECT * FROM person; };",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.contains(&"return-type".to_string()),
            "{source} must stay silent, got {codes:?}"
        );
    }
}

#[test]
fn a_return_inside_an_if_branch_is_checked() {
    // A `RETURN` inside an `IF` returns from the *function* — SurrealDB's own
    // `fn::fib` relies on it — so it must satisfy the declared type.
    let source = r#"
        DEFINE FUNCTION fn::beau::number($input: int) -> int {
            IF ($input == 69) {
                RETURN "";
            };
            RETURN 0;
        };
    "#;
    let diagnostics = diagnostics_for(source);
    let reported: Vec<String> = messages_of(&diagnostics)
        .into_iter()
        .filter(|message| message.contains("returns `int`"))
        .collect();
    assert_eq!(
        reported.len(),
        1,
        "only the string return is wrong, got {reported:?}"
    );
    assert!(
        reported[0].contains("`fn::beau::number` returns `int`, but this value is `string`"),
        "got {reported:?}"
    );
}

#[test]
fn every_branch_of_an_if_chain_is_checked() {
    let source = r#"
        DEFINE FUNCTION fn::x($n: int) -> int {
            IF $n = 1 {
                RETURN 'a';
            } ELSE IF $n = 2 {
                RETURN 'b';
            } ELSE {
                RETURN 'c';
            };
        };
    "#;
    let count = codes_of(&diagnostics_for(source))
        .iter()
        .filter(|code| *code == "return-type")
        .count();
    assert_eq!(count, 3, "each branch returns from the function");
}

#[test]
fn a_return_inside_a_nested_if_is_checked() {
    let source = r#"
        DEFINE FUNCTION fn::x($n: int) -> int {
            IF $n > 0 {
                IF $n > 10 {
                    RETURN 'deep';
                };
            };
            RETURN 0;
        };
    "#;
    assert!(
        codes_of(&diagnostics_for(source)).contains(&"return-type".to_string()),
        "nesting does not change which function a RETURN leaves"
    );
}

#[test]
fn a_return_inside_a_for_body_is_checked() {
    let source = r#"
        DEFINE FUNCTION fn::x() -> int {
            FOR $i IN [1, 2] {
                RETURN 'nope';
            };
            RETURN 0;
        };
    "#;
    assert!(
        codes_of(&diagnostics_for(source)).contains(&"return-type".to_string()),
        "a RETURN in a FOR body returns from the function, not the loop"
    );
}

#[test]
fn the_recursion_pattern_from_surrealdbs_own_tests_stays_silent() {
    // Verbatim shape of `fn::fib` in
    // `language-tests/tests/bench/util/recursion-functions.surql`. If widening
    // the walk ever starts reporting this, the widening is wrong.
    let source = r#"
        DEFINE FUNCTION fn::fib($n: int) -> int {
            IF $n < 2 {
                RETURN $n;
            };
            RETURN fn::fib($n - 1) + fn::fib($n - 2);
        };
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(!codes.contains(&"return-type".to_string()), "got {codes:?}");
}

#[test]
fn a_return_inside_a_closure_in_the_body_is_not_the_functions_return() {
    // The closure's `RETURN` returns from the closure.
    let source = r#"
        DEFINE FUNCTION fn::x() -> int {
            LET $f = || { RETURN 'inner'; };
            RETURN 0;
        };
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(!codes.contains(&"return-type".to_string()), "got {codes:?}");
}

#[test]
fn a_return_nested_deeper_than_the_body_is_not_attributed_to_the_function() {
    // A block bound by `LET` returns from that block, not from the function, so
    // attributing its value to the function would report against something the
    // function never returns.
    let source = r#"
        DEFINE FUNCTION fn::x() -> int {
            LET $y = { RETURN ''; };
            RETURN 1;
        };
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(!codes.contains(&"return-type".to_string()), "got {codes:?}");
}

#[test]
fn a_broken_body_produces_no_return_diagnostic() {
    // A syntax diagnostic already covers it; guessing what a body it could not
    // read returns would pile an invented error on top.
    let codes = codes_of(&diagnostics_for(
        "DEFINE FUNCTION fn::x() -> int { RETURN @@@; };",
    ));
    assert!(!codes.contains(&"return-type".to_string()), "got {codes:?}");
}

// ──────────────────────────────────────────────────────────────────────
// Method-call syntax
// ──────────────────────────────────────────────────────────────────────

#[test]
fn a_method_call_argument_is_type_checked() {
    // `IdiomFunction` was never visited, so `$x.foo()` was checked for nothing.
    // SurrealDB's own wording for this is
    // `Incorrect arguments for method extend(). Argument 2 was the wrong type.`
    let diagnostics = diagnostics_for("RETURN { a: 9 }.extend('9');");
    assert!(
        codes_of(&diagnostics).contains(&"argument-type".to_string()),
        "got {:?}",
        messages_of(&diagnostics)
    );
    assert!(
        messages_of(&diagnostics)
            .iter()
            .any(|message| message.contains("Argument 2 of `.extend()`")),
        "the receiver is argument one, so the engine and the server must agree \
         on the number: {:?}",
        messages_of(&diagnostics)
    );
}

#[test]
fn a_valid_method_call_is_never_flagged() {
    for source in [
        "RETURN { a: 9 }.extend({ b: 10 });",
        "RETURN 'abc'.len();",
        "RETURN [1, 2, 3].at(0);",
        "RETURN 'a,b'.split(',');",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.iter().any(|code| code.starts_with("argument-")),
            "{source} is valid, got {codes:?}"
        );
    }
}

#[test]
fn a_method_count_is_reported_excluding_the_receiver() {
    // `array::at(array, int)` — one argument is written, so two is too many.
    let diagnostics = diagnostics_for("RETURN [1, 2].at(0, 1);");
    assert!(
        codes_of(&diagnostics).contains(&"argument-count".to_string()),
        "got {:?}",
        messages_of(&diagnostics)
    );
    assert!(
        messages_of(&diagnostics)
            .iter()
            .any(|message| message.contains("`.at()` (`array::at`) expects 1 argument, found 2")),
        "the count is what the author writes: {:?}",
        messages_of(&diagnostics)
    );
}

#[test]
fn a_method_on_an_unknown_receiver_stays_silent() {
    // Nothing binds `$unknown`, so no table applies and nothing is reported.
    // This is the gate, and it is why a wrong receiver kind is worse than none:
    // `String` and the catch-all disagree about four arities.
    for source in [
        "RETURN $unknown.extend('9');",
        "DEFINE FUNCTION fn::f($x: any) { RETURN $x.extend('9'); };",
        "DEFINE FUNCTION fn::f($x: option<string>) { RETURN $x.len(); };",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.iter().any(|code| code.starts_with("argument-")),
            "{source} must stay silent, got {codes:?}"
        );
    }
}

#[test]
fn a_remapped_method_is_now_resolved_and_checked() {
    // This case used to sit in `a_method_on_an_unknown_receiver_stays_silent`.
    // `<number>.round()` is `math::round`, so the old `<receiver>::<method>`
    // guess looked for `number::round`, found nothing, and said nothing. The
    // generated receiver tables resolve it, so a bad call is now a diagnostic.
    let diagnostics = diagnostics_for("RETURN (1.5).round('nope');");
    let messages = messages_of(&diagnostics);
    assert!(
        !codes_of(&diagnostics).is_empty(),
        "a remapped method must now be checked: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|message| message.contains("math::round")),
        "the message must name the function it resolved to: {messages:?}"
    );
}

#[test]
fn a_method_further_along_a_path_is_not_attributed_to_the_first_link() {
    // The receiver of `extend` is `{ a: 9 }.b`, whose type is unknown. Reading
    // the type of `{ a: 9 }` instead would invent one and report against it.
    let codes = codes_of(&diagnostics_for("RETURN { a: 9 }.b.extend('9');"));
    assert!(
        !codes.iter().any(|code| code.starts_with("argument-")),
        "got {codes:?}"
    );
}

#[test]
fn the_reported_defect_is_fixed_an_int_where_a_string_belongs() {
    let diagnostics = diagnostics_for("RETURN string::len(42);");
    assert!(
        codes_of(&diagnostics).contains(&"argument-type".to_string()),
        "string::len takes a string, got {:?}",
        messages_of(&diagnostics)
    );
    assert!(
        messages_of(&diagnostics)
            .iter()
            .any(|message| message
                .contains("Argument 1 of `string::len` expects `string`, found `int`")),
        "got {:?}",
        messages_of(&diagnostics)
    );
}

#[test]
fn argument_types_are_checked_across_namespaces_the_curated_table_never_had() {
    for (source, function) in [
        ("RETURN math::clamp('a', 1, 10);", "math::clamp"),
        ("RETURN array::at('nope', 0);", "array::at"),
        ("RETURN time::day(true);", "time::day"),
        ("RETURN duration::days(true);", "duration::days"),
        ("RETURN object::keys(42);", "object::keys"),
    ] {
        let diagnostics = diagnostics_for(source);
        assert!(
            codes_of(&diagnostics).contains(&"argument-type".to_string()),
            "{function} must reject that argument, got {:?}",
            messages_of(&diagnostics)
        );
    }
}

#[test]
fn a_string_against_a_stringly_type_stays_silent() {
    // `assign.rs` treats `string → datetime|duration|uuid|bytes|regex|file` as a
    // runtime question, because a *specific* string may well coerce. That rule
    // predates this work and must keep holding for builtins.
    for source in [
        "RETURN time::day('2024-01-01T00:00:00Z');",
        "RETURN duration::days('1w');",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.contains(&"argument-type".to_string()),
            "{source} may coerce at runtime, got {codes:?}"
        );
    }
}

#[test]
fn a_widening_numeric_argument_is_accepted() {
    // `int` flows into `number`; only narrowing is a runtime question.
    for source in [
        "RETURN math::abs(3);",
        "RETURN math::abs(3.5);",
        "RETURN math::clamp(5, 1, 10);",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.contains(&"argument-type".to_string()),
            "{source} is valid, got {codes:?}"
        );
    }
}

#[test]
fn a_cast_parameter_accepts_a_string_pattern() {
    // `string::matches(String, Cast<Regex>)` — the engine casts, so a string
    // literal is legal even though the parameter is a regular expression.
    let codes = codes_of(&diagnostics_for("RETURN string::matches('abc', 'a.*');"));
    assert!(
        !codes.contains(&"argument-type".to_string()),
        "a cast parameter must stay permissive, got {codes:?}"
    );
}

#[test]
fn a_geometry_parameter_accepts_every_shape_a_geometry_arrives_in() {
    // A point tuple and an object literal are both geometries, but the lattice
    // types them `point` and `object`. SurrealDB's own `geo::` tests use both.
    for source in [
        "RETURN geo::distance((-0.12, 51.5), (-0.14, 51.6));",
        "RETURN geo::is_valid({ type: 'Point', coordinates: [-0.12, 51.5] });",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.contains(&"argument-type".to_string()),
            "{source} is valid, got {codes:?}"
        );
    }
}

#[test]
fn a_scalar_flowing_into_a_collection_parameter_is_not_reported() {
    // An aggregate supplies the whole group where the text names one field, so
    // this is valid inside `AS SELECT … GROUP BY …` and the checker cannot see
    // the difference.
    let codes = codes_of(&diagnostics_for("RETURN math::sum(<float> 1.5);"));
    assert!(
        !codes.contains(&"argument-type".to_string()),
        "a scalar against array<number> is a runtime question, got {codes:?}"
    );
    // The reverse stays an error.
    assert!(
        codes_of(&diagnostics_for("RETURN array::at([1, 2], [1, 2]);"))
            .contains(&"argument-type".to_string()),
        "an array where an int belongs is wrong in any context"
    );
}

#[test]
fn a_type_the_lattice_cannot_model_stays_silent() {
    // `type::field` returns `field` and `type::range` returns `range<record>`;
    // neither is modellable, and both must remain silent.
    for source in [
        "RETURN type::field('name');",
        "RETURN string::len(type::field('name'));",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.iter().any(|code| code.starts_with("argument-")),
            "{source} must stay silent, got {codes:?}"
        );
    }
}

#[test]
fn a_wrong_count_suppresses_the_type_report() {
    // Comparing positions is meaningless once the count is wrong, so exactly
    // one diagnostic comes back.
    let codes = codes_of(&diagnostics_for("RETURN string::len('a', 'b');"));
    assert_eq!(
        codes
            .iter()
            .filter(|code| code.starts_with("argument-"))
            .count(),
        1,
        "got {codes:?}"
    );
}

#[test]
fn a_variadic_types_every_argument_it_absorbs() {
    // `array::concat(Rest<Array>)` — each argument must be an array.
    let codes = codes_of(&diagnostics_for("RETURN array::concat([1], 'nope', [3]);"));
    assert!(
        codes.contains(&"argument-type".to_string()),
        "a variadic still types what it absorbs, got {codes:?}"
    );
}

#[test]
fn a_call_with_an_unparseable_argument_is_never_flagged() {
    // The pinned grammar cannot parse a closure or a signed decimal suffix, so
    // the argument list holds an `ERROR` node. That node might stand for one
    // argument or five, which makes the count meaningless — and both forms are
    // valid SurrealQL, so counting it reported a wrong arity on working code.
    for source in [
        "RETURN math::ceil(-102023.1dec);",
        "RETURN type::of(|| 'test');",
        "RETURN array::map([1], || 1);",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.iter().any(|code| code.starts_with("argument-")),
            "{source} has an unparseable argument, got {codes:?}"
        );
    }

    // Contrast, using a construct whose parse does not depend on the grammar
    // revision: the guard suppresses a call it *cannot read*, not every call.
    //
    // A closure would be the natural contrast here, and an earlier version of
    // this test used one — but whether `|$a, $b| $a + $b` parses depends on the
    // grammar revision. The pinned `826d0c2` accepts only a block body, while
    // later revisions add an expression body, so the assertion passed locally
    // and failed in continuous integration. Never assert a diagnostic on a
    // construct whose parse tree differs between grammar revisions.
    assert!(
        codes_of(&diagnostics_for("RETURN array::at([1, 2], 0, 3);"))
            .contains(&"argument-count".to_string()),
        "three arguments to a two-argument function is a real error"
    );
}

#[test]
fn a_middleware_registration_is_not_a_call() {
    // `MIDDLEWARE fn::x()` registers a function. The API runtime invokes it with
    // `(request, next)` supplied, so the written list is always shorter than the
    // declared parameters.
    let source = r#"
        DEFINE FUNCTION fn::mw($request: any, $next: any) { RETURN $next; };
        DEFINE CONFIG API MIDDLEWARE fn::mw();
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.iter().any(|code| code.starts_with("argument-")),
        "a middleware registration supplies no arguments, got {codes:?}"
    );
}

#[test]
fn a_parameter_whose_type_admits_none_may_be_omitted() {
    // SurrealDB substitutes `NONE` for a missing argument when the declared type
    // accepts it. Its own `custom_optional_args.surql` proves the distinction:
    // `fn::any_arg()` returns a value, `fn::one_arg()` is an error.
    let legal = r#"
        DEFINE FUNCTION fn::any_arg($a: any) { RETURN $a; };
        RETURN fn::any_arg();
    "#;
    assert!(
        !codes_of(&diagnostics_for(legal))
            .iter()
            .any(|code| code.starts_with("argument-")),
        "an `any` parameter may be omitted"
    );

    let illegal = r#"
        DEFINE FUNCTION fn::one_arg($a: bool) { RETURN $a; };
        RETURN fn::one_arg();
    "#;
    assert!(
        codes_of(&diagnostics_for(illegal)).contains(&"argument-count".to_string()),
        "a `bool` parameter may not be omitted"
    );
}

#[test]
fn a_user_function_still_takes_precedence_over_the_catalogue() {
    // `fn::` is a separate namespace, so this only proves the split did not
    // break the existing path.
    let source = r#"
        DEFINE FUNCTION fn::len($a: string, $b: string) -> int { RETURN 1; };
        RETURN fn::len('a');
    "#;
    let diagnostics = diagnostics_for(source);
    assert!(
        codes_of(&diagnostics).contains(&"argument-count".to_string()),
        "the user function expects two arguments, got {:?}",
        messages_of(&diagnostics)
    );
}

#[test]
fn correct_calls_produce_no_argument_diagnostics() {
    let source = r#"
        DEFINE FUNCTION fn::greet($name: string, $times: int) -> string {
            RETURN $name;
        };
        RETURN fn::greet('hi', 3);
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.iter().any(|c| c.starts_with("argument-")),
        "valid call flagged: {codes:?}"
    );
}

#[test]
fn uncertain_arguments_stay_silent() {
    // Every one of these is a construct the typer cannot pin down. All
    // must be silent rather than guessed at — a false positive here is
    // far worse than a missed error.
    let cases = [
        // A variable: no scope table yet.
        "RETURN fn::take($x);",
        // A method idiom.
        "RETURN fn::take($v.trim());",
        // A builtin whose declared return type is itself unmodellable
        // (`type::field(any) -> field`).
        "RETURN fn::take(type::field('x'));",
        // NONE into a non-optional slot is a runtime concern.
        "RETURN fn::take(NONE);",
        // A nested function call whose return type is undeclared.
        "RETURN fn::take(fn::other());",
    ];

    for case in cases {
        let source =
            format!("DEFINE FUNCTION fn::take($value: record<user>) {{ RETURN $value; }};\n{case}");
        let codes = codes_of(&diagnostics_for(&source));
        assert!(
            !codes.contains(&"argument-type".to_string()),
            "`{case}` should be silent, got {codes:?}"
        );
    }
}

#[test]
fn an_arithmetic_argument_is_now_typed_and_checked() {
    // `1 + 2` used to sit in `uncertain_arguments_stay_silent`, because
    // `BinaryExpression` had no arm and every operator expression was
    // `unknown`. It types as `int` now, and `int` into a `record<user>`
    // parameter is a mismatch SurrealDB rejects too — so the silence that test
    // pinned was a miss, not a policy.
    let source = "DEFINE FUNCTION fn::take($value: record<user>) { RETURN $value; };\n\
                  RETURN fn::take(1 + 2);";
    let messages = messages_of(&diagnostics_for(source));
    assert!(
        messages
            .iter()
            .any(|message| message.contains("expects `record<user>`, found `int`")),
        "got {messages:?}"
    );
}

#[test]
fn comments_between_arguments_are_not_counted_as_arguments() {
    // Comment/BlockComment are named `extras` in this grammar, so a naive
    // named-child count sees three arguments here.
    let source = r#"
        DEFINE FUNCTION fn::pair($a: string, $b: string) { RETURN $a; };
        RETURN fn::pair('x', /* note */ 'y');
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"argument-count".to_string()),
        "a comment is not an argument: {codes:?}"
    );
}

#[test]
fn adversarial_fixture_reports_no_argument_diagnostics() {
    // The strongest false-positive guard available: 22 typed functions
    // and ~50 call sites of production code that must stay clean.
    let source = include_str!("fixtures/adversarial.surql");
    let codes = codes_of(&diagnostics_for(source));
    let noisy: Vec<_> = codes
        .iter()
        .filter(|code| code.starts_with("argument-") || *code == "let-type")
        .collect();
    assert!(noisy.is_empty(), "false positives on real code: {noisy:?}");
}

#[test]
fn builtin_return_types_flow_into_argument_checking() {
    // `type::record(table, key) -> record` is in the builtin table as a
    // signature *string*; parsing its `-> T` tail is what makes this work.
    let source = r#"
        DEFINE FUNCTION fn::take($value: record<user>) { RETURN $value; };
        RETURN fn::take(type::record('user', 'beau'));
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"argument-type".to_string()),
        "a bare `record` fits `record<user>`: {codes:?}"
    );

    // …and the same return type is rejected where it genuinely cannot fit.
    let bad = r#"
        DEFINE FUNCTION fn::take($value: int) { RETURN $value; };
        RETURN fn::take(type::string(1));
    "#;
    let messages: Vec<String> = diagnostics_for(bad)
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `int`, found `string`")),
        "type::string(...) -> string should be caught: {messages:?}"
    );
}

#[test]
fn reports_each_bad_property_of_an_object_argument() {
    // The reported case: `{ line: 15102, asset: 2 }` against
    // `{ line: record<orderLine>, asset: record<asset> }`.
    let source = format!(
        "{DOC_ADD}\nLET $r = type::record(\"user\", \"beau\");\n\
         fn::doc::add($r, {{ line: 15102, asset: 2 }});"
    );
    let diagnostics = diagnostics_for(&source);
    let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();

    assert!(
        messages.contains(
            &"Argument 2 of `fn::doc::add`: property `line` expects `record<orderLine>`, found `int`."
        ),
        "got {messages:?}"
    );
    assert!(
        messages.contains(
            &"Argument 2 of `fn::doc::add`: property `asset` expects `record<asset>`, found `int`."
        ),
        "got {messages:?}"
    );

    // Each squiggle covers just that property's value.
    let line_error = diagnostics
        .iter()
        .find(|d| d.message.contains("property `line`"))
        .expect("line error");
    assert_eq!(
        line_error.range.end.character - line_error.range.start.character,
        5,
        "range should span exactly `15102`"
    );
}

#[test]
fn reports_a_missing_required_object_property() {
    let source = format!("{DOC_ADD}\nfn::doc::add(user:a, {{ line: orderLine:b }});");
    let messages: Vec<String> = diagnostics_for(&source)
        .iter()
        .map(|d| d.message.clone())
        .collect();

    assert!(
        messages.contains(
            &"Argument 2 of `fn::doc::add`: missing required property `asset`.".to_string()
        ),
        "got {messages:?}"
    );
}

#[test]
fn object_argument_with_correct_properties_is_silent() {
    let source =
        format!("{DOC_ADD}\nfn::doc::add(user:a, {{ line: orderLine:b, asset: asset:c }});");
    let codes = codes_of(&diagnostics_for(&source));
    assert!(
        !codes.contains(&"argument-type".to_string()),
        "a correct object literal was flagged: {codes:?}"
    );
}

#[test]
fn optional_object_properties_may_be_omitted() {
    let source = r#"
        DEFINE FUNCTION fn::save($opts: { name: string, note: option<string> }) {
            RETURN $opts;
        };
        RETURN fn::save({ name: 'x' });
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"argument-type".to_string()),
        "an option<T> property is not required: {codes:?}"
    );
}

#[test]
fn extra_object_properties_are_not_reported() {
    // Unconfirmed that SurrealQL seals object types, so this stays quiet.
    let source = r#"
        DEFINE FUNCTION fn::save($opts: { name: string }) { RETURN $opts; };
        RETURN fn::save({ name: 'x', extra: 1 });
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"argument-type".to_string()),
        "extra properties must stay silent: {codes:?}"
    );
}

// ---------------------------------------------------------------------------
// LET bindings and variable types
// ---------------------------------------------------------------------------

/// Hover markdown at the first occurrence of `needle` in `source`.
fn hover_at(source: &str, needle: &str) -> Option<String> {
    let analysis =
        analyze_document(uri("hover.surql"), source, SymbolOrigin::Local).expect("analysis");
    let workspace = workspace_from(vec![analysis.clone()]);
    let model = MergedSemanticModel::build(&workspace, &Default::default());

    let offset = source.find(needle).expect("needle present");
    let line = source[..offset].matches('\n').count() as u32;
    let column = (offset - source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;

    model.hover_markdown_at(&analysis, Position::new(line, column), needle, None)
}

#[test]
fn let_bound_variable_reports_its_inferred_type() {
    // `type::record`'s signature can only promise `-> record`, but the
    // call names the table, so the binding should carry `record<user>`.
    let source = "LET $r = type::record(\"user\", \"beau\");\nRETURN $r;";
    let hover = hover_at(source, "$r").expect("hover for $r");

    assert!(hover.contains("LET $r"), "got {hover}");
    assert!(hover.contains("Type: `record<user>`"), "got {hover}");
}

#[test]
fn record_constructors_are_narrowed_to_the_named_table() {
    for (value, expected) in [
        // Both argument orders the builtin accepts.
        ("type::record('user', 'beau')", "record<user>"),
        ("type::record(\"user\", \"beau\")", "record<user>"),
        ("type::thing('user', 'beau')", "record<user>"),
        // Single-argument form takes a whole record id.
        ("type::record('user:beau')", "record<user>"),
    ] {
        let source = format!("LET $v = {value};\nRETURN $v;");
        let hover = hover_at(&source, "$v").unwrap_or_default();
        assert!(
            hover.contains(&format!("Type: `{expected}`")),
            "`{value}` should be `{expected}`, got {hover}"
        );
    }
}

#[test]
fn record_constructors_stay_coarse_when_the_table_is_dynamic() {
    // The table is only knowable when it is a literal. Anything else must
    // fall back to the signature's bare `record`, which is assignable to
    // any `record<T>` and so cannot cause a false positive.
    for value in [
        "type::record($table, 'beau')",
        "type::record($id)",
        "type::record(fn::pick(), 'beau')",
    ] {
        let source = format!("LET $v = {value};\nRETURN $v;");
        let hover = hover_at(&source, "$v").unwrap_or_default();
        assert!(
            hover.contains("Type: `record`"),
            "`{value}` should stay a bare `record`, got {hover}"
        );
    }
}

#[test]
fn a_narrowed_record_is_checked_against_the_parameter_table() {
    // The payoff: this used to pass silently, because a bare `record` is
    // assignable to every `record<T>`.
    let source = r#"
        DEFINE FUNCTION fn::take($user: record<user>) { RETURN $user; };
        LET $c = type::record('company', 'acme');
        RETURN fn::take($c);
    "#;
    let messages: Vec<String> = diagnostics_for(source)
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `record<user>`, found `record<company>`")),
        "a mismatched table should now be caught: {messages:?}"
    );
}

#[test]
fn a_narrowed_record_matching_the_parameter_is_silent() {
    let source = r#"
        DEFINE FUNCTION fn::take($user: record<user>) { RETURN $user; };
        LET $u = type::record('user', 'beau');
        RETURN fn::take($u);
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"argument-type".to_string()),
        "record<user> into record<user> must be silent: {codes:?}"
    );
}

#[test]
fn let_bound_variable_types_from_literals() {
    for (value, expected) in [
        ("'hello'", "string"),
        ("42", "int"),
        ("1.5", "float"),
        ("true", "bool"),
        ("user:beau", "record<user>"),
        ("{ a: 1 }", "{ a: int }"),
    ] {
        let source = format!("LET $v = {value};\nRETURN $v;");
        let hover = hover_at(&source, "$v").unwrap_or_default();
        assert!(
            hover.contains(&format!("Type: `{expected}`")),
            "`{value}` should be `{expected}`, got {hover}"
        );
    }
}

#[test]
fn a_typed_variable_satisfies_a_matching_parameter() {
    let source = r#"
        DEFINE FUNCTION fn::take($value: record<user>) { RETURN $value; };
        LET $r = type::record('user', 'beau');
        RETURN fn::take($r);
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"argument-type".to_string()),
        "a bare `record` fits `record<user>`: {codes:?}"
    );
}

#[test]
fn a_typed_variable_is_checked_against_a_parameter() {
    let source = r#"
        DEFINE FUNCTION fn::take($value: record<user>) { RETURN $value; };
        LET $name = 'beau';
        RETURN fn::take($name);
    "#;
    let messages: Vec<String> = diagnostics_for(source)
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `record<user>`, found `string`")),
        "a string-typed variable should be caught: {messages:?}"
    );
}

#[test]
fn later_bindings_shadow_earlier_ones() {
    let source = "LET $x = 1;\nLET $x = 'later';\nRETURN $x;";
    let analysis =
        analyze_document(uri("shadow.surql"), source, SymbolOrigin::Local).expect("analysis");
    let workspace = workspace_from(vec![analysis.clone()]);
    let model = MergedSemanticModel::build(&workspace, &Default::default());

    // On the `RETURN` line, the second binding wins.
    let hover = model
        .hover_markdown_at(&analysis, Position::new(2, 8), "$x", None)
        .expect("hover");
    assert!(hover.contains("Type: `string`"), "got {hover}");
}

#[test]
fn a_binding_is_not_visible_before_its_declaration() {
    let source = "RETURN $later;\nLET $later = 1;";
    let hover = hover_at(source, "$later");
    assert!(
        hover.is_none(),
        "a variable used before LET must not resolve, got {hover:?}"
    );
}

#[test]
fn block_scoped_bindings_do_not_leak() {
    let source = r#"
        DEFINE FUNCTION fn::wrap() {
            LET $inner = 'hidden';
            RETURN $inner;
        };
        RETURN $inner;
    "#;
    let analysis =
        analyze_document(uri("scope.surql"), source, SymbolOrigin::Local).expect("analysis");
    let workspace = workspace_from(vec![analysis.clone()]);
    let model = MergedSemanticModel::build(&workspace, &Default::default());

    // The trailing `RETURN $inner;` is outside the function body.
    let outer = source.rfind("$inner").expect("outer use");
    let line = source[..outer].matches('\n').count() as u32;
    let column = (outer - source[..outer].rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;
    assert!(
        model
            .hover_markdown_at(&analysis, Position::new(line, column), "$inner", None)
            .is_none(),
        "a LET inside a function body must not escape it"
    );
}

#[test]
fn function_parameters_are_bound_inside_the_body() {
    let source = r#"
        DEFINE FUNCTION fn::greet($name: string) { RETURN $name; };
    "#;
    let hover = hover_at(source, "$name").unwrap_or_default();
    // The first `$name` is the declaration site itself, which sits outside
    // the body span; the binding is what the body sees.
    let body_use = source.rfind("$name").expect("body use");
    let analysis =
        analyze_document(uri("params.surql"), source, SymbolOrigin::Local).expect("analysis");
    let workspace = workspace_from(vec![analysis.clone()]);
    let model = MergedSemanticModel::build(&workspace, &Default::default());
    let line = source[..body_use].matches('\n').count() as u32;
    let column = (body_use - source[..body_use].rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;

    let inside = model
        .hover_markdown_at(&analysis, Position::new(line, column), "$name", None)
        .expect("hover inside body");
    assert!(inside.contains("Type: `string`"), "got {inside} / {hover}");
}

#[test]
fn declared_let_type_is_checked_against_the_value() {
    let messages: Vec<String> = diagnostics_for("LET $n: int = 'not a number';")
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        messages.contains(&"`$n` is declared `int` but the value is `string`.".to_string()),
        "got {messages:?}"
    );
}

#[test]
fn declared_let_type_is_silent_when_it_matches() {
    let codes = codes_of(&diagnostics_for("LET $n: int = 42;"));
    assert!(!codes.contains(&"let-type".to_string()), "{codes:?}");
}

#[test]
fn unresolvable_initializers_leave_the_variable_untyped() {
    // Must not guess. Each of these stays `Unknown`, so downstream uses
    // of the variable stay silent too.
    for value in ["$other", "$a.b", "$v.trim()"] {
        let source = format!(
            "DEFINE FUNCTION fn::take($value: record<user>) {{ RETURN $value; }};\n\
             LET $v = {value};\nRETURN fn::take($v);"
        );
        let codes = codes_of(&diagnostics_for(&source));
        assert!(
            !codes.contains(&"argument-type".to_string()),
            "`{value}` should leave $v untyped, got {codes:?}"
        );
    }
}

#[test]
fn in_scope_variables_are_offered_for_completion() {
    let source = "LET $total = 42;\nLET $name = 'x';\nRETURN ";
    let analysis =
        analyze_document(uri("complete.surql"), source, SymbolOrigin::Local).expect("analysis");
    let workspace = workspace_from(vec![analysis.clone()]);
    let model = MergedSemanticModel::build(&workspace, &Default::default());

    let items = model.variable_completion_items(&analysis, Position::new(2, 7), "");
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();

    assert!(labels.contains(&"$total"), "got {labels:?}");
    assert!(labels.contains(&"$name"), "got {labels:?}");
    let total = items.iter().find(|i| i.label == "$total").expect("$total");
    assert_eq!(total.detail.as_deref(), Some("int"));
}

#[test]
fn out_of_scope_variables_are_not_offered() {
    let source = "DEFINE FUNCTION fn::wrap() { LET $hidden = 1; RETURN $hidden; };\nRETURN ";
    let analysis =
        analyze_document(uri("complete.surql"), source, SymbolOrigin::Local).expect("analysis");
    let workspace = workspace_from(vec![analysis.clone()]);
    let model = MergedSemanticModel::build(&workspace, &Default::default());

    let items = model.variable_completion_items(&analysis, Position::new(1, 7), "");
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        !labels.contains(&"$hidden"),
        "a function-local binding leaked: {labels:?}"
    );
}

// ---------------------------------------------------------------------------
// Parameter types at the hover / signature-help surfaces
// ---------------------------------------------------------------------------
//
// These assert the rendered strings a user actually reads. Parameter types
// were previously only covered *indirectly*, through diagnostic message
// text — so a regression in `function_signature` could have gone unnoticed
// while every diagnostic test stayed green.

#[test]
fn function_hover_renders_full_generic_parameter_types() {
    let source = r#"DEFINE FUNCTION fn::doc::add($user: record<user>, $doc: {
  line: record<orderLine>,
  asset: record<asset>
}) -> record<orderDoc> {
  RETURN $doc;
};"#;
    let analysis =
        analyze_document(uri("sig.surql"), source, SymbolOrigin::Local).expect("analysis");
    let workspace = workspace_from(vec![analysis.clone()]);
    let model = MergedSemanticModel::build(&workspace, &Default::default());

    let hover = model
        .hover_markdown_for_token("fn::doc::add", None)
        .expect("hover for the function");

    // The generic argument must survive all the way to the rendered string.
    assert!(
        hover.contains("$user: record<user>"),
        "expected `$user: record<user>`, got:\n{hover}"
    );
    assert!(
        hover.contains("$doc: { line: record<orderLine>, asset: record<asset> }"),
        "expected the full inline object type, got:\n{hover}"
    );
    assert!(
        hover.contains("-> record<orderDoc>"),
        "expected the generic return type, got:\n{hover}"
    );
    // Guard against the specific regression reported: a bare `record`.
    assert!(
        !hover.contains("$user: record,") && !hover.contains("$user: record "),
        "parameter type was flattened to a bare `record`:\n{hover}"
    );
}

#[test]
fn function_hover_keeps_other_generic_type_shapes() {
    let source = r#"DEFINE FUNCTION fn::mix(
  $ids: record<orderData | project>,
  $piles: array<record<orderLine>>,
  $note: option<string>,
  $pair: [string, string]
) { RETURN $ids; };"#;
    let analysis =
        analyze_document(uri("sig2.surql"), source, SymbolOrigin::Local).expect("analysis");
    let workspace = workspace_from(vec![analysis.clone()]);
    let model = MergedSemanticModel::build(&workspace, &Default::default());

    let hover = model
        .hover_markdown_for_token("fn::mix", None)
        .expect("hover");

    for expected in [
        "$ids: record<orderData | project>",
        "$piles: array<record<orderLine>>",
        "$note: option<string>",
        "$pair: [string, string]",
    ] {
        assert!(
            hover.contains(expected),
            "missing `{expected}` in:\n{hover}"
        );
    }
}

#[test]
fn signature_help_labels_match_the_hover_signature() {
    // `signature_help` and function hover used to format parameters with
    // two separate copies of the same code. They now share
    // `param_label` / `function_signature`, so this pins the shared shape
    // and the generics that flow through it.
    let source = r#"DEFINE FUNCTION fn::doc::add($user: record<user>, $doc: {
  line: record<orderLine>,
  asset: record<asset>
}) { RETURN $doc; };"#;
    let analysis =
        analyze_document(uri("sighelp.surql"), source, SymbolOrigin::Local).expect("analysis");
    let workspace = workspace_from(vec![analysis.clone()]);
    let model = MergedSemanticModel::build(&workspace, &Default::default());
    let function = model
        .functions
        .get("fn::doc::add")
        .expect("indexed function");

    let labels: Vec<String> = function.params.iter().map(param_label).collect();
    assert_eq!(
        labels,
        vec![
            "$user: record<user>".to_string(),
            "$doc: { line: record<orderLine>, asset: record<asset> }".to_string(),
        ]
    );

    // The overall signature is built from exactly those labels.
    let signature = function_signature(function);
    for label in &labels {
        assert!(
            signature.contains(label),
            "signature `{signature}` is missing `{label}`"
        );
    }
}

#[test]
fn unannotated_parameters_render_without_a_type() {
    let source = "DEFINE FUNCTION fn::loose($a, $b) { RETURN $a; };";
    let analysis =
        analyze_document(uri("loose.surql"), source, SymbolOrigin::Local).expect("analysis");
    let workspace = workspace_from(vec![analysis.clone()]);
    let model = MergedSemanticModel::build(&workspace, &Default::default());
    let function = model.functions.get("fn::loose").expect("indexed");

    assert_eq!(function_signature(function), "fn::loose($a, $b)");
}

#[test]
fn build_version_identifies_the_binary() {
    // Exists so "is the editor running the binary I just built?" is a
    // question you can answer by reading `serverInfo.version`, rather than
    // by deducing it from behaviour.
    let version = surrealql_language_server::core::server::build_version();

    assert!(
        version.starts_with(env!("CARGO_PKG_VERSION")),
        "should lead with the crate version, got {version}"
    );
    assert!(
        version.contains("grammar "),
        "should name the grammar revision, got {version}"
    );
    // The bare crate version alone is what made this ambiguous.
    assert_ne!(version, env!("CARGO_PKG_VERSION"));
}

// ---------------------------------------------------------------------------
// Undefined variable references
// ---------------------------------------------------------------------------

#[test]
fn reports_an_undefined_variable_with_a_suggestion() {
    // The reported case: a typo'd `$f` inside an object literal. SurrealDB
    // substitutes NONE for an unset param rather than failing, so nothing
    // at runtime would tell you either.
    let source = r#"
        LET $r = user:beau;
        LET $x = orderLine:a;
        LET $f = file:b;
        RETURN { line: $x, asset: $fx };
    "#;
    let diagnostics = diagnostics_for(source);
    let undefined: Vec<&str> = diagnostics
        .iter()
        .filter(|d| {
            matches!(&d.code,
            Some(tower_lsp_server::ls_types::NumberOrString::String(c))
                if c == "undefined-variable")
        })
        .map(|d| d.message.as_str())
        .collect();

    assert_eq!(
        undefined,
        vec!["`$fx` is not defined. Did you mean `$f`?"],
        "all of {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let error = diagnostics
        .iter()
        .find(|d| d.message.starts_with("`$fx`"))
        .expect("the error");
    assert_eq!(
        error.severity,
        Some(tower_lsp_server::ls_types::DiagnosticSeverity::ERROR)
    );
    // Squiggle covers exactly `$fx`.
    assert_eq!(error.range.end.character - error.range.start.character, 3);
}

#[test]
fn every_binding_form_counts_as_defined() {
    let source = r#"
        DEFINE PARAM $page_size VALUE 20;
        DEFINE FUNCTION fn::use($param: int) {
            LET $local = 1;
            FOR $item IN [1, 2] {
                RETURN $item + $local + $param + $page_size;
            };
            RETURN $local;
        };
        LET $top = 1;
        RETURN $top + $page_size;
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"undefined-variable".to_string()),
        "a legitimate binding was flagged: {codes:?}"
    );
}

#[test]
fn special_variables_are_never_undefined() {
    let source = r#"
        DEFINE FIELD name ON person TYPE string VALUE $value;
        DEFINE EVENT audit ON TABLE person WHEN $event = 'UPDATE' THEN {
            LET $id = ($after OR $before).id;
            RETURN $id;
        };
        RETURN [$auth, $this, $session, $token, $request, $parent, $input];
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"undefined-variable".to_string()),
        "a language-provided variable was flagged: {codes:?}"
    );
}

#[test]
fn bindings_inside_a_then_clause_block_are_visible() {
    // `DEFINE EVENT … THEN { … }` nests its block in a `ThenClause`, so it
    // is not a direct `Block` child of the DEFINE. Walking only direct
    // children left every `LET` in there unbound.
    let source = r#"
        DEFINE EVENT lineUpdate ON TABLE orderLine WHEN $event != 'DELETE' THEN {
            IF $event != 'DELETE' {
                LET $bef = $before.id;
                LET $aft = $after.id;
                IF $bef != $aft {
                    LET $who = $after.updatedBy;
                    RETURN [$bef, $aft, $who];
                };
            };
        };
    "#;
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"undefined-variable".to_string()),
        "a LET nested in a THEN block was flagged: {codes:?}"
    );
}

#[test]
fn out_of_scope_uses_are_reported() {
    let source = r#"
        DEFINE FUNCTION fn::wrap() { LET $inner = 1; RETURN $inner; };
        RETURN $inner;
    "#;
    let messages: Vec<String> = diagnostics_for(source)
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        messages.contains(&"`$inner` is not defined.".to_string()),
        "a function-local binding must not be visible outside: {messages:?}"
    );
}

#[test]
fn caller_bound_variables_can_be_declared_in_config() {
    let source = "SELECT * FROM person WHERE id = $wanted;";
    let analysis =
        analyze_document(uri("bind.surql"), source, SymbolOrigin::Local).expect("analysis");
    let workspace = workspace_from(vec![analysis.clone()]);
    let model = MergedSemanticModel::build(&workspace, &Default::default());

    // Undeclared: reported, since nothing in the file binds it.
    let strict = model.semantic_diagnostics(&analysis, &ServerSettings::default());
    assert!(
        strict.iter().any(|d| d.message.contains("`$wanted`")),
        "expected a report by default"
    );

    // Declared as caller-bound: silent.
    let mut settings = ServerSettings::default();
    settings.analysis.external_params = vec!["wanted".to_string()];
    let relaxed = model.semantic_diagnostics(&analysis, &settings);
    assert!(
        !relaxed.iter().any(|d| d.message.contains("`$wanted`")),
        "externalParams should suppress it, got {:?}",
        relaxed.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn syntax_errors_do_not_produce_invented_variable_errors() {
    // A `LET` the parser choked on binds nothing, so every later use would
    // look undefined. The syntax error is reported on its own; piling name
    // errors on top of it is noise.
    let source = "LET $ok = @@@ broken @@@;\nRETURN $ok;";
    let analysis =
        analyze_document(uri("broken.surql"), source, SymbolOrigin::Local).expect("analysis");
    assert!(
        !analysis.syntax_diagnostics.is_empty(),
        "fixture should actually fail to parse"
    );
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"undefined-variable".to_string()),
        "should stay quiet inside a broken parse: {codes:?}"
    );
}

#[test]
fn adversarial_fixture_reports_no_undefined_variables() {
    let source = include_str!("fixtures/adversarial.surql");
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"undefined-variable".to_string()),
        "false positives on real code: {codes:?}"
    );
}

#[test]
fn expression_bodied_closure_parameters_are_bound() {
    // A closure body can be a bare expression with no `Block`, so scoping
    // its parameters to a block child binds nothing.
    let source = "LET $xs = ['a', 'b'];\nRETURN $xs.filter(|$item| $item != 'a');";
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"undefined-variable".to_string()),
        "closure parameter was flagged: {codes:?}"
    );
}

// ---------------------------------------------------------------------------
// Inferred function return types
// ---------------------------------------------------------------------------
//
// A `DEFINE FUNCTION` with no `-> T` used to make every call site `unknown`.
// The return type is now read from the body. These tests come in pairs: one for
// what the inference now knows, one pinning what it deliberately refuses to
// guess. The refusals matter more — an inferred type feeds the argument check,
// so a type narrower than the truth reports against code that works.

/// Hover markdown at the first occurrence of `needle`, across a workspace of
/// `(path, source)` documents. Hover is taken in the *last* document.
///
/// The single-document `hover_at` cannot express the case this feature exists
/// for: a `fn::` definition in one file and the call in another.
fn hover_across(documents: &[(&str, &str)], needle: &str) -> Option<String> {
    let analyses: Vec<DocumentAnalysis> = documents
        .iter()
        .map(|(path, source)| {
            analyze_document(uri(path), source, SymbolOrigin::Local).expect("analysis")
        })
        .collect();
    let workspace = workspace_from(analyses.clone());
    let model = MergedSemanticModel::build(&workspace, &Default::default());

    let target = analyses.last().expect("a document");
    let source = &target.text;
    let offset = source.find(needle).expect("needle present");
    let line = source[..offset].matches('\n').count() as u32;
    let column = (offset - source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;
    model.hover_markdown_at(target, Position::new(line, column), needle, None)
}

/// The type hover reports for `needle`, as a bare string.
fn inferred_type_of(source: &str, needle: &str) -> String {
    let hover = hover_at(source, needle).unwrap_or_default();
    hover
        .lines()
        .find_map(|line| line.trim().strip_prefix("- Type: "))
        .unwrap_or("<no type line>")
        .trim_matches('`')
        .to_string()
}

#[test]
fn a_body_with_one_return_gives_the_call_site_its_type() {
    // The reported case. `string::slug` returns a string, so the function does
    // too, and nothing had to be written to say so.
    let source = "DEFINE FUNCTION fn::custom::slug($input: string) {\n\
                  RETURN string::slug($input);\n\
                  };\n\
                  LET $slug = fn::custom::slug('some random string');\n\
                  RETURN $slug;";
    assert_eq!(inferred_type_of(source, "$slug"), "string");
}

#[test]
fn a_trailing_expression_body_gives_the_call_site_its_type() {
    // A body with no `RETURN` yields its last expression.
    let source = "DEFINE FUNCTION fn::one() { 1 };\nLET $n = fn::one();";
    assert_eq!(inferred_type_of(source, "$n"), "int");
}

#[test]
fn a_declared_return_type_still_wins_over_the_body() {
    // The annotation is what the author promised and what the engine coerces
    // to. Never override it, even when the body disagrees.
    let source = "DEFINE FUNCTION fn::f() -> any { RETURN 1; };\nLET $v = fn::f();";
    assert_eq!(inferred_type_of(source, "$v"), "any");
}

#[test]
fn a_throwing_tail_does_not_block_inference() {
    // `THROW` always raises, so no value passes through it. Without this the
    // validate-or-throw shape could never be inferred.
    let source = "DEFINE FUNCTION fn::must($ok: bool) {\n\
                  IF $ok { RETURN 1; };\n\
                  THROW 'not ok';\n\
                  };\n\
                  LET $n = fn::must(true);";
    assert_eq!(inferred_type_of(source, "$n"), "int");
}

#[test]
fn inference_resolves_a_chain_across_rounds() {
    // `fn::outer` can only be typed once `fn::inner` is, which takes a second
    // round. A one-pass implementation, or one that cached the first round's
    // failure, leaves this `unknown`.
    let source = "DEFINE FUNCTION fn::inner() { RETURN 1; };\n\
                  DEFINE FUNCTION fn::outer() { RETURN fn::inner(); };\n\
                  LET $n = fn::outer();";
    assert_eq!(inferred_type_of(source, "$n"), "int");
}

#[test]
fn inference_crosses_documents() {
    // The whole reason this runs in `MergedSemanticModel::build` rather than
    // per document: a `fn::` definition routinely lives in another file.
    let hover = hover_across(
        &[
            (
                "functions.surql",
                "DEFINE FUNCTION fn::custom::slug($input: string) { RETURN string::slug($input); };",
            ),
            ("query.surql", "LET $slug = fn::custom::slug('x');\nRETURN $slug;"),
        ],
        "$slug",
    )
    .expect("hover for $slug");
    assert!(hover.contains("Type: `string`"), "got {hover}");
}

#[test]
fn divergent_return_paths_infer_a_union() {
    let source = "DEFINE FUNCTION fn::f($c: bool) { IF $c { RETURN 'a' }; RETURN 1; };\n\
                  LET $v = fn::f(true);";
    assert_eq!(inferred_type_of(source, "$v"), "string | int");
}

#[test]
fn repeated_return_types_are_not_repeated_in_the_union() {
    // `TypeExpr::union` does not deduplicate, so without a guard the commonest
    // multi-return shape hovers as `string | string`.
    let source = "DEFINE FUNCTION fn::label($n: int) {\n\
                  IF $n > 0 { RETURN 'pos'; };\n\
                  RETURN 'neg';\n\
                  };\n\
                  LET $v = fn::label(1);";
    assert_eq!(inferred_type_of(source, "$v"), "string");
}

#[test]
fn a_union_of_inferred_returns_reports_nothing() {
    // A union on the value side of `assignable` can never come back
    // `Incompatible`, so a divergent body is informative in hover and silent in
    // the checker. This pins that property — the union case is only safe
    // because of it.
    let source = "DEFINE FUNCTION fn::mixed($c: bool) { IF $c { RETURN 's' }; RETURN 0; };\n\
                  DEFINE FUNCTION fn::take($n: int) -> int { RETURN $n; };\n\
                  LET $x: string = fn::mixed(true);\n\
                  RETURN fn::take(fn::mixed(false));";
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"argument-type".to_string()) && !codes.contains(&"let-type".to_string()),
        "a union must stay silent: {codes:?}"
    );
}

// --- What the inference refuses to guess -----------------------------------

#[test]
fn an_unresolvable_body_leaves_the_call_untyped() {
    // Every one of these must stay `unknown`. Each comment is the reason.
    for body in [
        // No statement kind has a type yet.
        "{ SELECT * FROM person }",
        "{ CREATE person CONTENT {} }",
        // The tail is the `IF`, whose own type is unknown.
        "{ IF $c { RETURN 1 } ELSE { RETURN 2 }; }",
        // `THROW` alone never produces a value.
        "{ THROW 'no' }",
        // `BinaryExpression` has no arm.
        "{ RETURN $c + 1; }",
        // Field access has no arm.
        "{ RETURN $c.field; }",
        // `IF … THEN … END` hides its returns from the walk, so refuse.
        "{ IF $c THEN RETURN 1 END; RETURN 'a'; }",
        // An empty body contributes nothing.
        "{ }",
    ] {
        let source =
            format!("DEFINE FUNCTION fn::f($c: any) {body};\nLET $v = fn::f(1);\nRETURN $v;");
        assert_eq!(
            inferred_type_of(&source, "$v"),
            "unknown",
            "`{body}` must not be inferred"
        );
        // Only the type checks are this feature's business. `CREATE person`
        // also draws an `unknown-table` warning, which is unrelated and
        // pre-existing.
        let noisy: Vec<String> = codes_of(&diagnostics_for(&source))
            .into_iter()
            .filter(|code| {
                code.starts_with("argument-") || code == "let-type" || code == "return-type"
            })
            .collect();
        assert!(noisy.is_empty(), "`{body}` must stay silent: {noisy:?}");
    }
}

#[test]
fn a_missing_return_path_prevents_inference() {
    // `fn::maybe` yields NONE when the branch does not fire, so `string` would
    // be a lie — and the narrow answer is the one that fires a diagnostic. The
    // trailing-statement contribution is what keeps this honest.
    let source = "DEFINE FUNCTION fn::maybe($n: int) { IF $n > 0 { RETURN 'positive'; }; };\n\
                  LET $v = fn::maybe(1);";
    assert_eq!(inferred_type_of(source, "$v"), "unknown");
}

#[test]
fn inference_does_not_see_document_level_lets() {
    // A function body sees only its own parameters. `$greeting` is unset inside
    // the body and the engine yields NONE, so resolving it against the whole
    // document would infer a type the function cannot produce.
    let source = "LET $greeting = 'hello';\n\
                  DEFINE FUNCTION fn::f() { RETURN $greeting; };\n\
                  LET $v = fn::f();";
    assert_eq!(inferred_type_of(source, "$v"), "unknown");
}

#[test]
fn a_recursive_function_without_an_annotation_stays_unknown() {
    // Also the termination test: nothing here may loop or recurse.
    let source = "DEFINE FUNCTION fn::fib($n: int) {\n\
                  IF $n < 2 { RETURN $n; };\n\
                  RETURN fn::fib($n - 1) + fn::fib($n - 2);\n\
                  };\n\
                  LET $v = fn::fib(10);";
    assert_eq!(inferred_type_of(source, "$v"), "unknown");
}

#[test]
fn mutually_recursive_functions_stay_unknown() {
    let source = "DEFINE FUNCTION fn::a() { RETURN fn::b(); };\n\
                  DEFINE FUNCTION fn::b() { RETURN fn::a(); };\n\
                  LET $v = fn::a();";
    assert_eq!(inferred_type_of(source, "$v"), "unknown");
}

#[test]
fn a_javascript_body_is_not_inferred() {
    let source = "DEFINE FUNCTION fn::js() { function() { return 1; } };\n\
                  LET $v = fn::js();";
    assert_eq!(inferred_type_of(source, "$v"), "unknown");
}

#[test]
fn a_broken_return_annotation_is_not_replaced_by_an_inference() {
    // `function_return_type` answers `None` for a half-typed `-> ` exactly as
    // it does for an absent one. Substituting a guess for the annotation the
    // author is mid-way through writing would be the wrong move.
    let source = "DEFINE FUNCTION fn::f() -> { RETURN 1; };\nLET $v = fn::f();";
    assert_eq!(inferred_type_of(source, "$v"), "unknown");
}

// --- Where the inference becomes visible -----------------------------------

#[test]
fn an_inferred_return_type_reaches_the_argument_check() {
    let source = "DEFINE FUNCTION fn::give() { RETURN 'x'; };\n\
                  DEFINE FUNCTION fn::take($n: int) -> int { RETURN $n; };\n\
                  RETURN fn::take(fn::give());";
    let messages: Vec<String> = diagnostics_for(source)
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `int`, found `string`")),
        "got {messages:?}"
    );
}

#[test]
fn a_matching_inferred_return_type_reaches_nothing() {
    let source = "DEFINE FUNCTION fn::give() { RETURN 'x'; };\n\
                  DEFINE FUNCTION fn::take($s: string) -> string { RETURN $s; };\n\
                  RETURN fn::take(fn::give());";
    let codes = codes_of(&diagnostics_for(source));
    assert!(
        !codes.contains(&"argument-type".to_string()),
        "got {codes:?}"
    );
}

#[test]
fn an_inferred_return_type_reaches_the_declared_return_check() {
    // The new capability in the other direction: a wrapper that declares a
    // type its unannotated callee cannot satisfy.
    let source = "DEFINE FUNCTION fn::n() { RETURN 1; };\n\
                  DEFINE FUNCTION fn::g() -> string { RETURN fn::n(); };";
    let codes = codes_of(&diagnostics_for(source));
    assert!(codes.contains(&"return-type".to_string()), "got {codes:?}");
}

#[test]
fn function_hover_marks_an_inferred_return_type() {
    let source =
        "DEFINE FUNCTION fn::custom::slug($input: string) { RETURN string::slug($input); };";
    let hover = hover_at(source, "fn::custom::slug").expect("hover");
    assert!(hover.contains("-> string"), "got {hover}");
    assert!(
        hover.contains("Return type inferred from the body."),
        "an inferred arrow must say so: {hover}"
    );
}

#[test]
fn function_hover_does_not_claim_a_declared_type_was_inferred() {
    let source = "DEFINE FUNCTION fn::x() -> int { RETURN 1; };";
    let hover = hover_at(source, "fn::x").expect("hover");
    assert!(hover.contains("-> int"), "got {hover}");
    assert!(
        !hover.contains("inferred from the body"),
        "a declared type is not an inference: {hover}"
    );
}

#[test]
fn repeated_builds_infer_the_same_type() {
    // `model.functions` is a `HashMap`, so candidate order varies. A round is
    // computed in full before it is written precisely so the answer cannot
    // depend on that order.
    let source = "DEFINE FUNCTION fn::a() { RETURN 'x'; };\n\
                  DEFINE FUNCTION fn::b() { RETURN fn::a(); };\n\
                  DEFINE FUNCTION fn::c() { RETURN fn::b(); };\n\
                  LET $v = fn::c();";
    let first = inferred_type_of(source, "$v");
    assert_eq!(first, "string");
    for _ in 0..20 {
        assert_eq!(inferred_type_of(source, "$v"), first, "unstable inference");
    }
}

// ---------------------------------------------------------------------------
// Arithmetic operand types
// ---------------------------------------------------------------------------
//
// SurrealDB rejects `"" + "222" + 3` at run time, and the server used to say
// nothing: `BinaryExpression` had no arm in `infer_expr_type`. The operand rules
// now come from the engine's own tables (`semantic::operate`), which are
// irregular per operator — `+` concatenates two strings but rejects a string and
// an int, `*` scales a duration in one direction only, and `/` never fails.
//
// Half of these tests pin what stays silent. That half matters more: an operand
// pair the checker misjudges squiggles a query that runs.

/// The `operator-type` messages `source` produces.
fn operator_messages(source: &str) -> Vec<String> {
    diagnostics_for(source)
        .into_iter()
        .filter(|diagnostic| {
            matches!(
                &diagnostic.code,
                Some(tower_lsp_server::ls_types::NumberOrString::String(code))
                    if code == "operator-type"
            )
        })
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn the_reported_addition_is_caught() {
    // `"" + "222"` concatenates to a string, and `string + int` is the pair
    // SurrealDB rejects.
    let messages = operator_messages(r#"RETURN "" + "222" + 3;"#);
    assert_eq!(
        messages,
        vec!["Cannot perform addition with `string` and `int`.".to_string()],
        "one diagnostic, naming both operand types"
    );
}

#[test]
fn casting_either_side_silences_the_reported_addition() {
    // Both corrections from the report. These are the whole point: a cast must
    // not merely avoid a wrong answer, it must produce the right one.
    for source in [
        r#"RETURN "" + "222" + <string>3;"#,
        r#"RETURN <int>"0" + <int>"222" + 3;"#,
    ] {
        assert!(
            operator_messages(source).is_empty(),
            "`{source}` is valid SurrealQL"
        );
    }
}

#[test]
fn a_cast_gives_the_expression_its_type() {
    // Proves the cast is *typed*, not just tolerated.
    assert_eq!(
        inferred_type_of(r#"LET $v = "" + "222" + <string>3;"#, "$v"),
        "string"
    );
    assert_eq!(
        inferred_type_of(r#"LET $v = <int>"0" + <int>"222" + 3;"#, "$v"),
        "int"
    );
}

#[test]
fn each_rejected_arm_of_the_engine_tables_is_caught() {
    // One case per shape the engine's `TryAdd`/`TrySub`/`TryMul`/`TryPow` impls
    // send to their catch-all. The first four are pinned by SurrealDB's own
    // corpus files.
    for (source, expected) in [
        ("RETURN [1,2,3] - 1;", "Cannot perform subtraction"),
        ("RETURN {1,} + 1;", "Cannot perform addition"),
        ("RETURN 1s * 1s;", "Cannot perform multiplication"),
        ("RETURN 1s ** 1s;", "Cannot raise the value"),
        // Multiplication is one-directional in the engine.
        ("RETURN 2 * 1s;", "Cannot perform multiplication"),
        // A concrete kind that appears in no arm at all.
        ("RETURN true + 1;", "Cannot perform addition"),
        ("RETURN 1s - 1;", "Cannot perform subtraction"),
    ] {
        let messages = operator_messages(source);
        assert!(
            messages.iter().any(|message| message.starts_with(expected)),
            "`{source}` should report `{expected}`, got {messages:?}"
        );
    }
}

#[test]
fn every_accepted_arm_of_the_engine_tables_is_silent() {
    // The other half of the same tables. Each of these runs.
    for source in [
        // Collections combine in all four array/set pairings.
        "RETURN [1,2] + [3,4];",
        "RETURN {1,2} + [3];",
        "RETURN [1,2] + {3,};",
        "RETURN [1,2,3] - [1];",
        // Numeric promotion across the whole chain.
        "RETURN 8 + 3dec;",
        "RETURN 8 + 3.5;",
        "RETURN 2 ** 8;",
        // Durations and datetimes.
        "RETURN 1s + 1s;",
        "RETURN 1s * 2;",
        r#"RETURN d"2024-01-01T00:00:00Z" + 1h;"#,
        r#"RETURN d"2024-01-02T00:00:00Z" - d"2024-01-01T00:00:00Z";"#,
        // Objects merge.
        "RETURN {a:1} + {b:2};",
        // Strings concatenate.
        r#"RETURN "a" + "b" + "c";"#,
    ] {
        assert!(
            operator_messages(source).is_empty(),
            "`{source}` is valid SurrealQL, got {:?}",
            operator_messages(source)
        );
    }
}

#[test]
fn division_is_never_reported() {
    // `fnc::operate::div` wraps a failure as `unwrap_or(f64::NAN)`, so
    // `[1,2,3] / 1` evaluates to NaN rather than failing. There is no error to
    // surface, and inventing one would squiggle a query that runs.
    for source in [
        "RETURN [1,2,3] / 1;",
        r#"RETURN "abc" / 2;"#,
        "RETURN {a:1} / 2;",
        "RETURN 1d / 24;",
    ] {
        assert!(
            operator_messages(source).is_empty(),
            "`{source}` yields NaN, not an error"
        );
    }
}

#[test]
fn a_comparison_is_never_reported() {
    // No comparison, containment, or logical operator can fail: `=` answers
    // `false` for a mismatched pair and `Value` derives `PartialOrd`, so
    // `1 < "a"` has a defined answer.
    for source in [
        r#"RETURN 1 = "1";"#,
        r#"RETURN 1 < "a";"#,
        r#"RETURN 1 != "1";"#,
        r#"RETURN [1] CONTAINS "a";"#,
        r#"RETURN 1 && "a";"#,
        r#"RETURN 1 ?? "a";"#,
    ] {
        assert!(
            operator_messages(source).is_empty(),
            "`{source}` cannot fail in the engine"
        );
    }
}

// --- Precedence -------------------------------------------------------------

#[test]
fn a_mixed_chain_is_regrouped_to_the_engines_precedence() {
    // The grammar puts every operator on one left-associative level, so it
    // parses this as `(1 + 1) * 3`. SurrealDB reads `1 + (1 * 3)` and answers
    // `4` — its own `precedence.surql` asserts that. Both groupings are silent
    // here, so the type is what proves the re-grouping ran.
    assert!(operator_messages("RETURN 1 + 1 * 3;").is_empty());
    assert_eq!(inferred_type_of("LET $v = 1 + 1 * 3;", "$v"), "int");
}

#[test]
fn a_regrouped_chain_names_the_operands_the_engine_pairs() {
    // Read as parsed this is `("" + 1) * 2`, which would report `string` against
    // `int` for *multiplication*. Re-grouped it is `"" + (1 * 2)`, so the report
    // must name addition — and `int`, the type of `1 * 2`.
    let messages = operator_messages(r#"RETURN "" + 1 * 2;"#);
    assert_eq!(
        messages,
        vec!["Cannot perform addition with `string` and `int`.".to_string()],
        "the diagnostic must describe the pair the engine forms"
    );
}

#[test]
fn a_parenthesised_group_is_respected() {
    // A `SubQuery` ends the chain, so the written grouping stands.
    assert!(operator_messages("RETURN (1 + 1) * 3;").is_empty());
    assert_eq!(inferred_type_of("LET $v = (1 + 1) * 3;", "$v"), "int");
}

#[test]
fn a_long_chain_reports_once() {
    // The check acts at the root of a chain only. Every node on the left spine
    // is itself a `BinaryExpression`, so folding from each would report the same
    // pair once per level.
    assert_eq!(operator_messages(r#"RETURN 1 + 2 + 3 + "a";"#).len(), 1);
}

// --- What the gate refuses to judge ----------------------------------------

#[test]
fn an_operand_that_is_not_provably_one_kind_is_silent() {
    // `value_kind` is the gate for the whole feature. Every one of these must
    // stay silent, and a diagnostic here is a false positive.
    for source in [
        // A parameter with no annotation.
        "DEFINE FUNCTION fn::f($x) { RETURN $x + 1; };",
        // The top type accepts anything.
        "DEFINE FUNCTION fn::f($x: any) { RETURN $x + 1; };",
        // An optional may hold NONE *or* a number, so nothing is provable.
        "DEFINE FUNCTION fn::f($x: option<int>) { RETURN $x + 1; };",
        // A union is the same argument.
        "DEFINE FUNCTION fn::f($x: int | string) { RETURN $x + 1; };",
        // Field access and method calls have no arm in `infer_expr_type`.
        "DEFINE FUNCTION fn::f($x: object) { RETURN $x.count + 1; };",
        // A statement result is untyped.
        "RETURN (SELECT * FROM person) + 1;",
    ] {
        assert!(
            operator_messages(source).is_empty(),
            "`{source}` is not provable, got {:?}",
            operator_messages(source)
        );
    }
}

#[test]
fn an_unparseable_chain_is_silent() {
    // A chain the parser could not read says nothing about its operands, and a
    // syntax diagnostic already covers the position.
    let codes = codes_of(&diagnostics_for(r#"RETURN "a" + ;"#));
    assert!(
        !codes.contains(&"operator-type".to_string()),
        "got {codes:?}"
    );
}

#[test]
fn an_assignment_operator_is_silent() {
    // `+=` parses as a `BinaryExpression` in this grammar but goes through the
    // engine's looser `increment` path, which accepts more than `+` does.
    //
    // Bind `$a` first, so both operands are provably typed and the operator is
    // the only reason this stays silent.
    assert!(operator_messages("LET $a = [1];\nRETURN $a += 1;").is_empty());
    // The same operands with a real `+` are reported, which is what proves the
    // test above is testing the operator and not the gate.
    assert_eq!(operator_messages("LET $a = [1];\nRETURN $a + 1;").len(), 1);
}

// --- Where the new type flows ----------------------------------------------

#[test]
fn an_arithmetic_result_reaches_the_other_checks() {
    // The payoff beyond the operator check itself: an arithmetic expression now
    // has a type, so every check downstream of `infer_expr_type` can see it.
    let messages = messages_of(&diagnostics_for(r#"LET $n: string = 1 + 2;"#));
    assert!(
        messages
            .iter()
            .any(|message| message.contains("declared `string` but the value is `int`")),
        "got {messages:?}"
    );
}

#[test]
fn an_arithmetic_body_is_now_inferrable() {
    // This closes the first gap the return-type-inference change recorded under
    // Known gaps: `RETURN $a + $b` used to leave the function untyped.
    let source = "DEFINE FUNCTION fn::add($a: int, $b: int) { RETURN $a + $b; };\n\
                  LET $v = fn::add(1, 2);";
    assert_eq!(inferred_type_of(source, "$v"), "int");
}

#[test]
fn the_right_side_of_a_short_circuit_is_not_reported() {
    // SurrealDB's own `precedence.surql` asserts `2 + 1 ?: true + 1` is `3`:
    // `?:` returns its truthy left side and never evaluates `true + 1`. A
    // failure that cannot run is not a failure.
    for source in [
        "RETURN 2 + 1 ?: true + 1;",
        "RETURN 2 + 1 ?? true + 1;",
        "RETURN false && true + 1;",
        "RETURN true || true + 1;",
    ] {
        assert!(
            operator_messages(source).is_empty(),
            "`{source}` never evaluates its right side, got {:?}",
            operator_messages(source)
        );
    }
    // The left side always runs, so it is still reported.
    assert_eq!(operator_messages("RETURN true + 1 ?: 2;").len(), 1);
}

#[test]
fn a_fragment_beside_a_parse_error_is_not_reported() {
    // The pinned grammar cannot parse mock syntax, and it fails by leaving
    // `ERROR` nodes *beside* a `BinaryExpression` rather than inside it. The
    // fragment `test:..=-9223372036854775806` then looks like a record id minus
    // an int, which is neither operand anyone wrote.
    let codes = codes_of(&diagnostics_for("|test:..=-9223372036854775806|;"));
    assert!(
        !codes.contains(&"operator-type".to_string()),
        "a parse failure must not become a type error: {codes:?}"
    );
}

#[test]
fn the_engines_own_method_fixture_reports_nothing() {
    // `language/functions/method_syntax.surql`, copied verbatim from SurrealDB.
    // Its own front matter says: "Asserts that no errors are produced when every
    // function registered for method syntax is called in that way", and it
    // expects `NONE`. 198 calls covering 93% of the 252 method names.
    //
    // This is the strongest guard this change has. Every diagnostic here is a
    // false positive by the engine's own declaration.
    let source = include_str!("fixtures/method_syntax.surql");
    let noisy: Vec<String> = diagnostics_for(source)
        .into_iter()
        .filter(|diagnostic| match &diagnostic.code {
            Some(tower_lsp_server::ls_types::NumberOrString::String(code)) => {
                code.starts_with("argument-") || code == "unknown-method"
            }
            _ => false,
        })
        .map(|diagnostic| {
            format!(
                "line {}: {}",
                diagnostic.range.start.line + 1,
                diagnostic.message
            )
        })
        .collect();
    assert!(
        noisy.is_empty(),
        "{} false positives on the engine's own fixture:\n  {}",
        noisy.len(),
        noisy.join("\n  ")
    );
}

#[test]
fn an_object_method_the_tables_do_not_hold_is_not_reported() {
    // When method dispatch fails on an object the engine retries the name as a
    // closure-valued field, so `{ a: |$x| $x }.a(1)` is legal and the field may
    // be named anything. Three files in SurrealDB's own corpus rely on it.
    for source in [
        "LET $obj = { a: |$a: int| $a };\nRETURN $obj.a(1);",
        "RETURN { fnc: |$x| $x }.fnc(1);",
    ] {
        let codes = codes_of(&diagnostics_for(source));
        assert!(
            !codes.contains(&"unknown-method".to_string()),
            "`{source}` calls a closure-valued field, got {codes:?}"
        );
    }
    // A receiver with no such fallback still reports.
    let codes = codes_of(&diagnostics_for("RETURN 'abc'.nonsense();"));
    assert!(
        codes.contains(&"unknown-method".to_string()),
        "a string has no closure-field fallback: {codes:?}"
    );
}

#[test]
fn a_method_hover_names_the_function_it_resolves_to() {
    let hover = hover_at("RETURN (5).round();", "round").expect("hover for .round()");
    assert!(hover.contains("math::round"), "got {hover}");
    assert!(hover.contains(".round()"), "got {hover}");
}

#[test]
fn a_method_hover_no_longer_answers_with_a_keyword() {
    // `AT` and `SPLIT` are both SurrealQL keywords, and `token_at` treats `.` as
    // a boundary — so hovering these used to describe the *keyword*. A wrong
    // answer, not a missing one.
    let at = hover_at("RETURN [1, 2].at(0);", "at").expect("hover for .at()");
    assert!(at.contains("array::at"), "got {at}");
    assert!(!at.contains("SurrealQL keyword"), "got {at}");

    let split = hover_at("RETURN 'a,b'.split(',');", "split").expect("hover for .split()");
    assert!(split.contains("string::split"), "got {split}");
    assert!(!split.contains("SurrealQL keyword"), "got {split}");
}

#[test]
fn a_method_hover_states_a_derived_return_type() {
    let hover = hover_at("RETURN 'abc'.is_alphanum();", "is_alphanum").expect("hover");
    assert!(hover.contains("string::is_alphanum"), "got {hover}");
}

#[test]
fn an_optional_chained_method_resolves() {
    // `$v.?.trim()` reads the same value as `$v.trim()`. The receiver search used
    // to insist the method be the first link, so this shape — six occurrences
    // across the two fixtures — resolved to nothing.
    let source = "LET $v = 'abc';\nRETURN $v.?.len('nope');";
    let messages = messages_of(&diagnostics_for(source));
    assert!(
        messages
            .iter()
            .any(|message| message.contains("string::len")),
        "an optional chain must still resolve: {messages:?}"
    );
}

#[test]
fn a_method_chain_carries_its_type_forward() {
    // `to_uuid` hands a `uuid` to the next link, so the chain types as `bool`.
    assert_eq!(
        inferred_type_of(
            "LET $v = '019535d9-3df7-79fb-b466-fa907fa17f9e'.to_uuid().is_uuid();",
            "$v"
        ),
        "bool"
    );
    // And a single link types too.
    assert_eq!(inferred_type_of("LET $v = (5).round();", "$v"), "number");
    assert_eq!(inferred_type_of("LET $v = 'abc'.to_int();", "$v"), "int");
}

// ---------------------------------------------------------------------------
// Builtin return types, read from the engine's registry
// ---------------------------------------------------------------------------

// The catalogue used to carry argument types and nothing else, so a call to any
// of the 24 namespaces outside `string::` and `type::` typed as `unknown` and
// silenced every check downstream of it. These cover the namespaces that gained
// a type, and — more importantly — the places that must stay silent.

#[test]
fn a_namespaced_call_now_has_a_type() {
    // The reported case. `rand::uuid::v4` is declared `() -> Uuid` in the
    // engine's registry; nothing had to be hand-written to say so.
    assert_eq!(inferred_type_of("LET $x = rand::uuid::v4();", "$x"), "uuid");
    for (source, expected) in [
        ("LET $x = time::now();", "datetime"),
        ("LET $x = array::len([1, 2, 3]);", "int"),
        ("LET $x = math::abs(-1);", "number"),
        ("LET $x = crypto::sha256('a');", "string"),
        ("LET $x = duration::secs(1h);", "int"),
        ("LET $x = rand::bool();", "bool"),
        ("LET $x = object::keys({ a: 1 });", "array<string>"),
        ("LET $x = vector::add([1], [2]);", "array<number>"),
    ] {
        assert_eq!(inferred_type_of(source, "$x"), expected, "{source}");
    }
}

#[test]
fn a_type_from_a_namespaced_call_reaches_the_argument_check() {
    // The point of typing the call at all: the type has to travel.
    let messages = messages_of(&diagnostics_for(
        "LET $x = rand::uuid::v4();\nRETURN string::len($x);",
    ));
    assert!(
        messages.iter().any(|message| message
            .contains("Argument 1 of `string::len` expects `string`, found `uuid`")),
        "got {messages:?}"
    );
}

#[test]
fn a_type_from_a_namespaced_call_reaches_the_let_check() {
    let messages = messages_of(&diagnostics_for("LET $x: int = rand::uuid::v4();"));
    assert!(
        messages
            .iter()
            .any(|message| message.contains("declared `int` but the value is `uuid`")),
        "got {messages:?}"
    );
}

#[test]
fn a_type_from_a_namespaced_call_reaches_the_return_check() {
    let messages = messages_of(&diagnostics_for(
        "DEFINE FUNCTION fn::f() -> int { RETURN rand::uuid::v4(); };",
    ));
    assert!(
        messages
            .iter()
            .any(|message| message.contains("returns `int`")),
        "got {messages:?}"
    );
}

#[test]
fn a_datetime_from_the_engine_follows_the_engines_own_arithmetic() {
    // `time::now()` types as `datetime` now, which reaches the operator check
    // for the first time. The engine has a `(Datetime, Duration)` arm for `+`
    // and none for `(Datetime, Int)`, and the check must match it exactly —
    // this is where a new type is most likely to invent a false positive.
    assert!(
        operator_messages("LET $t = time::now();\nRETURN $t + 1h;").is_empty(),
        "datetime + duration is legal: {:?}",
        operator_messages("LET $t = time::now();\nRETURN $t + 1h;")
    );
    assert_eq!(
        operator_messages("LET $t = time::now();\nRETURN $t + 1;").len(),
        1,
        "datetime + int is not"
    );
    // `datetime - datetime` is the one arm that changes category.
    assert_eq!(
        inferred_type_of("LET $d = time::now() - time::now();", "$d"),
        "duration"
    );
}

#[test]
fn the_call_form_and_the_method_form_agree() {
    // These disagreed before: the call path read the curated table and the
    // method path read three hand-written tables that covered `math::` and the
    // `time::`/`duration::` accessors. One resolver now answers both.
    for (call, method) in [
        ("LET $x = math::abs(-1);", "LET $x = (-1).abs();"),
        (
            "LET $x = time::year(time::now());",
            "LET $x = time::now().year();",
        ),
        ("LET $x = string::len('a');", "LET $x = 'a'.len();"),
    ] {
        assert_eq!(
            inferred_type_of(call, "$x"),
            inferred_type_of(method, "$x"),
            "`{call}` and `{method}` must agree"
        );
    }
}

#[test]
fn a_return_type_that_follows_an_argument_stays_silent() {
    // The half of the engine's declarations that say `Kind::Any`. These return
    // whatever they were handed, so no single type is right and the checker must
    // report nothing rather than guess. A regression here is a false positive on
    // working code, which costs more than the silence it replaced.
    for source in [
        "LET $x = array::first([1, 2]);",
        "LET $x = array::at([1, 2], 0);",
        "LET $x = object::values({ a: 1 });",
        "LET $x = array::group([[1], [2]]);",
    ] {
        assert_eq!(inferred_type_of(source, "$x"), "unknown", "{source}");
    }
}

#[test]
fn the_curated_table_still_wins_where_it_is_more_specific() {
    // The engine's macros take a bare identifier, so they cannot spell
    // `array<string>` and `string::split` declares `Any` there. The curated
    // entry is more specific and must not be overwritten by it.
    assert_eq!(
        inferred_type_of("LET $x = string::split('a,b', ',');", "$x"),
        "array<string>"
    );
    // And where the curated table has no entry, the engine answers.
    assert_eq!(
        inferred_type_of("LET $x = string::semver::major('1.2.3');", "$x"),
        "int"
    );
}

#[test]
fn an_unreadable_argument_signature_still_reports_its_return_type() {
    // `rand::int` takes `NoneOrRange<i64>`, whose arity the catalogue cannot
    // express, so `signature_known` is false and there is no signature to hang
    // an arrow on. The registry still declares the return type.
    assert_eq!(inferred_type_of("LET $x = rand::int(1, 10);", "$x"), "int");
    let model = MergedSemanticModel::default();
    let hover = model
        .hover_markdown_for_token("rand::int", None)
        .expect("hover");
    assert!(
        hover.contains("Returns: `int`"),
        "a function with no readable signature must still state its return type: {hover}"
    );
}

#[test]
fn hover_on_a_namespaced_call_shows_the_return_type() {
    let model = MergedSemanticModel::default();
    let hover = model
        .hover_markdown_for_token("rand::uuid::v4", None)
        .expect("hover");
    assert!(
        hover.contains("rand::uuid::v4() -> uuid"),
        "the signature must carry the return type: {hover}"
    );
    // And it is said once, not twice.
    assert!(
        !hover.contains("Returns:"),
        "the arrow already says it: {hover}"
    );
}

#[test]
fn a_wrong_engine_declaration_is_corrected() {
    // `crypto::joaat` is declared `-> String` in the engine's registry, but its
    // implementation hashes to a `u32` and returns an `int`. The registry is
    // never read by SurrealDB itself, so nothing upstream caught it;
    // `cargo run -p xtask --features probe -- verify-returns` did, by calling
    // the function and looking at the answer.
    //
    // Believing the declaration would report this composition as a type error on
    // code the engine runs happily, so this test is the guard on that.
    assert_eq!(
        inferred_type_of("LET $h = crypto::joaat('tobie');", "$h"),
        "int"
    );
    assert!(
        operator_messages("LET $h = crypto::joaat('tobie');\nRETURN $h + 1;").is_empty(),
        "an int may be added to an int"
    );
    let codes = codes_of(&diagnostics_for(
        "RETURN math::abs(crypto::joaat('tobie'));",
    ));
    assert!(
        !codes.contains(&"argument-type".to_string()),
        "a hash is a number, so `math::abs` accepts it: {codes:?}"
    );
    // And the sibling hashes really are strings, so the correction is narrow.
    assert_eq!(
        inferred_type_of("LET $h = crypto::sha256('tobie');", "$h"),
        "string"
    );
}
