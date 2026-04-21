use tower_lsp::lsp_types::{
    Diagnostic, DiagnosticSeverity, DocumentSymbol, Location, SymbolKind, Url,
};
use tree_sitter::{Node, Parser};

use crate::grammar::language;
use crate::semantic::text::{byte_range_to_lsp, compact_preview};
use crate::semantic::type_expr::TypeExpr;
use crate::semantic::types::{
    AccessDef, DocumentAnalysis, EventDef, FieldDef, FunctionDef, FunctionLanguage, FunctionParam,
    IndexDef, InferenceFact, ParamDef, PermissionMode, PermissionRule, QueryAction, QueryFact,
    SymbolOrigin, SymbolReference, TableDef,
};

const TRANSPARENT_NODES: &[&str] = &[
    "source_file",
    "expressions",
    "expression",
    "subquery_statement",
    "primary_statement",
];

pub fn analyze_document(uri: Url, text: &str, origin: SymbolOrigin) -> Option<DocumentAnalysis> {
    let mut parser = Parser::new();
    parser.set_language(&language()).ok()?;
    let tree = parser.parse(text, None)?;
    let root = tree.root_node();

    let mut analysis = DocumentAnalysis {
        uri: uri.clone(),
        text: text.to_string(),
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: Vec::new(),
        params: Vec::new(),
        accesses: Vec::new(),
        query_facts: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: collect_syntax_diagnostics(text, root),
        document_symbols: Vec::new(),
    };

    collect_statements(root, text, &uri, origin, &mut analysis);
    Some(analysis)
}

pub fn collect_syntax_diagnostics(source: &str, node: Node<'_>) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    collect_node_diagnostics(source, node, &mut diagnostics);
    diagnostics
}

fn collect_statements(
    node: Node<'_>,
    source: &str,
    uri: &Url,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let kind = node.kind();
    if TRANSPARENT_NODES.contains(&kind) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect_statements(child, source, uri, origin, analysis);
        }
        return;
    }

    if kind.ends_with("_statement") {
        match kind {
            "define_table_statement" => extract_table(node, source, uri, origin, analysis),
            "define_event_statement" => extract_event(node, source, uri, origin, analysis),
            "define_field_statement" => extract_field(node, source, uri, origin, analysis),
            "define_function_statement" => extract_function(node, source, uri, origin, analysis),
            "define_index_statement" => extract_index(node, source, uri, origin, analysis),
            "define_param_statement" => extract_param(node, source, uri, origin, analysis),
            "define_access_statement" => extract_access(node, source, uri, origin, analysis),
            "select_statement" => {
                extract_query_fact(node, source, uri, QueryAction::Select, analysis)
            }
            "create_statement" => {
                extract_query_fact(node, source, uri, QueryAction::Create, analysis)
            }
            "update_statement" | "upsert_statement" => {
                extract_query_fact(node, source, uri, QueryAction::Update, analysis)
            }
            "delete_statement" => {
                extract_query_fact(node, source, uri, QueryAction::Delete, analysis)
            }
            "relate_statement" => {
                extract_query_fact(node, source, uri, QueryAction::Relate, analysis)
            }
            _ => {
                if let Some(symbol) = statement_symbol(node, source, uri) {
                    analysis.document_symbols.push(symbol);
                }
            }
        }

        collect_function_references(node, source, uri, analysis);
        return;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_statements(child, source, uri, origin, analysis);
    }
}

fn extract_table(
    node: Node<'_>,
    source: &str,
    uri: &Url,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let Some(name) = direct_named_children(node)
        .into_iter()
        .find(|child| child.kind() == "identifier")
        .and_then(|child| text_of(source, child))
    else {
        return;
    };

    let table = TableDef {
        name: name.clone(),
        schema_mode: direct_named_children(node)
            .into_iter()
            .find_map(|child| match child.kind() {
                "keyword_schemafull" => Some("schemafull".to_string()),
                "keyword_schemaless" => Some("schemaless".to_string()),
                _ => None,
            }),
        comment: extract_comment(node, source),
        permissions: direct_named_children(node)
            .into_iter()
            .filter(|child| child.kind() == "permissions_for_clause")
            .map(|child| parse_permission_rule(child, source, origin, uri))
            .collect(),
        origin,
        explicit: true,
        inference: None,
        location: location(uri, source, node),
    };

    for inferred in infer_record_types_from_table(&table, uri, source, node) {
        upsert_inferred_table(analysis, inferred, uri, source, node);
    }

    analysis.document_symbols.push(definition_symbol(
        &format!("TABLE {name}"),
        SymbolKind::STRUCT,
        source,
        node,
    ));
    analysis.tables.push(table);
}

