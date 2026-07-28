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
        // Shallow (ref-counted) copy; `root` keeps borrowing the local
        // `tree` for the `collect_statements` walk below.
        tree: tree.clone(),
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
    // One sweep over the whole tree rather than per-statement calls: a
    // `fn::` call can appear anywhere (a LET value, a RETURN expression,
    // an IF condition), and collecting per-statement both missed those
    // and risked double-counting once containers started descending.
    collect_function_references(root, text, &uri, &mut analysis);
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

    if kind == k::DEFINE_STATEMENT {
        match define_form(node, source).as_deref() {
            Some("table") => extract_table(node, source, uri, origin, analysis),
            Some("field") => extract_field(node, source, uri, origin, analysis),
            Some("event") => extract_event(node, source, uri, origin, analysis),
            Some("function") => extract_function(node, source, uri, origin, analysis),
            Some("index") => extract_index(node, source, uri, origin, analysis),
            Some("param") => extract_param(node, source, uri, origin, analysis),
            Some("access" | "scope") => extract_access(node, source, uri, origin, analysis),
            _ => {
                if let Some(symbol) = statement_symbol(node, source, uri) {
                    analysis.document_symbols.push(symbol);
                }
            }
        }
        // A DEFINE FUNCTION body can hold nested statements worth
        // indexing; every other DEFINE form is a leaf.
        if define_form(node, source).as_deref() == Some("function")
            && let Some(body) = k::find_child(node, k::BLOCK)
        {
            collect_statements(body, source, uri, origin, analysis);
        }
        return;
    }

    match kind {
        k::SELECT_STATEMENT => {
            extract_query_fact(node, source, uri, QueryAction::Select, analysis);
            return;
        }
        k::CREATE_STATEMENT => {
            extract_query_fact(node, source, uri, QueryAction::Create, analysis);
            return;
        }
        k::UPDATE_STATEMENT | k::UPSERT_STATEMENT => {
            extract_query_fact(node, source, uri, QueryAction::Update, analysis);
            return;
        }
        k::DELETE_STATEMENT => {
            extract_query_fact(node, source, uri, QueryAction::Delete, analysis);
            return;
        }
        k::RELATE_STATEMENT => {
            extract_query_fact(node, source, uri, QueryAction::Relate, analysis);
            return;
        }
        k::INSERT_STATEMENT => {
            extract_query_fact(node, source, uri, QueryAction::Create, analysis);
            return;
        }
        // Control-flow and binding statements are *containers*: their
        // bodies hold the statements we actually care about. Record a
        // symbol for the statement itself, then keep descending — the old
        // catch-all returned here, which made every `LET`, `FOR`, `IF` and
        // `RETURN` body invisible to the analyzer.
        k::LET_STATEMENT
        | k::FOR_STATEMENT
        | k::IF_ELSE_STATEMENT
        | k::RETURN_STATEMENT
        | k::THROW_STATEMENT => {
            if let Some(symbol) = statement_symbol(node, source, uri) {
                analysis.document_symbols.push(symbol);
            }
        }
        // Any other leaf statement (USE, INFO, KILL, …) carries nothing
        // nested that we index, so record it and stop.
        kind if kind.ends_with("Statement") => {
            if let Some(symbol) = statement_symbol(node, source, uri) {
                analysis.document_symbols.push(symbol);
            }
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

fn define_form(node: Node<'_>, source: &str) -> Option<String> {
    k::named_children(node)
        .into_iter()
        .filter(|child| k::is_keyword(*child))
        .nth(1)
        .and_then(|child| text_of(source, child))
        .map(|text| text.to_ascii_lowercase())
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

    // The field name follows `DEFINE FIELD` as an `inclusive_predicate`.
    // Compound field names like `address.city` parse as a `path` and
    // become `address.city` in our model.
    let Some(name) = children
        .iter()
        .find(|child| matches!(child.kind(), k::IDIOM | k::PATH | k::IDENT))
        .and_then(|child| k::dotted_name(source, *child))
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
        .and_then(|payload| type_expr_of(payload, source));

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

    // `WHEN ... THEN ...` arrives as two sibling clauses.
    let when_clause = children
        .iter()
        .find(|child| child.kind() == k::WHEN_CLAUSE)
        .copied()
        .and_then(first_non_keyword_child)
        .and_then(|child| text_of(source, child))
        .map(|text| compact_preview(&text));
    let then_clause = children
        .iter()
        .find(|child| child.kind() == k::THEN_CLAUSE)
        .copied()
        .and_then(|clause| k::find_child_any(clause, &[k::BLOCK, k::SUB_QUERY]))
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
    uri: &Uri,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let children = k::named_children(node);

    // User functions are named with the dedicated `custom_function_name`
    // kind (builtin calls use `builtin_function_name`). The
    // `custom_function_name` child of the define statement is the
    // function being defined.
    let Some(name_node) = children
        .iter()
        .find(|child| child.kind() == k::CUSTOM_FUNCTION_NAME)
        .copied()
    else {
        return;
    };
    let Some(name) = text_of(source, name_node) else {
        return;
    };

    // Each parameter is its own `ParamDefinition` child of the DEFINE
    // statement — the grammar has no wrapper list node.
    let params = children
        .iter()
        .filter(|child| child.kind() == k::PARAM_DEFINITION)
        .filter_map(|param| parse_function_param(*param, source))
        .collect();

    let return_type = function_return_type(&children, source);

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

    // `FIELDS`/`COLUMNS <a>, <b>` — each field is an identifier (or a
    // dotted `path` for compound fields) under the clause.
    let fields = children
        .iter()
        .find(|child| child.kind() == k::FIELDS_COLUMNS_CLAUSE)
        .map(|clause| {
            k::named_children(*clause)
                .into_iter()
                .filter(|c| !k::is_keyword(*c))
                .filter_map(|c| k::dotted_name(source, c))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // A `unique_clause` sibling marks a UNIQUE index.
    let unique = children.iter().any(|child| {
        child.kind() == k::UNIQUE_CLAUSE || k::has_descendant(*child, k::UNIQUE_CLAUSE)
    });

    // Any remaining index-variant clause (HNSW/MTREE/search analyzer/…) is
    // captured verbatim as an option string.
    let options = children
        .iter()
        .filter(|child| {
            let kind = child.kind();
            (kind.ends_with("_clause") || kind.ends_with("Clause"))
                && !matches!(
                    kind,
                    k::ON_TABLE_CLAUSE
                        | k::FIELDS_COLUMNS_CLAUSE
                        | k::UNIQUE_CLAUSE
                        | k::COMMENT_CLAUSE
                )
                && !(unique && k::has_descendant(**child, k::UNIQUE_CLAUSE))
        })
        .filter_map(|child| text_of(source, *child))
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
            !k::is_keyword(**child)
                && !matches!(
                    child.kind(),
                    k::VARIABLE_NAME | k::PERMISSIONS_BASIC_CLAUSE | k::COMMENT_CLAUSE
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
    // `DEFINE ACCESS x ...` / `DEFINE SCOPE x ...` name the access as the
    // first `identifier` child of the statement.
    let Some(name) = k::named_children(node)
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
            // Value is the last named child of the property (the key aside).
            let value_node = property_children
                .iter()
                .rev()
                .find(|item| item.kind() != k::OBJECT_KEY)
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
    for reference in descendants_of_kind(node, k::CUSTOM_FUNCTION_NAME) {
        let Some(name) = text_of(source, reference) else {
            continue;
        };
        if !name.starts_with("fn::") {
            continue; // builtin function reference – not user-defined.
        }
        if is_function_being_defined(reference, source) {
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
    descendants_of_kind(node, k::CUSTOM_FUNCTION_NAME)
        .into_iter()
        .filter_map(|child| text_of(source, child))
        .filter(|name| name.starts_with("fn::"))
        .collect()
}

/// True when `node` is the `FunctionName` *being defined* by a
/// `DEFINE FUNCTION` statement — i.e. a direct named child of a
/// `DefineStatement` whose form is `function`. Call sites inside the
/// function's body live deeper in the tree (inside a `Block`/
/// `FunctionCall`) and are kept.
///
/// `extract_function` already records the declaration as a reference, so
/// getting this wrong double-counts it: duplicate document highlights and
/// overlapping rename edits at the definition site.
fn is_function_being_defined(node: Node<'_>, source: &str) -> bool {
    if node.kind() != k::CUSTOM_FUNCTION_NAME {
        return false;
    }
    node.parent().is_some_and(|parent| {
        parent.kind() == k::DEFINE_STATEMENT
            && define_form(parent, source).as_deref() == Some("function")
    })
}

fn infer_record_types_from_table(
    _table: &TableDef,
    _uri: &Uri,
    _source: &str,
    _node: Node<'_>,
) -> Vec<TableDef> {
    Vec::new()
}

/// Parse one `ParamDefinition` node.
///
/// The grammar rule is
/// `ParamDefinition: seq($.VariableName, optional(seq($.Colon, alias($._safeType, $.Type))))`,
/// so the declared type is **not** the immediate next sibling of the
/// name — a named `Colon` node sits between them. Scanning for the first
/// type-bearing child instead of relying on adjacency is what makes this
/// work; the previous positional version silently produced `None` for
/// every annotated parameter.
fn parse_function_param(param: Node<'_>, source: &str) -> Option<FunctionParam> {
    let children = k::named_children(param);
    let name = children
        .iter()
        .find(|child| child.kind() == k::VARIABLE_NAME)
        .and_then(|child| text_of(source, *child))?;
    let type_expr = children
        .iter()
        .find(|child| k::TYPE_KINDS.contains(&child.kind()))
        .and_then(|child| type_expr_of(*child, source));
    Some(FunctionParam { name, type_expr })
}

/// Extract a function's `-> type` annotation.
///
/// The grammar splices `optional(seq($.LookupRight, $._type))` directly
/// into the DEFINE statement's children — there is no `ReturnsClause`
/// wrapper — so the return type is the first type-bearing sibling that
/// follows the `->` (`LookupRight`) token.
fn function_return_type(children: &[Node<'_>], source: &str) -> Option<TypeExpr> {
    let arrow = children
        .iter()
        .position(|child| child.kind() == k::LOOKUP_RIGHT)?;
    children
        .iter()
        .skip(arrow + 1)
        .find(|child| k::TYPE_KINDS.contains(&child.kind()))
        .and_then(|child| type_expr_of(*child, source))
}

/// Build a [`TypeExpr`] from a node that carries a type expression.
///
/// Reads the grammar's own type nodes. Going via the node's *source text*
/// and re-parsing it looks equivalent but is not: the string parser has no
/// object or tuple case, so `{ line: record<orderLine> }` would collapse to an
/// opaque `Other(_)` and take its `record<>` links with it.
fn type_expr_of(node: Node<'_>, source: &str) -> Option<TypeExpr> {
    Some(TypeExpr::from_node(node, source))
}

fn parse_permission_rule(
    node: Node<'_>,
    source: &str,
    origin: SymbolOrigin,
    uri: &Uri,
) -> PermissionRule {
    // `permissions_for_clause(keyword_permissions, keyword_for,
    //   keyword_<action>+, where_clause | keyword_full | keyword_none)`
    // `permissions_basic_clause(keyword_permissions, keyword_full |
    //   keyword_none | where_clause)`
    //
    // Actions come from the dedicated `keyword_select/create/update/delete`
    // nodes; the mode is FULL/NONE keywords or a `where_clause` expression.
    let scope = k::named_children(node);

    let mut actions = Vec::new();
    for child in &scope {
        if k::is_kw(*child, source, "select") {
            actions.push(QueryAction::Select);
        } else if k::is_kw(*child, source, "create") {
            actions.push(QueryAction::Create);
        } else if k::is_kw(*child, source, "update") {
            actions.push(QueryAction::Update);
        } else if k::is_kw(*child, source, "delete") {
            actions.push(QueryAction::Delete);
        }
    }
    if actions.is_empty() {
        actions.push(QueryAction::Execute);
    }

    let mode = if scope.iter().any(|child| k::is_kw(*child, source, "full")) {
        PermissionMode::Full
    } else if scope.iter().any(|child| k::is_kw(*child, source, "none")) {
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

/// True when `kind` names a trailing clause node. Every clause rule in
/// the grammar is suffixed `Clause` (`WhereClause`, `SetClause`,
/// `ContentClause`, `ReturnClause`, `GroupClause`, …), so the suffix is a
/// reliable "the target list ends here" marker.
fn is_clause_kind(kind: &str) -> bool {
    kind.ends_with("Clause")
}

/// The subtrees naming the tables a CRUD statement operates on.
///
/// There is no `FromClause` node in the grammar: `SelectStatement` splices
/// `Keyword(FROM) [Keyword(ONLY)] csep(value) modifier*` straight into its
/// children. So rather than looking for a wrapper, we find the keyword
/// that introduces the target list and take the value children that
/// follow it, stopping at the first trailing clause.
fn target_nodes_for_statement<'tree>(
    node: Node<'tree>,
    source: &str,
    children: &[Node<'tree>],
) -> Vec<Node<'tree>> {
    // Where the target list begins: after `FROM` for SELECT, after `INTO`
    // for INSERT, and after the leading statement keyword otherwise.
    let start = match node.kind() {
        k::SELECT_STATEMENT => children
            .iter()
            .position(|child| k::is_kw(*child, source, "FROM")),
        k::INSERT_STATEMENT => children
            .iter()
            .position(|child| k::is_kw(*child, source, "INTO")),
        _ => children.iter().position(|child| k::is_keyword(*child)),
    };
    let Some(start) = start else {
        return Vec::new();
    };

    children
        .iter()
        .skip(start + 1)
        // `ONLY` / `IGNORE` and friends sit between the introducer and the
        // first target.
        .skip_while(|child| k::is_keyword(**child))
        .take_while(|child| !is_clause_kind(child.kind()) && !k::is_keyword(**child))
        .copied()
        .collect()
}

fn target_tables_for_statement(node: Node<'_>, source: &str) -> Vec<String> {
    let children = k::named_children(node);
    let relevant_nodes = match node.kind() {
        k::SELECT_STATEMENT
        | k::CREATE_STATEMENT
        | k::UPSERT_STATEMENT
        | k::UPDATE_STATEMENT
        | k::DELETE_STATEMENT
        | k::RELATE_STATEMENT
        | k::INSERT_STATEMENT => target_nodes_for_statement(node, source, &children),
        _ => vec![node],
    };

    let mut names = Vec::new();
    for relevant in relevant_nodes {
        // A record id carries its table in the leading `RecordTbIdent`, so
        // normalising the record-id text (splitting on `:`) yields it.
        for candidate in descendants_of_kind(relevant, k::IDENT)
            .into_iter()
            .chain(descendants_of_kind(relevant, k::RECORD_ID))
            .chain(descendants_of_kind(relevant, k::RANGE_RECORD_ID))
        {
            if let Some(name) =
                text_of(source, candidate).and_then(|value| normalize_table_name(&value))
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

/// Coarse literal typing used by *schema inference* (`SET x = …`,
/// `CONTENT { … }`), which materialises `FieldDef`s.
///
/// Deliberately lossy: `Int`/`Float`/`Decimal` all collapse to `number`,
/// because an inferred field type is a guess about a column, not a claim
/// about one expression.
fn infer_type_from_value(node: Node<'_>, source: &str) -> TypeExpr {
    match node.kind() {
        k::STRING | k::FORMAT_STRING => TypeExpr::Scalar("string".to_string()),
        k::INT | k::FLOAT | k::DECIMAL | k::NUMBER => TypeExpr::Scalar("number".to_string()),
        k::ARRAY => TypeExpr::Array(Box::new(TypeExpr::Unknown)),
        k::OBJECT => TypeExpr::Scalar("object".to_string()),
        k::DURATION => TypeExpr::Scalar("duration".to_string()),
        k::RECORD_ID => text_of(source, node)
            .and_then(|value| normalize_table_name(&value))
            .map(|table| TypeExpr::Record(vec![table]))
            .unwrap_or(TypeExpr::Unknown),
        k::BOOL => TypeExpr::Scalar("bool".to_string()),
        k::NONE => TypeExpr::Scalar("null".to_string()),
        _ => TypeExpr::Unknown,
    }
}

fn normalize_table_name(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches(|ch| matches!(ch, '`' | '|'));
    if trimmed.is_empty() {
        return None;
    }

    let candidate = trimmed
        .split(':')
        .next()
        .unwrap_or(trimmed)
        .trim_matches(|ch| matches!(ch, '<' | '>' | '(' | ')' | '[' | ']' | '|'))
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
    // `on_table_clause(keyword_on, keyword_table?, identifier)`. The
    // table target is the first non-keyword child.
    k::named_children(node)
        .into_iter()
        .find(|child| !k::is_keyword(*child))
        .and_then(|child| match child.kind() {
            k::IDENT => text_of(source, child),
            _ => k::dotted_name(source, child).or_else(|| text_of(source, child)),
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

fn first_non_keyword_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| !k::is_keyword(*child))
}

fn text_of(source: &str, node: Node<'_>) -> Option<String> {
    node.utf8_text(source.as_bytes())
        .ok()
        .map(|text| text.trim().to_string())
}

/// The type payload of a `TypeClause` — `(Keyword[TYPE], <type>)` or
/// `(Keyword[FLEXIBLE], Keyword[TYPE], <type>)`.
///
/// The payload is whichever type-bearing kind the grammar chose, which
/// includes `UnionType` (`TYPE string | int`) and `LiteralType`
/// (`TYPE [string, string]`, `TYPE 'a' | 'b'`, `TYPE { a: int }`).
/// Matching only the scalar kinds dropped those on the floor.
fn second_type_payload(clause: Node<'_>) -> Option<Node<'_>> {
    k::find_child_any(clause, k::TYPE_KINDS)
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
    root: Node<'_>,
    source: &str,
    range_start: usize,
    range_end: usize,
    model: &MergedSemanticModel,
) -> Vec<InlayHint> {
    let mut hints = Vec::new();
    walk_inlay_hints(root, source, range_start, range_end, model, &mut hints);
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
            .find(|child| child.kind() == k::CUSTOM_FUNCTION_NAME);
        // `model.functions` is keyed by the *full* name including the
        // `fn::` prefix (see `merge_function`), so look up the raw text.
        // Stripping the prefix first meant this never matched and custom
        // function inlay hints never appeared.
        if let Some(name_node) = name_node
            && let Ok(raw) = name_node.utf8_text(source.as_bytes())
            && raw.starts_with("fn::")
            && let Some(function) = model.functions.get(raw.trim())
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

    use crate::semantic::type_expr::TypeExpr;
    use crate::semantic::types::{DocumentAnalysis, QueryAction, SymbolOrigin};

    use super::analyze_document;

    fn analyze(text: &str) -> DocumentAnalysis {
        let uri = Uri::from_str("file:///workspace/test.surql").expect("valid uri");
        analyze_document(uri, text, SymbolOrigin::Local).expect("analysis")
    }

    fn scalar(name: &str) -> TypeExpr {
        TypeExpr::Scalar(name.to_string())
    }

    #[test]
    fn extracts_declared_function_parameter_types() {
        // `ParamDefinition` puts a named `Colon` between the name and the
        // type, so positional adjacency never matched and every annotated
        // parameter came back untyped.
        let analysis = analyze(
            r#"DEFINE FUNCTION fn::greet($name: string, $times: int, $extra) {
                 RETURN $name;
               };"#,
        );

        let function = analysis.functions.first().expect("one function");
        let params: Vec<_> = function
            .params
            .iter()
            .map(|param| (param.name.as_str(), param.type_expr.clone()))
            .collect();

        assert_eq!(
            params,
            vec![
                ("$name", Some(scalar("string"))),
                ("$times", Some(scalar("int"))),
                ("$extra", None),
            ]
        );
    }

    #[test]
    fn extracts_structured_function_parameter_types() {
        // A record union and an array of records.
        let analysis = analyze(
            r#"DEFINE FUNCTION fn::add($id: record<orderData | project>, $lines: array<record<orderLine>>) {
                 RETURN $id;
               };"#,
        );

        let function = analysis.functions.first().expect("one function");
        assert_eq!(
            function.params[0].type_expr,
            Some(TypeExpr::Record(vec![
                "orderData".to_string(),
                "project".to_string()
            ]))
        );
        assert_eq!(
            function.params[1].type_expr,
            Some(TypeExpr::Array(Box::new(TypeExpr::Record(vec![
                "orderLine".to_string()
            ]))))
        );
    }

    #[test]
    fn extracts_inline_object_parameter_type() {
        // Reading the type node's *source text* and re-parsing it loses
        // this entirely: the string parser has no object case, so the
        // whole `{ … }` lands in `Other(_)` and the `record<>` links
        // inside it disappear.
        let analysis = analyze(
            r#"DEFINE FUNCTION fn::add($user: record<user>, $doc: {
                 line: record<orderLine>,
                 asset: record<asset>
               }) {
                 RETURN $doc;
               };"#,
        );

        let function = analysis.functions.first().expect("one function");
        assert_eq!(
            function.params[1].type_expr,
            Some(TypeExpr::Object(vec![
                (
                    "line".to_string(),
                    TypeExpr::Record(vec!["orderLine".to_string()])
                ),
                (
                    "asset".to_string(),
                    TypeExpr::Record(vec!["asset".to_string()])
                ),
            ]))
        );
        // The nested record links must survive, or implicit table
        // registration and record-type navigation lose them.
        assert_eq!(
            function.params[1]
                .type_expr
                .as_ref()
                .expect("typed")
                .record_tables(),
            vec!["orderLine".to_string(), "asset".to_string()]
        );
    }

    #[test]
    fn extracts_tuple_and_literal_union_field_types() {
        let analysis = analyze(
            r#"DEFINE FIELD id ON task TYPE [string, string];
               DEFINE FIELD status ON task TYPE 'open' | 'done';"#,
        );

        let id = analysis
            .fields
            .iter()
            .find(|field| field.name == "id")
            .expect("id field");
        assert_eq!(
            id.type_expr,
            Some(TypeExpr::Tuple(vec![scalar("string"), scalar("string")]))
        );

        let status = analysis
            .fields
            .iter()
            .find(|field| field.name == "status")
            .expect("status field");
        assert_eq!(
            status.type_expr,
            Some(TypeExpr::Union(vec![
                TypeExpr::Literal("'open'".to_string()),
                TypeExpr::Literal("'done'".to_string()),
            ]))
        );
        // Hover renders this verbatim, so the quotes must round-trip.
        assert_eq!(
            status.type_expr.as_ref().unwrap().to_string(),
            "'open' | 'done'"
        );
    }

    #[test]
    fn normalizes_none_union_into_option() {
        // Remote `INFO FOR DB` spells optionals as `none | string`; local
        // schemas write `option<string>`. They must compare equal.
        let analysis = analyze(
            r#"DEFINE FIELD a ON t TYPE option<string>;
               DEFINE FIELD b ON t TYPE none | string;"#,
        );

        let of = |name: &str| {
            analysis
                .fields
                .iter()
                .find(|field| field.name == name)
                .and_then(|field| field.type_expr.clone())
        };
        assert_eq!(of("a"), of("b"));
        assert_eq!(of("a"), Some(TypeExpr::Option(Box::new(scalar("string")))));
    }

    #[test]
    fn extracts_function_return_type() {
        // `-> type` is spliced straight into the DEFINE statement as
        // `LookupRight` + a type node; there is no `ReturnsClause` wrapper.
        let analysis = analyze(
            r#"DEFINE FUNCTION fn::slug($input: string) -> string {
                 RETURN string::slug($input);
               };"#,
        );

        let function = analysis.functions.first().expect("one function");
        assert_eq!(function.return_type, Some(scalar("string")));
    }

    #[test]
    fn record_union_type_does_not_register_a_phantom_table() {
        // `record<a | b>` used to parse as one table literally named
        // "a | b", which the implicit-table pass then registered.
        let analysis = analyze("DEFINE FIELD owner ON job TYPE record<person | company>;");

        let mut tables: Vec<_> = analysis.tables.iter().map(|t| t.name.as_str()).collect();
        tables.sort_unstable();
        assert_eq!(tables, vec!["company", "person"]);
    }

    #[test]
    fn extracts_union_and_literal_field_types() {
        // `second_type_payload` matched only the scalar type kinds, so
        // `UnionType` and `LiteralType` payloads were dropped entirely.
        let analysis = analyze(
            r#"DEFINE FIELD status ON task TYPE 'open' | 'done';
               DEFINE FIELD id ON task TYPE [string, string];
               DEFINE FIELD score ON task TYPE string | int;"#,
        );

        for name in ["status", "id", "score"] {
            let field = analysis
                .fields
                .iter()
                .find(|field| field.name == name)
                .unwrap_or_else(|| panic!("field {name}"));
            assert!(
                field.type_expr.is_some(),
                "field `{name}` should carry a type, got None"
            );
        }
    }

    #[test]
    fn resolves_select_target_tables() {
        // There is no `FromClause` node; targets follow the FROM keyword.
        for (text, expected) in [
            ("SELECT * FROM person;", vec!["person"]),
            (
                "SELECT name FROM person, company;",
                vec!["person", "company"],
            ),
            ("SELECT * FROM ONLY person:tobie;", vec!["person"]),
            ("SELECT * FROM person WHERE name = 'x';", vec!["person"]),
            ("SELECT name AS n FROM person LIMIT 1;", vec!["person"]),
        ] {
            let analysis = analyze(text);
            let fact = analysis
                .query_facts
                .iter()
                .find(|fact| fact.action == QueryAction::Select)
                .unwrap_or_else(|| panic!("select fact for {text}"));
            assert_eq!(fact.target_tables, expected, "targets for {text}");
            assert!(!fact.dynamic, "{text} should not be dynamic");
        }
    }

    #[test]
    fn descends_into_let_and_control_flow_bodies() {
        // The old catch-all returned without descending, so statements
        // nested in LET / FOR / IF bodies were invisible.
        let analysis = analyze(
            r#"LET $people = SELECT * FROM person;
               FOR $p IN $people {
                 UPDATE company SET seen = true;
               };
               IF $people { DELETE audit; };"#,
        );

        let actions: Vec<_> = analysis
            .query_facts
            .iter()
            .map(|fact| (fact.action, fact.target_tables.clone()))
            .collect();

        assert!(
            actions.contains(&(QueryAction::Select, vec!["person".to_string()])),
            "SELECT inside LET missing, got {actions:?}"
        );
        assert!(
            actions.contains(&(QueryAction::Update, vec!["company".to_string()])),
            "UPDATE inside FOR missing, got {actions:?}"
        );
        assert!(
            actions.contains(&(QueryAction::Delete, vec!["audit".to_string()])),
            "DELETE inside IF missing, got {actions:?}"
        );
    }

    #[test]
    fn records_insert_statements_as_query_facts() {
        let analysis = analyze("INSERT INTO person { name: 'Tobie' };");

        let fact = analysis.query_facts.first().expect("insert fact");
        assert_eq!(fact.action, QueryAction::Create);
        assert_eq!(fact.target_tables, vec!["person".to_string()]);
    }

    #[test]
    fn function_declaration_is_not_double_counted_as_a_reference() {
        // `extract_function` already records the declaration; the
        // reference sweep must skip it or rename emits overlapping edits.
        let analysis = analyze(
            r#"DEFINE FUNCTION fn::greet($name: string) { RETURN $name; };
               RETURN fn::greet('x');"#,
        );

        let declarations = analysis
            .references
            .iter()
            .filter(|reference| reference.name == "fn::greet")
            .count();
        assert_eq!(
            declarations, 2,
            "expected one declaration + one call site, got {declarations}"
        );
    }

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
    fn accepts_range_create_record_id() {
        let uri = Uri::from_str("file:///workspace/mock.surql").expect("valid uri");
        let text = "CREATE |node:1..10|;";

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");

        assert!(
            analysis.syntax_diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            analysis.syntax_diagnostics
        );
        assert_eq!(analysis.query_facts.len(), 1);
        assert_eq!(
            analysis.query_facts[0].target_tables,
            vec!["node".to_string()]
        );
        assert!(
            !analysis.query_facts[0].dynamic,
            "range record id target should resolve statically"
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
