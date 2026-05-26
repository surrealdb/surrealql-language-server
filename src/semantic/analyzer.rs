use ls_types::{
    Diagnostic, DiagnosticSeverity, DocumentSymbol, InlayHint, InlayHintKind, InlayHintLabel,
    Location, NumberOrString, SymbolKind, Uri,
};
use tree_sitter::{Node, Parser};

use crate::grammar::language;
use crate::semantic::node_kind as k;
use crate::semantic::text::{byte_range_to_lsp, compact_preview, offset_to_position};
use crate::semantic::type_expr::TypeExpr;
use crate::semantic::types::{
    AccessDef, DocumentAnalysis, EventDef, FieldDef, FunctionDef, FunctionLanguage, FunctionParam,
    IndexDef, InferenceFact, MergedSemanticModel, ParamDef, PermissionMode, PermissionRule,
    QueryAction, QueryFact, SymbolOrigin, SymbolReference, TableDef,
};

pub fn analyze_document(uri: Uri, text: &str, origin: SymbolOrigin) -> Option<DocumentAnalysis> {
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
    uri: &Uri,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let kind = node.kind();

    match kind {
        k::DEFINE_STATEMENT => {
            match k::define_statement_variant(node, source).as_deref() {
                Some("TABLE") => extract_table(node, source, uri, origin, analysis),
                Some("FIELD") => extract_field(node, source, uri, origin, analysis),
                Some("EVENT") => extract_event(node, source, uri, origin, analysis),
                Some("FUNCTION") => extract_function(node, source, uri, origin, analysis),
                Some("INDEX") => extract_index(node, source, uri, origin, analysis),
                Some("PARAM") => extract_param(node, source, uri, origin, analysis),
                Some("ACCESS") | Some("SCOPE") => {
                    extract_access(node, source, uri, origin, analysis)
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
        k::SELECT_STATEMENT => {
            extract_query_fact(node, source, uri, QueryAction::Select, analysis);
            collect_function_references(node, source, uri, analysis);
            return;
        }
        k::CREATE_STATEMENT => {
            extract_query_fact(node, source, uri, QueryAction::Create, analysis);
            collect_function_references(node, source, uri, analysis);
            return;
        }
        k::UPDATE_STATEMENT | k::UPSERT_STATEMENT => {
            extract_query_fact(node, source, uri, QueryAction::Update, analysis);
            collect_function_references(node, source, uri, analysis);
            return;
        }
        k::DELETE_STATEMENT => {
            extract_query_fact(node, source, uri, QueryAction::Delete, analysis);
            collect_function_references(node, source, uri, analysis);
            return;
        }
        k::RELATE_STATEMENT => {
            extract_query_fact(node, source, uri, QueryAction::Relate, analysis);
            collect_function_references(node, source, uri, analysis);
            return;
        }
        // Other statements we still want symbol entries for.
        kind if kind.ends_with("Statement") && kind != k::SURREALQL => {
            if let Some(symbol) = statement_symbol(node, source, uri) {
                analysis.document_symbols.push(symbol);
            }
            collect_function_references(node, source, uri, analysis);
            return;
        }
        _ => {}
    }

    // Descend into containers (SurrealQL root, Block, SubQuery, etc.).
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_statements(child, source, uri, origin, analysis);
    }
}

fn extract_table(
    node: Node<'_>,
    source: &str,
    uri: &Uri,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let children = k::named_children(node);

    // Skip the leading `DEFINE`+`TABLE` keywords, the table name is the
    // next Ident.
    let Some(name) = children
        .iter()
        .find(|child| child.kind() == k::IDENT)
        .and_then(|child| text_of(source, *child))
    else {
        return;
    };

    let schema_mode = children.iter().find_map(|child| {
        if !k::is_kw(*child, source, "SCHEMAFULL") && !k::is_kw(*child, source, "SCHEMALESS") {
            return None;
        }
        let text = text_of(source, *child)?;
        Some(text.to_ascii_lowercase())
    });

    let table = TableDef {
        name: name.clone(),
        schema_mode,
        comment: extract_comment(node, source),
        permissions: children
            .iter()
            .filter(|child| child.kind() == k::PERMISSIONS_FOR_CLAUSE)
            .map(|child| parse_permission_rule(*child, source, origin, uri))
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
    uri: &Uri,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let children = k::named_children(node);

    // The field name is the `Idiom` immediately following `DEFINE FIELD`.
    // Compound field names like `address.city` parse as
    // `Idiom(Ident("address"), Ident("city"))` and become `address.city`
    // in our model.
    let Some(name) = children
        .iter()
        .find(|child| child.kind() == k::IDIOM)
        .and_then(|child| k::idiom_text(source, *child))
    else {
        return;
    };

    let table = children
        .iter()
        .find(|child| child.kind() == k::ON_TABLE_CLAUSE)
        .and_then(|child| identifier_from_on_table_clause(*child, source))
        .unwrap_or_else(|| "unknown".to_string());

    // `TypeClause(Keyword[TYPE], <type>)` — the second named child is the
    // actual type, which may be `TypeName`, `ParameterizedType`,
    // `UnionType`, or `LiteralType`.
    let type_expr = children
        .iter()
        .find(|child| child.kind() == k::TYPE_CLAUSE)
        .and_then(|clause| second_type_payload(*clause))
        .and_then(|payload| text_of(source, payload))
        .map(|text| TypeExpr::parse(&text));

    let field = FieldDef {
        table: table.clone(),
        name: name.clone(),
        type_expr: type_expr.clone(),
        comment: extract_comment(node, source),
        permissions: children
            .iter()
            .filter(|child| child.kind() == k::PERMISSIONS_FOR_CLAUSE)
            .map(|child| parse_permission_rule(*child, source, origin, uri))
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
    uri: &Uri,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let children = k::named_children(node);
    // Skip leading DEFINE/EVENT keywords, name is the next `Ident`.
    let Some(name) = children
        .iter()
        .find(|child| child.kind() == k::IDENT)
        .and_then(|child| text_of(source, *child))
    else {
        return;
    };
    let table = children
        .iter()
        .find(|child| child.kind() == k::ON_TABLE_CLAUSE)
        .and_then(|child| identifier_from_on_table_clause(*child, source))
        .unwrap_or_else(|| "unknown".to_string());

    // In the new grammar `WhenClause` and `ThenClause` are direct
    // children of `DefineStatement` (no `when_then_clause` wrapper).
    let when_clause = children
        .iter()
        .find(|child| child.kind() == k::WHEN_CLAUSE)
        .and_then(|child| text_of(source, *child))
        .map(|text| compact_preview(&text));
    let then_clause = children
        .iter()
        .find(|child| child.kind() == k::THEN_CLAUSE)
        .and_then(|child| text_of(source, *child))
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
    uri: &Uri,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let children = k::named_children(node);

    // Custom function names share the visible `FunctionName` kind with
    // builtin names. The first `FunctionName` child of `DefineStatement`
    // is always the function being defined.
    let Some(name_node) = children
        .iter()
        .find(|child| child.kind() == k::FUNCTION_NAME)
        .copied()
    else {
        return;
    };
    let Some(name) = text_of(source, name_node) else {
        return;
    };

    // Parameters are direct `ParamDefinition` children, not wrapped in a
    // `param_list` node anymore.
    let params = parse_function_params(&children, source);

    // `-> type` is encoded as a `Type` (composite) or a bare `TypeName`
    // child immediately following the parameters. The grammar emits one
    // of these whenever the source has a return-type annotation.
    let return_type = children
        .iter()
        .find(|child| child.kind() == k::TYPE || child.kind() == k::TYPE_NAME)
        .and_then(|child| text_of(source, *child))
        .map(|text| TypeExpr::parse(text.trim_start_matches("->").trim()));

    let language = detect_function_language(&children);

    let permissions = children
        .iter()
        .filter(|child| child.kind() == k::PERMISSIONS_BASIC_CLAUSE)
        .map(|child| parse_permission_rule(*child, source, origin, uri))
        .collect::<Vec<_>>();

    let body_node = children
        .iter()
        .find(|child| child.kind() == k::BLOCK)
        .copied();
    let called_functions = body_node
        .map(|body| collect_called_functions(body, source))
        .unwrap_or_default();

    let selection_range = byte_range_to_lsp(source, name_node.start_byte(), name_node.end_byte());

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

/// A function body is JavaScript when its `Block` contains any
/// `FunctionJs` descendant. The new grammar emits `FunctionJs` for
/// `function(...) { ... }` calls (replacing the old `scripting_function`
/// kind).
fn detect_function_language(children: &[Node<'_>]) -> FunctionLanguage {
    let has_js = children
        .iter()
        .find(|child| child.kind() == k::BLOCK)
        .is_some_and(|block| k::has_descendant(*block, k::FUNCTION_JS));
    if has_js {
        FunctionLanguage::JavaScript
    } else {
        FunctionLanguage::SurrealQL
    }
}

fn extract_index(
    node: Node<'_>,
    source: &str,
    uri: &Uri,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let children = k::named_children(node);
    let Some(name) = children
        .iter()
        .find(|child| child.kind() == k::IDENT)
        .and_then(|child| text_of(source, *child))
    else {
        return;
    };
    let table = children
        .iter()
        .find(|child| child.kind() == k::ON_TABLE_CLAUSE)
        .and_then(|child| identifier_from_on_table_clause(*child, source))
        .unwrap_or_else(|| "unknown".to_string());

    // FIELDS / COLUMNS each get their own `Idiom` child (one per field).
    let fields = children
        .iter()
        .find(|child| child.kind() == k::FIELDS_COLUMNS_CLAUSE)
        .map(|clause| {
            k::named_children(*clause)
                .into_iter()
                .filter(|c| c.kind() == k::IDIOM)
                .filter_map(|c| k::idiom_text(source, c))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // The `IndexClause` wraps the actual index variant. It might be a
    // `UniqueClause`, `SearchAnalyzerClause`, `MtreeClause`, or
    // `HnswClause`.
    let index_clause = children
        .iter()
        .find(|child| child.kind() == k::INDEX_CLAUSE);

    let unique = index_clause.is_some_and(|clause| {
        k::named_children(*clause)
            .iter()
            .any(|child| child.kind() == k::UNIQUE_CLAUSE)
    });

    let options = index_clause
        .map(|clause| {
            k::named_children(*clause)
                .into_iter()
                .filter(|child| {
                    matches!(
                        child.kind(),
                        k::SEARCH_ANALYZER_CLAUSE | k::MTREE_CLAUSE | k::HNSW_CLAUSE
                    )
                })
                .filter_map(|child| text_of(source, child))
                .map(|text| compact_preview(&text))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

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
    uri: &Uri,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let children = k::named_children(node);
    let Some(name) = children
        .iter()
        .find(|child| child.kind() == k::VARIABLE_NAME)
        .and_then(|child| text_of(source, *child))
    else {
        return;
    };

    // `DEFINE PARAM $x VALUE <value>` – the value follows the `VALUE`
    // keyword. Hidden value rules don't surface, so we take the last
    // named child that isn't a keyword, identifier, or permissions clause.
    let value_preview = children
        .iter()
        .rev()
        .find(|child| {
            !matches!(
                child.kind(),
                k::KEYWORD | k::VARIABLE_NAME | k::PERMISSIONS_BASIC_CLAUSE | k::COMMENT_CLAUSE
            )
        })
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
    uri: &Uri,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    // `DEFINE ACCESS x ...` parses as
    // `DefineStatement(Keyword[DEFINE], AccessDefinition(Keyword[ACCESS], Ident, ...))`.
    // Similarly `DEFINE SCOPE x ...` wraps in `ScopeDefinition`.
    let wrapper = k::named_children(node)
        .into_iter()
        .find(|child| matches!(child.kind(), k::ACCESS_DEFINITION | k::SCOPE_DEFINITION));
    let lookup_root = wrapper.unwrap_or(node);

    let Some(name) = k::named_children(lookup_root)
        .into_iter()
        .find(|child| child.kind() == k::IDENT)
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
    uri: &Uri,
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
    uri: &Uri,
    action: QueryAction,
    targets: &[String],
    touched_fields: &[String],
) -> Vec<FieldDef> {
    let mut fields = Vec::new();
    let target_table = targets
        .first()
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());

    for assignment in descendants_of_kind(node, k::FIELD_ASSIGNMENT) {
        let children = k::named_children(assignment);
        let Some(name) = children
            .iter()
            .find(|child| child.kind() == k::IDENT)
            .and_then(|child| text_of(source, *child))
        else {
            continue;
        };
        // The right-hand side of `field = value` is the last named child
        // (the hidden `_value` rule means its children appear directly
        // under the `FieldAssignment`).
        let type_expr = children
            .iter()
            .rev()
            .find(|child| !matches!(child.kind(), k::IDENT | k::OPERATOR))
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

    for object in descendants_of_kind(node, k::OBJECT) {
        let object_content = k::find_child(object, k::OBJECT_CONTENT);
        let property_parent = object_content.unwrap_or(object);
        for child in k::named_children(property_parent) {
            if child.kind() != k::OBJECT_PROPERTY {
                continue;
            }
            let property_children = k::named_children(child);
            let key_node = property_children
                .iter()
                .find(|item| item.kind() == k::OBJECT_KEY)
                .copied();
            let Some(name) = key_node.and_then(|key| {
                // ObjectKey wraps either a `KeyName` (raw identifier) or a `String`.
                let inner = k::named_children(key);
                let first = inner.first().copied().unwrap_or(key);
                text_of(source, first)
            }) else {
                continue;
            };
            // Value is the last named child of the property (after Colon).
            let value_node = property_children
                .iter()
                .rev()
                .find(|item| !matches!(item.kind(), k::OBJECT_KEY | k::COLON))
                .copied();
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
    uri: &Uri,
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
    uri: &Uri,
    analysis: &mut DocumentAnalysis,
) {
    for reference in descendants_of_kind(node, k::FUNCTION_NAME) {
        let Some(name) = text_of(source, reference) else {
            continue;
        };
        if !name.starts_with("fn::") {
            continue; // builtin function reference – not user-defined.
        }
        if is_function_being_defined(reference) {
            continue;
        }
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
    descendants_of_kind(node, k::FUNCTION_NAME)
        .into_iter()
        .filter_map(|child| text_of(source, child))
        .filter(|name| name.starts_with("fn::"))
        .collect()
}

/// True when `node` is the `FunctionName` *being defined* by a
/// `DefineStatement` — i.e. a direct named child of a
/// `DEFINE FUNCTION` statement. Call sites inside the function's
/// body live deeper in the tree (inside a `Block`/`FunctionCall`) and
/// are kept.
fn is_function_being_defined(node: Node<'_>) -> bool {
    if node.kind() != k::FUNCTION_NAME {
        return false;
    }
    node.parent()
        .is_some_and(|parent| parent.kind() == k::DEFINE_STATEMENT)
}

fn infer_record_types_from_table(
    _table: &TableDef,
    _uri: &Uri,
    _source: &str,
    _node: Node<'_>,
) -> Vec<TableDef> {
    Vec::new()
}

fn parse_function_params(children: &[Node<'_>], source: &str) -> Vec<FunctionParam> {
    let mut params = Vec::new();
    for child in children {
        if child.kind() != k::PARAM_DEFINITION {
            continue;
        }
        let inner = k::named_children(*child);
        let name = inner
            .iter()
            .find(|item| item.kind() == k::VARIABLE_NAME)
            .and_then(|item| text_of(source, *item));
        // Type is wrapped in a `Type` node when present; the inner
        // hidden `_safeType` resolves to `TypeName`, `ParameterizedType`,
        // etc.
        let type_expr = inner
            .iter()
            .find(|item| item.kind() == k::TYPE)
            .and_then(|item| text_of(source, *item))
            .map(|text| TypeExpr::parse(&text));
        if let Some(name) = name {
            params.push(FunctionParam { name, type_expr });
        }
    }
    params
}

fn parse_permission_rule(
    node: Node<'_>,
    source: &str,
    origin: SymbolOrigin,
    uri: &Uri,
) -> PermissionRule {
    // `PermissionsForClause(Keyword[PERMISSIONS], PermissionGroup+|None|Literal)`
    // `PermissionsBasicClause(Keyword[PERMISSIONS], None|Literal|WhereClause)`
    //
    // For each `PermissionGroup` (or for the simpler basic clause body)
    // we collect the explicit action keywords (SELECT/CREATE/UPDATE/
    // DELETE) and decide on the mode.
    let children = k::named_children(node);

    let groups: Vec<Node<'_>> = children
        .iter()
        .copied()
        .filter(|child| child.kind() == k::PERMISSION_GROUP)
        .collect();

    let mut actions = Vec::new();
    let scope: Vec<Node<'_>> = if groups.is_empty() {
        children
    } else {
        groups.iter().copied().flat_map(k::named_children).collect()
    };

    for child in &scope {
        if child.kind() != k::KEYWORD {
            continue;
        }
        match text_of(source, *child).as_deref() {
            Some(text) if text.eq_ignore_ascii_case("SELECT") => actions.push(QueryAction::Select),
            Some(text) if text.eq_ignore_ascii_case("CREATE") => actions.push(QueryAction::Create),
            Some(text) if text.eq_ignore_ascii_case("UPDATE") => actions.push(QueryAction::Update),
            Some(text) if text.eq_ignore_ascii_case("DELETE") => actions.push(QueryAction::Delete),
            _ => {}
        }
    }
    if actions.is_empty() {
        actions.push(QueryAction::Execute);
    }

    let mode = if scope.iter().any(|child| child.kind() == k::LITERAL) {
        PermissionMode::Full
    } else if scope.iter().any(|child| child.kind() == k::NONE) {
        PermissionMode::None
    } else {
        let expression = scope
            .iter()
            .find(|child| child.kind() == k::WHERE_CLAUSE)
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
    let children = k::named_children(node);
    // For each CRUD form we collect candidate "target value" subtrees,
    // then walk them for `Ident` / `RecordId` leaves.
    let relevant_nodes: Vec<Node<'_>> = match node.kind() {
        k::SELECT_STATEMENT => {
            // After `FROM` keyword: take all named children until a
            // clause node. The hidden value rule means `FROM <ident>`
            // shows up as `Keyword[FROM]` followed by the `Ident`.
            let mut found_from = false;
            children
                .iter()
                .copied()
                .filter(|child| {
                    if k::is_kw(*child, source, "FROM") {
                        found_from = true;
                        return false;
                    }
                    found_from && !child.kind().ends_with("Clause")
                })
                .collect()
        }
        k::CREATE_STATEMENT | k::UPDATE_STATEMENT | k::UPSERT_STATEMENT | k::DELETE_STATEMENT => {
            children
                .iter()
                .copied()
                .filter(|child| {
                    matches!(
                        child.kind(),
                        k::IDENT
                            | k::IDIOM
                            | k::RECORD_ID
                            | k::FUNCTION_CALL
                            | k::VARIABLE_NAME
                            | k::PATH
                    )
                })
                .collect()
        }
        k::RELATE_STATEMENT => children
            .iter()
            .copied()
            .filter(|child| {
                matches!(
                    child.kind(),
                    k::IDENT | k::RECORD_ID | k::FUNCTION_CALL | k::VARIABLE_NAME | k::ARRAY
                )
            })
            .collect(),
        _ => vec![node],
    };

    let mut names = Vec::new();
    for relevant in relevant_nodes {
        for identifier in descendants_of_kind(relevant, k::IDENT)
            .into_iter()
            .chain(descendants_of_kind(relevant, k::RECORD_ID))
        {
            if let Some(name) =
                text_of(source, identifier).and_then(|value| normalize_table_name(&value))
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
    }
    names
}

fn collect_field_names(node: Node<'_>, source: &str) -> Vec<String> {
    let mut fields = Vec::new();
    for assignment in descendants_of_kind(node, k::FIELD_ASSIGNMENT) {
        if let Some(name) = k::named_children(assignment)
            .into_iter()
            .find(|child| child.kind() == k::IDENT)
            .and_then(|child| text_of(source, child))
            && !fields.contains(&name)
        {
            fields.push(name);
        }
    }
    fields
}

fn infer_type_from_value(node: Node<'_>, source: &str) -> TypeExpr {
    let kind = if node.kind() == k::IDENT
        || node.kind() == k::STRING
        || node.kind() == k::NUMBER
        || node.kind() == k::ARRAY
        || node.kind() == k::OBJECT
        || node.kind() == k::RECORD_ID
        || node.kind() == k::BOOL
        || node.kind() == k::NONE
    {
        Some(node)
    } else {
        first_named_descendant(node)
    };
    match kind.as_ref().map(Node::kind) {
        Some(k::STRING) | Some(k::FORMAT_STRING) => TypeExpr::Scalar("string".to_string()),
        Some(k::INT) | Some(k::FLOAT) | Some(k::DECIMAL) | Some(k::NUMBER) => {
            TypeExpr::Scalar("number".to_string())
        }
        Some(k::ARRAY) => TypeExpr::Array(Box::new(TypeExpr::Unknown)),
        Some(k::OBJECT) => TypeExpr::Scalar("object".to_string()),
        Some(k::RECORD_ID) => text_of(source, kind.unwrap())
            .and_then(|value| normalize_table_name(&value))
            .map(TypeExpr::Record)
            .unwrap_or(TypeExpr::Unknown),
        Some(k::BOOL) => TypeExpr::Scalar("bool".to_string()),
        Some(k::NONE) => TypeExpr::Scalar("null".to_string()),
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
    // `OnTableClause(Keyword[ON], Keyword[TABLE]?, _value)`. The
    // hidden value rule means the target appears directly as `Ident`,
    // `Idiom`, etc.
    k::named_children(node)
        .into_iter()
        .filter(|child| !matches!(child.kind(), k::KEYWORD))
        .find_map(|child| match child.kind() {
            k::IDENT => text_of(source, child),
            k::IDIOM => k::idiom_text(source, child),
            _ => text_of(source, child),
        })
}

fn extract_comment(node: Node<'_>, source: &str) -> Option<String> {
    let clause = k::named_children(node)
        .into_iter()
        .find(|child| child.kind() == k::COMMENT_CLAUSE)
        .and_then(|child| k::find_child(child, k::STRING))
        .and_then(|child| text_of(source, child))
        .map(|value| unquote(&value));

    clause.or_else(|| leading_comment_text(node, source))
}

fn leading_comment_text(node: Node<'_>, source: &str) -> Option<String> {
    let start_row = node.start_position().row;
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

fn statement_symbol(node: Node<'_>, source: &str, uri: &Uri) -> Option<DocumentSymbol> {
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
    source_uri: &Uri,
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

fn location(uri: &Uri, source: &str, node: Node<'_>) -> Location {
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
            code: Some(NumberOrString::String("parse".to_string())),
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
            code: Some(NumberOrString::String("parse".to_string())),
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

fn text_of(source: &str, node: Node<'_>) -> Option<String> {
    node.utf8_text(source.as_bytes())
        .ok()
        .map(|text| text.trim().to_string())
}

/// The second meaningful child of a `TypeClause` is the actual type
/// payload. `TypeClause` is either `(Keyword[TYPE], <type>)` or
/// `(Keyword[FLEXIBLE], Keyword[TYPE], <type>)`.
fn second_type_payload(clause: Node<'_>) -> Option<Node<'_>> {
    let children = k::named_children(clause);
    children.into_iter().find(|child| {
        matches!(
            child.kind(),
            k::TYPE_NAME | k::TYPE | "ParameterizedType" | "UnionType" | "LiteralType"
        )
    })
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

/// Walk the document and emit `param_name:` inlay hints next to every
/// argument of every custom function call that overlaps the requested
/// byte range. Builtin functions are skipped because their grammar
/// definitions only carry free-form signature strings, not structured
/// parameter names we can map onto positional arguments.
pub fn collect_inlay_hints(
    source: &str,
    range_start: usize,
    range_end: usize,
    model: &MergedSemanticModel,
) -> Vec<InlayHint> {
    let mut parser = Parser::new();
    if parser.set_language(&language()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let mut hints = Vec::new();
    walk_inlay_hints(
        tree.root_node(),
        source,
        range_start,
        range_end,
        model,
        &mut hints,
    );
    hints
}

fn walk_inlay_hints(
    node: Node<'_>,
    source: &str,
    range_start: usize,
    range_end: usize,
    model: &MergedSemanticModel,
    hints: &mut Vec<InlayHint>,
) {
    if node.start_byte() > range_end || node.end_byte() < range_start {
        return;
    }

    if node.kind() == k::FUNCTION_CALL {
        let mut cursor = node.walk();
        let name_node = node
            .children(&mut cursor)
            .find(|child| child.kind() == k::FUNCTION_NAME);
        if let Some(name_node) = name_node
            && let Ok(raw) = name_node.utf8_text(source.as_bytes())
            && let Some(stripped) = raw.strip_prefix("fn::")
            && let Some(function) = model.functions.get(stripped)
        {
            let mut cursor = node.walk();
            let arg_list = node
                .children(&mut cursor)
                .find(|child| child.kind() == k::ARGUMENT_LIST);
            if let Some(arg_list) = arg_list {
                emit_argument_hints(arg_list, source, function, hints);
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_inlay_hints(child, source, range_start, range_end, model, hints);
    }
}

fn emit_argument_hints(
    arg_list: Node<'_>,
    source: &str,
    function: &FunctionDef,
    hints: &mut Vec<InlayHint>,
) {
    let mut cursor = arg_list.walk();
    let arguments: Vec<Node<'_>> = arg_list
        .children(&mut cursor)
        .filter(|child| child.is_named())
        .collect();

    for (index, argument) in arguments.iter().enumerate() {
        let Some(param) = function.params.get(index) else {
            break;
        };
        hints.push(InlayHint {
            position: offset_to_position(source, argument.start_byte()),
            label: InlayHintLabel::String(format!("{}:", param.name)),
            kind: Some(InlayHintKind::PARAMETER),
            text_edits: None,
            tooltip: None,
            padding_left: Some(false),
            padding_right: Some(true),
            data: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use ls_types::Uri;

    use crate::semantic::types::SymbolOrigin;

    use super::analyze_document;

    #[test]
    fn indexes_define_statements_and_queries() {
        let uri = Uri::from_str("file:///workspace/schema.surql").expect("valid uri");
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
        let uri = Uri::from_str("file:///workspace/calendar.surql").expect("valid uri");
        // The new grammar mirrors lezer's modern IF/ELSE form (`IF expr
        // { ... } ELSE { ... }`); the legacy `IF expr THEN val END`
        // form with bare-value branches is not part of the grammar.
        let text = r#"
        DEFINE FIELD OVERWRITE organization ON calendar
            TYPE option<record<organization>>
            REFERENCE ON DELETE CASCADE
            DEFAULT {
                IF type::is_record($this.owner, 'account') { RETURN NONE };
                IF type::is_record($this.owner, 'team') { RETURN $this.owner.organization };
                IF type::is_record($this.owner, 'organization') { RETURN $this.owner };

                RETURN NONE
            }
            ASSERT {
                IF $value = NONE AND type::is_record($this.owner, 'account') { RETURN true };
                IF type::is_record($this.owner, 'team') { RETURN $value != NONE AND $value = $this.owner.organization };
                IF type::is_record($this.owner, 'organization') { RETURN $value != NONE AND $value = $this.owner };

                THROW 'CALENDAR_INVALID_OWNER'
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
        let uri = Uri::from_str("file:///workspace/schema.surql").expect("valid uri");
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
        let uri = Uri::from_str("file:///workspace/vector.surql").expect("valid uri");
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

    #[test]
    fn accepts_hnsw_index_with_type_clause() {
        let uri = Uri::from_str("file:///workspace/vector.surql").expect("valid uri");
        let text = r#"
        DEFINE INDEX OVERWRITE documents_vec_index
            ON TABLE documents
            FIELDS embedding
            HNSW DIMENSION 4 DIST COSINE TYPE F32;
        "#;

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");

        assert!(
            analysis.syntax_diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            analysis.syntax_diagnostics
        );
        assert_eq!(analysis.indexes.len(), 1);
        assert_eq!(analysis.indexes[0].table, "documents");
        assert_eq!(analysis.indexes[0].name, "documents_vec_index");
        assert_eq!(analysis.indexes[0].fields, vec!["embedding".to_string()]);
    }

    #[test]
    fn accepts_bare_f_float_literals() {
        let uri = Uri::from_str("file:///workspace/vectors.surql").expect("valid uri");
        let text = r#"
        LET $n = 1f;
        CREATE documents CONTENT { text: "foo", embedding: [1f, 2f, 3f, 4f] };
        "#;

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");

        assert!(
            analysis.syntax_diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            analysis.syntax_diagnostics
        );
    }

    #[test]
    fn accepts_piped_create_record_id_range() {
        let uri = Uri::from_str("file:///workspace/mock.surql").expect("valid uri");
        let text = "CREATE |node:1..10|;";

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");

        assert!(
            analysis.syntax_diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            analysis.syntax_diagnostics
        );
    }

    #[test]
    fn accepts_recurse_collect_inclusive_options() {
        let uri = Uri::from_str("file:///workspace/graph.surql").expect("valid uri");
        let text = "RETURN a:1.{..+collect+inclusive}(->edge->a[?bool]);";

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");

        assert!(
            analysis.syntax_diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            analysis.syntax_diagnostics
        );
    }

    #[test]
    fn accepts_for_loop_with_unparenthesized_select() {
        let uri = Uri::from_str("file:///workspace/graph.surql").expect("valid uri");
        let text = r#"
        FOR $node IN SELECT * FROM node {
            LET $next = type::record("node", $node.id.id() + 1);
            RELATE $node->edge->$next SET read = rand::bool();
        };
        "#;

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");

        assert!(
            analysis.syntax_diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            analysis.syntax_diagnostics
        );
    }
}