fn extract_field(
    node: Node<'_>,
    source: &str,
    uri: &Url,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let children = direct_named_children(node);
    let Some(name) = children
        .iter()
        .find(|child| child.kind() == "inclusive_predicate")
        .and_then(|child| text_of(source, *child))
        .map(|value| {
            value
                .split(',')
                .next()
                .unwrap_or(value.as_str())
                .trim()
                .to_string()
        })
    else {
        return;
    };

    let table = children
        .iter()
        .find(|child| child.kind() == "on_table_clause")
        .and_then(|child| {
            direct_named_children(*child)
                .into_iter()
                .find(|item| item.kind() == "identifier")
        })
        .and_then(|child| text_of(source, child))
        .unwrap_or_else(|| "unknown".to_string());
    let type_expr = children
        .iter()
        .find(|child| child.kind() == "type_clause")
        .and_then(|child| {
            direct_named_children(*child)
                .into_iter()
                .find(|item| item.kind() == "type")
        })
        .and_then(|child| text_of(source, child))
        .map(|text| TypeExpr::parse(&text));

    let field = FieldDef {
        table: table.clone(),
        name: name.clone(),
        type_expr: type_expr.clone(),
        comment: extract_comment(node, source),
        permissions: children
            .iter()
            .copied()
            .filter(|child| child.kind() == "permissions_for_clause")
            .map(|child| parse_permission_rule(child, source, origin, uri))
            .collect(),
        origin,
        explicit: true,
        inference: None,
        location: location(uri, source, node),
    };

    if let Some(type_expr) = type_expr {
        for record_table in type_expr.record_tables() {
            let inferred = TableDef {
                name: record_table.clone(),
                schema_mode: None,
                comment: Some(format!("Inferred from field `{name}` type.")),
                permissions: Vec::new(),
                origin,
                explicit: false,
                inference: Some(InferenceFact {
                    confidence: 0.75,
                    origin,
                    evidence: format!(
                        "Field `{table}.{name}` references `record<{record_table}>`."
                    ),
                }),
                location: location(uri, source, node),
            };
            upsert_inferred_table(analysis, inferred, uri, source, node);
        }
    }

    analysis.document_symbols.push(definition_symbol(
        &format!("FIELD {table}.{name}"),
        SymbolKind::FIELD,
        source,
        node,
    ));
    analysis.fields.push(field);
}

fn extract_event(
    node: Node<'_>,
    source: &str,
    uri: &Url,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let children = direct_named_children(node);
    let Some(name) = children
        .iter()
        .find(|child| child.kind() == "identifier")
        .and_then(|child| text_of(source, *child))
    else {
        return;
    };
    let table = children
        .iter()
        .find(|child| child.kind() == "on_table_clause")
        .and_then(|child| identifier_from_on_table_clause(*child, source))
        .unwrap_or_else(|| "unknown".to_string());
    let when_then = children
        .iter()
        .find(|child| child.kind() == "when_then_clause");
    let when_clause = when_then
        .and_then(|child| {
            descendants_of_kind(*child, "when_clause")
                .into_iter()
                .next()
        })
        .and_then(|child| text_of(source, child))
        .map(|text| compact_preview(&text));
    let then_clause = when_then
        .and_then(|child| {
            descendants_of_kind(*child, "then_clause")
                .into_iter()
                .next()
        })
        .and_then(|child| text_of(source, child))
        .map(|text| compact_preview(&text));

    analysis.document_symbols.push(definition_symbol(
        &format!("EVENT {table}.{name}"),
        SymbolKind::EVENT,
        source,
        node,
    ));
    analysis.events.push(EventDef {
        table,
        name,
        comment: extract_comment(node, source),
        when_clause,
        then_clause,
        origin,
        location: location(uri, source, node),
    });
}

