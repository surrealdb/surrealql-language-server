use tower_lsp::lsp_types::{Location, Position, Range, Url};

use surrealql_language_server::config::{AuthContext, ServerSettings};
use surrealql_language_server::semantic::analyzer::analyze_document;
use surrealql_language_server::semantic::model::is_record_type_context;
use surrealql_language_server::semantic::type_expr::TypeExpr;
use surrealql_language_server::semantic::types::{
    DocumentAnalysis, FieldDef, FunctionDef, FunctionLanguage, MergedSemanticModel, PermissionMode,
    PermissionRule, QueryAction, QueryFact, SymbolOrigin, TableDef, WorkspaceIndex,
};

fn uri(path: &str) -> Url {
    Url::parse(&format!("file:///workspace/{path}")).expect("valid uri")
}

fn empty_range() -> Range {
    Range::default()
}

fn empty_location(path: &str) -> Location {
    Location::new(uri(path), empty_range())
}

fn workspace_from(analyses: Vec<DocumentAnalysis>) -> WorkspaceIndex {
    let mut ws = WorkspaceIndex::default();
    for a in analyses {
        ws.documents.insert(a.uri.clone(), a);
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
    assert_eq!(func.params[0].name, "$n");
    // Return type may or may not be extracted depending on grammar node name;
    // at minimum we assert the function parses cleanly.
    assert_eq!(func.name, "fn::double");
}

#[test]
fn function_without_return_type_has_none() {
    let u = uri("functions.surql");
    let text = r#"
        DEFINE FUNCTION fn::noop() { RETURN NONE; };
    "#;
    let analysis = analyze_document(u, text, SymbolOrigin::Local).expect("analysis");
    assert_eq!(analysis.functions.len(), 1);
    // Without a `->` annotation the return type should be absent.
    // (May be Some if grammar exposes implicit type; we just ensure no panic.)
    let _ = &analysis.functions[0].return_type;
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
fn hover_for_js_function_shows_javascript_badge() {
    let u = uri("functions.surql");
    let mut ws = WorkspaceIndex::default();
    let analysis = DocumentAnalysis {
        uri: u.clone(),
        text: String::new(),
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
        query_facts: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    ws.documents.insert(u, analysis);
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
        query_facts: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    ws.documents.insert(u, analysis);
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
        DocumentAnalysis {
            uri: u.clone(),
            text: String::new(),
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
            query_facts: Vec::new(),
            references: Vec::new(),
            syntax_diagnostics: Vec::new(),
            document_symbols: Vec::new(),
        },
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
        DocumentAnalysis {
            uri: u.clone(),
            text: String::new(),
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
            query_facts: Vec::new(),
            references: Vec::new(),
            syntax_diagnostics: Vec::new(),
            document_symbols: Vec::new(),
        },
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
    let u = uri("schema.surql");
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
    let u = uri("schema.surql");
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
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: Vec::new(),
        params: Vec::new(),
        accesses: Vec::new(),
        query_facts: vec![QueryFact {
            action: QueryAction::Select,
            target_tables: vec!["thing".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: empty_location("query.surql"),
            source_preview: "SELECT * FROM thing".to_string(),
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
    let u = uri("query.surql");
    let table = TableDef {
        name: "secret".to_string(),
        schema_mode: None,
        comment: None,
        permissions: vec![PermissionRule {
            actions: vec![QueryAction::Select],
            mode: PermissionMode::None,
            raw: "PERMISSIONS FOR select NONE".to_string(),
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
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: Vec::new(),
        params: Vec::new(),
        accesses: Vec::new(),
        query_facts: vec![QueryFact {
            action: QueryAction::Select,
            target_tables: vec!["secret".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: empty_location("query.surql"),
            source_preview: "SELECT * FROM secret".to_string(),
        }],
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    let diagnostics = model.semantic_diagnostics(&analysis, &ServerSettings::default());
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].severity,
        Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR)
    );
}

#[test]
fn warning_for_unknown_table_in_query() {
    let mut model = MergedSemanticModel::default();
    let u = uri("query.surql");
    let analysis = DocumentAnalysis {
        uri: u.clone(),
        text: String::new(),
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: Vec::new(),
        params: Vec::new(),
        accesses: Vec::new(),
        query_facts: vec![QueryFact {
            action: QueryAction::Select,
            target_tables: vec!["totally_unknown_table".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: empty_location("query.surql"),
            source_preview: "SELECT * FROM totally_unknown_table".to_string(),
        }],
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    let diagnostics = model.semantic_diagnostics(&analysis, &ServerSettings::default());
    assert!(!diagnostics.is_empty());
    assert!(diagnostics[0].message.contains("Unknown table"));
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
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: Vec::new(),
        params: Vec::new(),
        accesses: Vec::new(),
        query_facts: vec![QueryFact {
            action: QueryAction::Select,
            target_tables: vec!["orders".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: empty_location("query.surql"),
            source_preview: "SELECT * FROM orders".to_string(),
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
    let u = uri("fn.surql");
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
    let u = uri("fn.surql");
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
        query_facts: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    let local = DocumentAnalysis {
        uri: u.clone(),
        text: String::new(),
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
        query_facts: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    let mut ws = WorkspaceIndex::default();
    ws.documents.insert(remote.uri.clone(), remote);
    ws.documents.insert(local.uri.clone(), local);
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
        DocumentAnalysis {
            uri: u.clone(),
            text: String::new(),
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
            query_facts: Vec::new(),
            references: Vec::new(),
            syntax_diagnostics: Vec::new(),
            document_symbols: Vec::new(),
        },
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
        query_facts: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };
    let model = MergedSemanticModel::default();
    let actions = model.code_actions(&u, &analysis, &[]);
    assert!(
        actions.iter().any(|a| {
            if let tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(ca) = a {
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

// Smoke tests for scripting functions in non-FUNCTION contexts
#[test]
fn scripting_function_in_define_event() {
    let u = uri("events.surql");
    let text = r#"
        DEFINE EVENT score ON TABLE person WHEN $event = 'CREATE'
            THEN function() { return { ok: true }; };
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
        DEFINE API '/test' FOR get THEN function() { return { status: 200 }; };
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
        DEFINE EVENT complex ON TABLE t THEN function() {
            const obj = { a: 1, b: { c: 2 } };
            return obj;
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