fn extract_function(
    node: Node<'_>,
    source: &str,
    uri: &Url,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let children = direct_named_children(node);
    let Some(name) = children
        .iter()
        .find(|child| child.kind() == "custom_function_name")
        .and_then(|child| text_of(source, *child))
    else {
        return;
    };

    let params = children
        .iter()
        .find(|child| child.kind() == "param_list")
        .map(|child| parse_function_params(*child, source))
        .unwrap_or_default();

    let return_type = children
        .iter()
        .find(|child| child.kind() == "type" || child.kind() == "return_type_clause")
        .and_then(|child| text_of(source, *child))
        .map(|text| TypeExpr::parse(text.trim_start_matches("->").trim()));

    let language = detect_function_language(&children, source, node);

    let permissions = children
        .iter()
        .copied()
        .filter(|child| child.kind() == "permissions_basic_clause")
        .map(|child| parse_permission_rule(child, source, origin, uri))
        .collect::<Vec<_>>();

    let body_node = children
        .iter()
        .find(|child| child.kind() == "block")
        .copied();

    let called_functions = body_node
        .map(|body| collect_called_functions(body, source))
        .unwrap_or_default();

    let selection_range = children
        .iter()
        .find(|child| child.kind() == "custom_function_name")
        .map(|child| byte_range_to_lsp(source, child.start_byte(), child.end_byte()))
        .unwrap_or_else(|| byte_range_to_lsp(source, node.start_byte(), node.end_byte()));

    analysis.document_symbols.push(definition_symbol(
        &format!("FUNCTION {name}"),
        SymbolKind::FUNCTION,
        source,
        node,
    ));
    analysis.references.push(SymbolReference {
        name: name.clone(),
        kind: SymbolKind::FUNCTION,
        location: Location::new(uri.clone(), selection_range),
        selection_range,
    });
    analysis.functions.push(FunctionDef {
        name,
        params,
        return_type,
        language,
        comment: extract_comment(node, source),
        permissions,
        origin,
        explicit: true,
        inference: None,
        location: location(uri, source, node),
        selection_range,
        body_range: body_node
            .map(|body| byte_range_to_lsp(source, body.start_byte(), body.end_byte())),
        called_functions,
    });
}

fn detect_function_language(
    children: &[Node<'_>],
    _source: &str,
    _node: Node<'_>,
) -> FunctionLanguage {
    // A DEFINE FUNCTION is JavaScript if its body block contains any scripting_function node.
    let has_scripting_fn = children
        .iter()
        .find(|child| child.kind() == "block")
        .is_some_and(|block| !descendants_of_kind(*block, "scripting_function").is_empty());
    if has_scripting_fn {
        FunctionLanguage::JavaScript
    } else {
        FunctionLanguage::SurrealQL
    }
}

fn extract_index(
    node: Node<'_>,
    source: &str,
    uri: &Url,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let children = direct_named_children(node);
    let Some(name) = children
        .iter()
        .find(|child| child.kind() == "identifier")
        .and_then(|child| text_of(source, *child))
    else {
        return;
    };
    let table = children
        .iter()
        .find(|child| child.kind() == "on_table_clause")
        .and_then(|child| identifier_from_on_table_clause(*child, source))
        .unwrap_or_else(|| "unknown".to_string());
    let fields = children
        .iter()
        .find(|child| child.kind() == "fields_columns_clause")
        .map(|child| {
            descendants_of_kind(*child, "identifier")
                .into_iter()
                .filter_map(|item| text_of(source, item))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let unique = children.iter().any(|child| child.kind() == "unique_clause");
    let options = children
        .iter()
        .copied()
        .filter(|child| {
            matches!(
                child.kind(),
                "search_analyzer_clause" | "mtree_dimension_clause" | "hnsw_dimension_clause"
            )
        })
        .filter_map(|child| text_of(source, child))
        .map(|text| compact_preview(&text))
        .collect::<Vec<_>>();

    analysis.document_symbols.push(definition_symbol(
        &format!("INDEX {table}.{name}"),
        SymbolKind::KEY,
        source,
        node,
    ));
    analysis.indexes.push(IndexDef {
        table,
        name,
        fields,
        unique,
        options,
        origin,
        location: location(uri, source, node),
    });
}

fn extract_param(
    node: Node<'_>,
    source: &str,
    uri: &Url,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let children = direct_named_children(node);
    let Some(name) = children
        .iter()
        .find(|child| child.kind() == "variable_name")
        .and_then(|child| text_of(source, *child))
    else {
        return;
    };
    let value_preview = children
        .iter()
        .find(|child| child.kind() == "value")
        .and_then(|child| text_of(source, *child))
        .map(|text| compact_preview(&text));

    analysis.document_symbols.push(definition_symbol(
        &format!("PARAM {name}"),
        SymbolKind::CONSTANT,
        source,
        node,
    ));
    analysis.params.push(ParamDef {
        name,
        value_preview,
        comment: extract_comment(node, source),
        origin,
        location: location(uri, source, node),
    });
}

fn extract_access(
    node: Node<'_>,
    source: &str,
    uri: &Url,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let Some(name) = direct_named_children(node)
        .into_iter()
        .find(|child| child.kind() == "identifier")
        .and_then(|child| text_of(source, child))
    else {
        return;
    };

    analysis.document_symbols.push(definition_symbol(
        &format!("ACCESS {name}"),
        SymbolKind::OBJECT,
        source,
        node,
    ));
    analysis.accesses.push(AccessDef {
        name,
        comment: None,
        origin,
        location: location(uri, source, node),
    });
}

fn extract_query_fact(
    node: Node<'_>,
    source: &str,
    uri: &Url,
    action: QueryAction,
    analysis: &mut DocumentAnalysis,
) {
    let targets = target_tables_for_statement(node, source);
    let touched_fields = collect_field_names(node, source);
    let dynamic = targets.is_empty();
    let preview = node
        .utf8_text(source.as_bytes())
        .ok()
        .map(compact_preview)
        .unwrap_or_default();

    analysis.document_symbols.push(
        statement_symbol(node, source, uri)
            .unwrap_or_else(|| definition_symbol(&preview, SymbolKind::EVENT, source, node)),
    );

    analysis.query_facts.push(QueryFact {
        action,
        target_tables: targets.clone(),
        touched_fields: touched_fields.clone(),
        dynamic,
        location: location(uri, source, node),
        source_preview: preview,
    });

    for table in &targets {
        let inferred = TableDef {
            name: table.clone(),
            schema_mode: None,
            comment: Some(format!("Inferred from {} statement.", action_label(action))),
            permissions: Vec::new(),
            origin: SymbolOrigin::Inferred,
            explicit: false,
            inference: Some(InferenceFact {
                confidence: 0.6,
                origin: SymbolOrigin::Inferred,
                evidence: format!("Observed `{table}` in {} statement.", action_label(action)),
            }),
            location: location(uri, source, node),
        };
        upsert_inferred_table(analysis, inferred, uri, source, node);
    }

    for inferred_field in
        infer_fields_from_statement(node, source, uri, action, &targets, &touched_fields)
    {
        analysis.fields.push(inferred_field);
    }
}

fn infer_fields_from_statement(
    node: Node<'_>,
    source: &str,
    uri: &Url,
    action: QueryAction,
    targets: &[String],
    touched_fields: &[String],
) -> Vec<FieldDef> {
    let mut fields = Vec::new();
    let target_table = targets
        .first()
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());

    for assignment in descendants_of_kind(node, "field_assignment") {
        let children = direct_named_children(assignment);
        let Some(name) = children
            .iter()
            .find(|child| child.kind() == "identifier")
            .and_then(|child| text_of(source, *child))
        else {
            continue;
        };
        let type_expr = children
            .iter()
            .find(|child| child.kind() == "value")
            .map(|child| infer_type_from_value(*child, source));

        fields.push(inferred_field(
            &target_table,
            &name,
            type_expr,
            uri,
            source,
            assignment,
            action,
        ));
    }

    for object in descendants_of_kind(node, "object") {
        for child in direct_named_children(object) {
            if child.kind() != "object_property" {
                continue;
            }
            let property_children = direct_named_children(child);
            let Some(name) = property_children
                .first()
                .and_then(|item| text_of(source, *item))
            else {
                continue;
            };
            let value_node = property_children
                .last()
                .copied()
                .filter(|value| value.kind() != "object_key");
            let type_expr = value_node.map(|value| infer_type_from_value(value, source));
            fields.push(inferred_field(
                &target_table,
                &name,
                type_expr,
                uri,
                source,
                child,
                action,
            ));
        }
    }

    for field in touched_fields {
        if fields.iter().any(|existing| existing.name == *field) {
            continue;
        }
        fields.push(inferred_field(
            &target_table,
            field,
            None,
            uri,
            source,
            node,
            action,
        ));
    }

    fields
}

fn inferred_field(
    table: &str,
    field: &str,
    type_expr: Option<TypeExpr>,
    uri: &Url,
    source: &str,
    node: Node<'_>,
    action: QueryAction,
) -> FieldDef {
    FieldDef {
        table: table.to_string(),
        name: field.to_string(),
        type_expr,
        comment: Some(format!("Inferred from {} statement.", action_label(action))),
        permissions: Vec::new(),
        origin: SymbolOrigin::Inferred,
        explicit: false,
        inference: Some(InferenceFact {
            confidence: 0.55,
            origin: SymbolOrigin::Inferred,
            evidence: format!(
                "Field `{field}` observed in {} statement.",
                action_label(action)
            ),
        }),
        location: location(uri, source, node),
    }
}

fn collect_function_references(
    node: Node<'_>,
    source: &str,
    uri: &Url,
    analysis: &mut DocumentAnalysis,
) {
    for reference in descendants_of_kind(node, "custom_function_name") {
        if ancestor_kind(reference, "define_function_statement").is_some() {
            continue;
        }
        let Some(name) = text_of(source, reference) else {
            continue;
        };
        let selection_range =
            byte_range_to_lsp(source, reference.start_byte(), reference.end_byte());
        analysis.references.push(SymbolReference {
            name,
            kind: SymbolKind::FUNCTION,
            location: Location::new(uri.clone(), selection_range),
            selection_range,
        });
    }
}

fn collect_called_functions(node: Node<'_>, source: &str) -> Vec<String> {
    descendants_of_kind(node, "custom_function_name")
        .into_iter()
        .filter_map(|child| text_of(source, child))
        .collect()
}

fn infer_record_types_from_table(
    _table: &TableDef,
    _uri: &Url,
    _source: &str,
    _node: Node<'_>,
) -> Vec<TableDef> {
    Vec::new()
}

fn parse_function_params(node: Node<'_>, source: &str) -> Vec<FunctionParam> {
    let mut params = Vec::new();
    let mut pending_name = None;

    for child in direct_named_children(node) {
        match child.kind() {
            "variable_name" => pending_name = text_of(source, child),
            "type" => {
                if let Some(name) = pending_name.take() {
                    params.push(FunctionParam {
                        name,
                        type_expr: text_of(source, child).map(|text| TypeExpr::parse(&text)),
                    });
                }
            }
            _ => {}
        }
    }

    if let Some(name) = pending_name {
        params.push(FunctionParam {
            name,
            type_expr: None,
        });
    }

    params
}

fn parse_permission_rule(
    node: Node<'_>,
    source: &str,
    origin: SymbolOrigin,
    uri: &Url,
) -> PermissionRule {
    let children = direct_named_children(node);
    let mut actions = Vec::new();
    for child in &children {
        match child.kind() {
            "keyword_select" => actions.push(QueryAction::Select),
            "keyword_create" => actions.push(QueryAction::Create),
            "keyword_update" => actions.push(QueryAction::Update),
            "keyword_delete" => actions.push(QueryAction::Delete),
            _ => {}
        }
    }
    if actions.is_empty() {
        actions.push(QueryAction::Execute);
    }

    let mode = if children.iter().any(|child| child.kind() == "keyword_full") {
        PermissionMode::Full
    } else if children.iter().any(|child| child.kind() == "keyword_none") {
        PermissionMode::None
    } else {
        let expression = children
            .iter()
            .find(|child| matches!(child.kind(), "where_clause" | "value"))
            .and_then(|child| text_of(source, *child))
            .unwrap_or_else(|| text_of(source, node).unwrap_or_default());
        PermissionMode::Expression(expression)
    };

    PermissionRule {
        actions,
        mode,
        raw: text_of(source, node).unwrap_or_default(),
        origin,
        location: Some(location(uri, source, node)),
    }
}

fn target_tables_for_statement(node: Node<'_>, source: &str) -> Vec<String> {
    let relevant_nodes = match node.kind() {
        "select_statement" => direct_named_children(node)
            .into_iter()
            .filter(|child| child.kind() == "from_clause")
            .collect::<Vec<_>>(),
        "create_statement" => direct_named_children(node)
            .into_iter()
            .filter(|child| child.kind() == "create_target")
            .collect::<Vec<_>>(),
        "update_statement" | "delete_statement" => direct_named_children(node)
            .into_iter()
            .filter(|child| matches!(child.kind(), "value" | "primary_statement"))
            .collect::<Vec<_>>(),
        "upsert_statement" => direct_named_children(node)
            .into_iter()
            .filter(|child| matches!(child.kind(), "identifier" | "value"))
            .collect::<Vec<_>>(),
        "relate_statement" => direct_named_children(node)
            .into_iter()
            .filter(|child| child.kind() == "relate_subject")
            .collect::<Vec<_>>(),
        _ => vec![node],
    };

    let mut names = Vec::new();
    for relevant in relevant_nodes {
        for identifier in descendants_of_kind(relevant, "identifier")
            .into_iter()
            .chain(descendants_of_kind(relevant, "record_id").into_iter())
            .chain(descendants_of_kind(relevant, "record_id_ident").into_iter())
        {
            if let Some(name) =
                text_of(source, identifier).and_then(|value| normalize_table_name(&value))
            {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

fn collect_field_names(node: Node<'_>, source: &str) -> Vec<String> {
    let mut fields = Vec::new();
    for assignment in descendants_of_kind(node, "field_assignment") {
        if let Some(name) = direct_named_children(assignment)
            .into_iter()
            .find(|child| child.kind() == "identifier")
            .and_then(|child| text_of(source, child))
        {
            if !fields.contains(&name) {
                fields.push(name);
            }
        }
    }
    fields
}

fn infer_type_from_value(node: Node<'_>, source: &str) -> TypeExpr {
    let kind = first_named_descendant(node);
    match kind.as_ref().map(Node::kind) {
        Some("string") | Some("prefixed_string") => TypeExpr::Scalar("string".to_string()),
        Some("int") | Some("float") | Some("decimal") | Some("number") => {
            TypeExpr::Scalar("number".to_string())
        }
        Some("array") => TypeExpr::Array(Box::new(TypeExpr::Unknown)),
        Some("object") => TypeExpr::Scalar("object".to_string()),
        Some("record_id") => text_of(source, kind.unwrap())
            .and_then(|value| normalize_table_name(&value))
            .map(TypeExpr::Record)
            .unwrap_or(TypeExpr::Unknown),
        Some("keyword_true") | Some("keyword_false") => TypeExpr::Scalar("bool".to_string()),
        Some("keyword_null") | Some("keyword_none") => TypeExpr::Scalar("null".to_string()),
        _ => TypeExpr::Unknown,
    }
}

fn normalize_table_name(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches('`');
    if trimmed.is_empty() {
        return None;
    }

    let candidate = trimmed
        .split(':')
        .next()
        .unwrap_or(trimmed)
        .trim_matches(|ch| matches!(ch, '<' | '>' | '(' | ')' | '[' | ']'))
        .to_string();

    if candidate.is_empty()
        || matches!(
            candidate.as_str(),
            "FROM" | "WHERE" | "SET" | "CONTENT" | "MERGE" | "PATCH" | "ONLY"
        )
    {
        None
    } else {
        Some(candidate)
    }
}

fn identifier_from_on_table_clause(node: Node<'_>, source: &str) -> Option<String> {
    direct_named_children(node)
        .into_iter()
        .find(|child| child.kind() == "identifier")
        .and_then(|child| text_of(source, child))
}

fn extract_comment(node: Node<'_>, source: &str) -> Option<String> {
    let clause = direct_named_children(node)
        .into_iter()
        .find(|child| child.kind() == "comment_clause")
        .and_then(|child| {
            direct_named_children(child)
                .into_iter()
                .find(|item| item.kind() == "string")
        })
        .and_then(|child| text_of(source, child))
        .map(|value| unquote(&value));

    clause.or_else(|| leading_comment_text(node, source))
}

fn leading_comment_text(node: Node<'_>, source: &str) -> Option<String> {
    let start_row = node.start_position().row as usize;
    let lines = source.lines().collect::<Vec<_>>();
    if start_row == 0 || start_row > lines.len() {
        return None;
    }

    let mut comments = Vec::new();
    let mut row = start_row;
    while row > 0 {
        row -= 1;
        let trimmed = lines[row].trim();
        if trimmed.is_empty() {
            if comments.is_empty() {
                continue;
            }
            break;
        }
        let Some(comment) = trimmed
            .strip_prefix("--")
            .or_else(|| trimmed.strip_prefix("//"))
            .or_else(|| trimmed.strip_prefix('#'))
        else {
            break;
        };
        comments.push(comment.trim().to_string());
    }

    if comments.is_empty() {
        None
    } else {
        comments.reverse();
        Some(comments.join("\n"))
    }
}

fn definition_symbol(name: &str, kind: SymbolKind, source: &str, node: Node<'_>) -> DocumentSymbol {
    #[allow(deprecated)]
    DocumentSymbol {
        name: name.to_string(),
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range: byte_range_to_lsp(source, node.start_byte(), node.end_byte()),
        selection_range: byte_range_to_lsp(source, node.start_byte(), node.start_byte()),
        children: None,
    }
}

fn statement_symbol(node: Node<'_>, source: &str, uri: &Url) -> Option<DocumentSymbol> {
    let preview = node
        .utf8_text(source.as_bytes())
        .ok()
        .map(compact_preview)
        .filter(|preview| !preview.is_empty())?;
    let _ = uri;
    Some(definition_symbol(&preview, SymbolKind::EVENT, source, node))
}

fn upsert_inferred_table(
    analysis: &mut DocumentAnalysis,
    inferred: TableDef,
    source_uri: &Url,
    source: &str,
    source_node: Node<'_>,
) {
    if analysis
        .tables
        .iter()
        .any(|table| table.name == inferred.name && table.explicit)
    {
        return;
    }
    if analysis
        .tables
        .iter()
        .any(|table| table.name == inferred.name && !table.explicit)
    {
        return;
    }
    analysis.document_symbols.push(definition_symbol(
        &format!("TABLE {}", inferred.name),
        SymbolKind::STRUCT,
        source,
        source_node,
    ));
    analysis.references.push(SymbolReference {
        name: inferred.name.clone(),
        kind: SymbolKind::STRUCT,
        location: inferred.location.clone(),
        selection_range: inferred.location.range,
    });
    let _ = source_uri;
    analysis.tables.push(inferred);
}

fn location(uri: &Url, source: &str, node: Node<'_>) -> Location {
    Location::new(
        uri.clone(),
        byte_range_to_lsp(source, node.start_byte(), node.end_byte()),
    )
}

fn collect_node_diagnostics(source: &str, node: Node<'_>, diagnostics: &mut Vec<Diagnostic>) {
    if node.is_missing() {
        let range = byte_range_to_lsp(source, node.start_byte(), node.start_byte());
        diagnostics.push(Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("surreal-language-server".to_string()),
            message: format!("Missing syntax near `{}`.", node.kind()),
            ..Diagnostic::default()
        });
        return;
    }

    if node.is_error() {
        diagnostics.push(Diagnostic {
            range: byte_range_to_lsp(source, node.start_byte(), node.end_byte()),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("surreal-language-server".to_string()),
            message: syntax_error_message(source, node),
            ..Diagnostic::default()
        });
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_node_diagnostics(source, child, diagnostics);
    }
}

fn syntax_error_message(source: &str, node: Node<'_>) -> String {
    let snippet = node
        .utf8_text(source.as_bytes())
        .ok()
        .map(compact_preview)
        .unwrap_or_default();

    if snippet.is_empty() {
        "Invalid SurrealQL syntax.".to_string()
    } else {
        format!("Invalid SurrealQL syntax near `{snippet}`.")
    }
}

fn direct_named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn descendants_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut matches = Vec::new();
    collect_descendants(node, kind, &mut matches);
    matches
}

fn collect_descendants<'tree>(node: Node<'tree>, kind: &str, matches: &mut Vec<Node<'tree>>) {
    if node.kind() == kind {
        matches.push(node);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_descendants(child, kind, matches);
    }
}

fn first_named_descendant(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).next()
}

fn ancestor_kind<'tree>(mut node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    while let Some(parent) = node.parent() {
        if parent.kind() == kind {
            return Some(parent);
        }
        node = parent;
    }
    None
}

fn text_of(source: &str, node: Node<'_>) -> Option<String> {
    node.utf8_text(source.as_bytes())
        .ok()
        .map(|text| text.trim().to_string())
}

fn unquote(value: &str) -> String {
    value.trim_matches('"').trim_matches('\'').to_string()
}

fn action_label(action: QueryAction) -> &'static str {
    match action {
        QueryAction::Select => "SELECT",
        QueryAction::Create => "CREATE",
        QueryAction::Update => "UPDATE",
        QueryAction::Delete => "DELETE",
        QueryAction::Relate => "RELATE",
        QueryAction::Execute => "EXECUTE",
    }
}

#[cfg(test)]
mod tests {
    use tower_lsp::lsp_types::Url;

    use crate::semantic::types::SymbolOrigin;

    use super::analyze_document;

    #[test]
    fn indexes_define_statements_and_queries() {
        let uri = Url::parse("file:///workspace/schema.surql").expect("valid uri");
        let text = r#"
        -- Person records
        DEFINE TABLE person SCHEMAFULL PERMISSIONS FOR select WHERE $auth.roles CONTAINS 'viewer';
        DEFINE FIELD email ON TABLE person TYPE string;
        DEFINE FUNCTION fn::greet($name: string) { RETURN $name; } COMMENT "Greets" PERMISSIONS FULL;
        CREATE person CONTENT { email: "a@b.com", active: true };
        "#;

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");
        assert_eq!(analysis.tables.len(), 1);
        assert!(analysis.events.is_empty());
        assert!(analysis.indexes.is_empty());
        assert_eq!(
            analysis
                .fields
                .iter()
                .filter(|field| field.explicit)
                .count(),
            1
        );
        assert_eq!(analysis.functions.len(), 1);
        assert!(!analysis.query_facts.is_empty());
    }

    #[test]
    fn accepts_define_field_block_default_and_assert() {
        let uri = Url::parse("file:///workspace/calendar.surql").expect("valid uri");
        let text = r#"
        DEFINE FIELD OVERWRITE organization ON calendar
            TYPE option<record<organization>>
            REFERENCE ON DELETE CASCADE
            DEFAULT {
                IF type::is_record($this.owner, 'account') THEN NONE END;
                IF type::is_record($this.owner, 'team') THEN $this.owner.organization END;
                IF type::is_record($this.owner, 'organization') THEN $this.owner END;

                NONE
            }
            ASSERT {
                IF $value = NONE AND type::is_record($this.owner, 'account') THEN RETURN true END;
                IF type::is_record($this.owner, 'team') THEN RETURN $value != NONE AND $value = $this.owner.organization END;
                IF type::is_record($this.owner, 'organization') THEN RETURN $value != NONE AND $value = $this.owner END;

                THROW 'CALENDAR_INVALID_OWNER';
            };
        "#;

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");

        assert!(
            analysis.syntax_diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            analysis.syntax_diagnostics
        );
        assert_eq!(analysis.fields.len(), 1);
    }

    #[test]
    fn extracts_indexes_events_and_table_permissions() {
        let uri = Url::parse("file:///workspace/schema.surql").expect("valid uri");
        let text = r#"
        DEFINE TABLE person PERMISSIONS FOR select FULL, create WHERE $auth.roles CONTAINS 'admin';
        DEFINE EVENT audit_person ON TABLE person WHEN $before != $after THEN (CREATE event CONTENT { table: 'person' });
        DEFINE INDEX person_email ON TABLE person FIELDS email UNIQUE;
        "#;

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");

        assert_eq!(analysis.tables.len(), 1);
        assert!(!analysis.tables[0].permissions.is_empty());
        assert_eq!(analysis.events.len(), 1);
        assert_eq!(analysis.events[0].table, "person");
        assert_eq!(analysis.indexes.len(), 1);
        assert_eq!(analysis.indexes[0].table, "person");
        assert_eq!(analysis.indexes[0].fields, vec!["email".to_string()]);
        assert!(analysis.indexes[0].unique);
        assert!(analysis.indexes[0].options.is_empty());
    }

    #[test]
    fn accepts_hnsw_index_variants() {
        let uri = Url::parse("file:///workspace/vector.surql").expect("valid uri");
        let text = r#"
        DEFINE INDEX embeddings_hnsw ON TABLE embedding FIELDS vector HNSW DIMENSION 1536 DIST COSINE EFC 200 M 16;
        "#;

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");

        assert!(
            analysis.syntax_diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            analysis.syntax_diagnostics
        );
        assert_eq!(analysis.indexes.len(), 1);
        assert_eq!(analysis.indexes[0].table, "embedding");
        assert_eq!(analysis.indexes[0].fields, vec!["vector".to_string()]);
        assert!(
            analysis.indexes[0]
                .options
                .iter()
                .any(|option| option.contains("HNSW DIMENSION 1536 DIST COSINE EFC 200 M 16"))
        );
    }
}
