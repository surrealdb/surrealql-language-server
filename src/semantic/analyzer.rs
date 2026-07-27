use ls_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, DocumentSymbol, InlayHint,
    InlayHintKind, InlayHintLabel, Location, SymbolKind, Uri,
};
use tree_sitter::{Node, Parser};

use crate::grammar::language;
use crate::semantic::codes;
use crate::semantic::node_kind as k;
use crate::semantic::text::{byte_range_to_lsp, compact_preview, offset_to_position};
use crate::semantic::type_expr::TypeExpr;
use crate::semantic::types::{
    AccessDef, DocumentAnalysis, EventDef, FieldDef, FunctionDef, FunctionLanguage, FunctionParam,
    IndexDef, InferenceFact, MergedSemanticModel, NamedRange, ParamDef, PermissionMode,
    PermissionRule, QueryAction, QueryFact, SymbolOrigin, SymbolReference, TableDef,
    TargetResolution,
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
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    };

    collect_statements(root, text, &uri, origin, &mut analysis);

    // Syntax diagnostics run after extraction so the keyword-typo
    // hint can skip names that are identifiers in this document
    // (a table named `orders` must not become "Did you mean `ORDER`?").
    let known_names: std::collections::HashSet<String> = analysis
        .tables
        .iter()
        .map(|table| table.name.to_ascii_uppercase())
        .chain(
            analysis
                .fields
                .iter()
                .map(|field| field.name.to_ascii_uppercase()),
        )
        .collect();
    analysis.syntax_diagnostics =
        collect_syntax_diagnostics_at(Some(&uri), text, root, &known_names);
    Some(analysis)
}

pub fn collect_syntax_diagnostics(source: &str, node: Node<'_>) -> Vec<Diagnostic> {
    collect_syntax_diagnostics_at(None, source, node, &Default::default())
}

/// Like [`collect_syntax_diagnostics`], but able to attach
/// `relatedInformation` (which needs a document URI) when a clamped
/// error span continues beyond its first line, and to suppress
/// keyword-typo hints for `known_names` (uppercased identifiers
/// defined in the document).
pub fn collect_syntax_diagnostics_at(
    uri: Option<&Uri>,
    source: &str,
    node: Node<'_>,
    known_names: &std::collections::HashSet<String>,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    collect_node_diagnostics(uri, source, node, known_names, &mut diagnostics);
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
        collect_function_references(node, source, uri, analysis);
        return;
    }

    match kind {
        k::DEFINE_TABLE_STATEMENT => {
            extract_table(node, source, uri, origin, analysis);
            collect_function_references(node, source, uri, analysis);
            return;
        }
        k::DEFINE_FIELD_STATEMENT => {
            extract_field(node, source, uri, origin, analysis);
            collect_function_references(node, source, uri, analysis);
            return;
        }
        k::DEFINE_EVENT_STATEMENT => {
            extract_event(node, source, uri, origin, analysis);
            collect_function_references(node, source, uri, analysis);
            return;
        }
        k::DEFINE_FUNCTION_STATEMENT => {
            extract_function(node, source, uri, origin, analysis);
            collect_function_references(node, source, uri, analysis);
            return;
        }
        k::DEFINE_INDEX_STATEMENT => {
            extract_index(node, source, uri, origin, analysis);
            collect_function_references(node, source, uri, analysis);
            return;
        }
        k::DEFINE_PARAM_STATEMENT => {
            extract_param(node, source, uri, origin, analysis);
            collect_function_references(node, source, uri, analysis);
            return;
        }
        k::DEFINE_ACCESS_STATEMENT | k::DEFINE_SCOPE_STATEMENT => {
            extract_access(node, source, uri, origin, analysis);
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
        // `subquery_statement` is a transparent wrapper — never treat it
        // as a leaf statement; fall through so we descend into the real
        // statement it wraps.
        kind if (kind.ends_with("_statement") || kind.ends_with("Statement"))
            && kind != "subquery_statement" =>
        {
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
        .find(|child| {
            matches!(
                child.kind(),
                k::INCLUSIVE_PREDICATE | k::IDIOM | k::PATH | k::IDENT
            )
        })
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

    // The snake_case grammar wraps `WHEN ... THEN ...` in one node; the
    // local PascalCase grammar emits separate WhenClause/ThenClause nodes.
    let when_then = children
        .iter()
        .find(|child| child.kind() == k::WHEN_THEN_CLAUSE)
        .copied();
    let when_clause = children
        .iter()
        .find(|child| child.kind() == k::WHEN_CLAUSE)
        .copied()
        .or(when_then)
        .and_then(|clause| first_non_keyword_child(clause))
        .and_then(|child| text_of(source, child))
        .map(|text| compact_preview(&text));
    let then_clause = children
        .iter()
        .find(|child| child.kind() == k::THEN_CLAUSE)
        .copied()
        .or(when_then)
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

    // Parameters live inside a `param_list` node as `variable_name`
    // followed by an optional `type`.
    let params = children
        .iter()
        .find(|child| child.kind() == k::PARAM_LIST)
        .map(|list| parse_function_params(*list, source))
        .unwrap_or_else(|| {
            children
                .iter()
                .filter(|child| child.kind() == k::PARAM_DEFINITION)
                .flat_map(|param| parse_function_params(*param, source))
                .collect()
        });

    // `-> type` is a `returns_clause` wrapping the return `type`.
    let return_type = children
        .iter()
        .find(|child| child.kind() == k::RETURNS_CLAUSE)
        .and_then(|clause| k::find_child_any(*clause, &[k::TYPE, k::TYPE_NAME]))
        .and_then(|child| text_of(source, child))
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
    let target_nodes = target_nodes_for_statement(node, source);
    let target_refs = target_refs_from_nodes(&target_nodes, source);
    let targets: Vec<String> = target_refs.iter().map(|entry| entry.name.clone()).collect();
    let field_refs = collect_field_refs(node, source);
    let touched_fields: Vec<String> = field_refs.iter().map(|entry| entry.name.clone()).collect();
    let target_resolution = if targets.is_empty() {
        classify_unresolved_targets(&target_nodes)
    } else {
        TargetResolution::Static
    };
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
        target_refs,
        field_refs,
        target_resolution,
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
            .find(|child| {
                !matches!(
                    child.kind(),
                    k::IDENT | k::OPERATOR | k::ASSIGNMENT_OPERATOR
                )
            })
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
    descendants_of_kind(node, k::CUSTOM_FUNCTION_NAME)
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
    if node.kind() != k::CUSTOM_FUNCTION_NAME {
        return false;
    }
    node.parent()
        .is_some_and(|parent| parent.kind() == k::DEFINE_FUNCTION_STATEMENT)
}

fn infer_record_types_from_table(
    _table: &TableDef,
    _uri: &Uri,
    _source: &str,
    _node: Node<'_>,
) -> Vec<TableDef> {
    Vec::new()
}

/// Parse a `param_list` node. Parameters appear as a `variable_name`
/// optionally followed by a `type` (`$a: int, $b` → `[($a, int), ($b, _)]`).
fn parse_function_params(param_list: Node<'_>, source: &str) -> Vec<FunctionParam> {
    let children = k::named_children(param_list);
    let mut params = Vec::new();
    let mut index = 0;
    while index < children.len() {
        let child = children[index];
        if child.kind() != k::VARIABLE_NAME {
            index += 1;
            continue;
        }
        let Some(name) = text_of(source, child) else {
            index += 1;
            continue;
        };
        // A `type` immediately following the name is its declared type.
        let type_expr = children
            .get(index + 1)
            .filter(|next| next.kind() == k::TYPE || next.kind() == k::TYPE_NAME)
            .and_then(|next| text_of(source, *next))
            .map(|text| TypeExpr::parse(&text));
        params.push(FunctionParam { name, type_expr });
        index += 1;
    }
    params
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

/// Node kinds a table *name* can be read from. Expression-shaped
/// targets (`$param`, function calls, subqueries, blocks) are kept in
/// the target region for [`classify_unresolved_targets`] but excluded
/// here so e.g. `DELETE fn::pick(person)` doesn't claim `person`.
/// Arrays and type casts can wrap record ids (`FROM [person:1]`,
/// `FROM <record> person:1`) so their names are extracted.
fn is_target_name_kind(kind: &str) -> bool {
    matches!(
        kind,
        k::CREATE_TARGET
            | k::VALUE
            | k::IDENT
            | k::RECORD_ID
            | k::RECORD_ID_RANGE
            | k::PATH
            | k::ARRAY
            | k::TYPE_CAST
    ) || kind == k::RELATE_SUBJECT
}

/// The subtrees that make up a statement's target region ("what comes
/// after CREATE/UPDATE/DELETE/FROM/INTO"), before any trailing
/// clauses. Includes expression-shaped targets so classification can
/// tell a `$param` target apart from something truly opaque.
fn target_nodes_for_statement<'tree>(node: Node<'tree>, source: &str) -> Vec<Node<'tree>> {
    let children = k::named_children(node);
    let is_region_kind = |child: &Node<'tree>| {
        is_target_name_kind(child.kind())
            || matches!(
                child.kind(),
                k::VARIABLE_NAME | k::FUNCTION_CALL | k::SUB_QUERY | k::BLOCK
            )
    };
    match node.kind() {
        k::SELECT_STATEMENT => {
            // Older grammar builds wrapped the targets in a
            // `from_clause`; the current one lays them out as direct
            // children after the `FROM` keyword, up to the first
            // trailing clause (`WHERE`, `GROUP`, …).
            if let Some(from) = children.iter().find(|child| child.kind() == k::FROM_CLAUSE) {
                return k::named_children(*from)
                    .into_iter()
                    .filter(|child| !k::is_keyword(*child) && child.kind() != k::WHERE_CLAUSE)
                    .collect();
            }
            let mut after_from = false;
            let mut region = Vec::new();
            for child in &children {
                if k::is_kw(*child, source, "FROM") {
                    after_from = true;
                    continue;
                }
                if !after_from {
                    continue;
                }
                if child.kind().ends_with("Clause") {
                    break;
                }
                if is_region_kind(child) {
                    region.push(*child);
                }
            }
            region
        }
        k::CREATE_STATEMENT
        | k::UPSERT_STATEMENT
        | k::UPDATE_STATEMENT
        | k::DELETE_STATEMENT
        | k::RELATE_STATEMENT => children.iter().copied().filter(is_region_kind).collect(),
        k::INSERT_STATEMENT => {
            // The table is the identifier immediately after `INTO`.
            let mut after_into = false;
            let mut targets = Vec::new();
            for child in &children {
                if k::is_kw(*child, source, "INTO") {
                    after_into = true;
                    continue;
                }
                if after_into && child.kind() == k::IDENT {
                    targets.push(*child);
                    break;
                }
            }
            targets
        }
        _ => vec![node],
    }
}

/// Walk the candidate target subtrees for `identifier` / `record_id`
/// leaves, keeping the tight range of the first token that produced
/// each deduped table name. A `record_id`'s range is narrowed to its
/// table prefix (the text before `:`) so a quick fix can replace just
/// the table name.
fn target_refs_from_nodes(relevant_nodes: &[Node<'_>], source: &str) -> Vec<NamedRange> {
    let mut refs: Vec<NamedRange> = Vec::new();
    for relevant in relevant_nodes
        .iter()
        .filter(|node| is_target_name_kind(node.kind()))
    {
        for candidate in descendants_of_kind(*relevant, k::IDENT)
            .into_iter()
            .chain(descendants_of_kind(*relevant, k::RECORD_ID))
            .chain(descendants_of_kind(*relevant, k::RECORD_ID_RANGE))
        {
            let Some(raw) = candidate.utf8_text(source.as_bytes()).ok() else {
                continue;
            };
            let Some(name) = normalize_table_name(raw) else {
                continue;
            };
            if refs.iter().any(|existing| existing.name == name) {
                continue;
            }
            let end_byte = match raw.find(':') {
                Some(colon) => candidate.start_byte() + colon,
                None => candidate.end_byte(),
            };
            refs.push(NamedRange {
                name,
                range: byte_range_to_lsp(source, candidate.start_byte(), end_byte),
            });
        }
    }
    refs
}

/// Explain *why* no static target was found, so the "could not be
/// resolved" warning fires only for genuinely opaque targets and not
/// for `$param` / expression targets that are fine at runtime.
fn classify_unresolved_targets(relevant_nodes: &[Node<'_>]) -> TargetResolution {
    // A bare `$param` target is the strongest signal — check every
    // region node's own kind before falling back to deep scans.
    if relevant_nodes
        .iter()
        .any(|node| node.kind() == k::VARIABLE_NAME)
    {
        return TargetResolution::Parameter;
    }
    if relevant_nodes.iter().any(|node| {
        matches!(
            node.kind(),
            k::FUNCTION_CALL | k::SUB_QUERY | k::BLOCK | k::SELECT_STATEMENT | k::ARRAY
        )
    }) {
        return TargetResolution::Expression;
    }
    for relevant in relevant_nodes {
        // Fallback statements land here whole — scan their children,
        // not the node itself (a SELECT contains itself otherwise).
        for child in k::named_children(*relevant) {
            if !descendants_of_kind(child, k::VARIABLE_NAME).is_empty() {
                return TargetResolution::Parameter;
            }
            for kind in [k::FUNCTION_CALL, k::SUB_QUERY, k::BLOCK] {
                if !descendants_of_kind(child, kind).is_empty() {
                    return TargetResolution::Expression;
                }
            }
        }
    }
    TargetResolution::Unresolved
}

fn collect_field_refs(node: Node<'_>, source: &str) -> Vec<NamedRange> {
    let mut fields: Vec<NamedRange> = Vec::new();
    for assignment in descendants_of_kind(node, k::FIELD_ASSIGNMENT) {
        if let Some((name, ident)) = k::named_children(assignment)
            .into_iter()
            .find(|child| child.kind() == k::IDENT)
            .and_then(|child| text_of(source, child).map(|name| (name, child)))
            && !fields.iter().any(|existing| existing.name == name)
        {
            fields.push(NamedRange {
                name,
                range: byte_range_to_lsp(source, ident.start_byte(), ident.end_byte()),
            });
        }
    }
    fields
}

fn infer_type_from_value(node: Node<'_>, source: &str) -> TypeExpr {
    // Unwrap the transparent `value`/`base_value` layers down to the
    // concrete literal/record node.
    let mut concrete = node;
    while matches!(concrete.kind(), k::VALUE | k::BASE_VALUE) {
        match first_named_descendant(concrete) {
            Some(child) => concrete = child,
            None => break,
        }
    }
    match concrete.kind() {
        k::STRING | k::PREFIXED_STRING => TypeExpr::Scalar("string".to_string()),
        k::INT | k::FLOAT | k::DECIMAL | k::NUMBER => TypeExpr::Scalar("number".to_string()),
        k::ARRAY => TypeExpr::Array(Box::new(TypeExpr::Unknown)),
        k::OBJECT => TypeExpr::Scalar("object".to_string()),
        k::RECORD_ID => text_of(source, concrete)
            .and_then(|value| normalize_table_name(&value))
            .map(TypeExpr::Record)
            .unwrap_or(TypeExpr::Unknown),
        "Bool" | "keyword_true" | "keyword_false" => TypeExpr::Scalar("bool".to_string()),
        "None" | "keyword_none" | "keyword_null" => TypeExpr::Scalar("null".to_string()),
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

/// Upper bound on syntax diagnostics per document. A pathological
/// buffer (generated dumps, pasted binaries) shouldn't flood the
/// editor's problems panel.
const MAX_SYNTAX_DIAGNOSTICS: usize = 100;

fn collect_node_diagnostics(
    uri: Option<&Uri>,
    source: &str,
    node: Node<'_>,
    known_names: &std::collections::HashSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if diagnostics.len() >= MAX_SYNTAX_DIAGNOSTICS {
        return;
    }

    if node.is_missing() {
        diagnostics.push(missing_node_diagnostic(source, node));
        return;
    }

    if node.is_error() {
        diagnostics.push(error_node_diagnostic(uri, source, node, known_names));
        // A single typo often makes tree-sitter emit one ERROR node
        // spanning a large region that still contains more precise
        // nested MISSING/ERROR nodes — surface those too instead of
        // hiding them behind one giant squiggle.
        descend_into_error(
            uri,
            source,
            node,
            known_names,
            node.start_position().row,
            diagnostics,
        );
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_node_diagnostics(uri, source, child, known_names, diagnostics);
    }
}

/// Walk the children of a reported ERROR node. Nested errors starting
/// on the same line as the already-reported parent would only repeat
/// the same underline, so they're descended through without their own
/// diagnostic; errors on later lines get reported (the parent's range
/// was clamped to its first line).
fn descend_into_error(
    uri: Option<&Uri>,
    source: &str,
    node: Node<'_>,
    known_names: &std::collections::HashSet<String>,
    reported_row: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if diagnostics.len() >= MAX_SYNTAX_DIAGNOSTICS {
            return;
        }
        if child.is_missing() {
            diagnostics.push(missing_node_diagnostic(source, child));
            continue;
        }
        if child.is_error() {
            if child.start_position().row == reported_row {
                descend_into_error(uri, source, child, known_names, reported_row, diagnostics);
            } else {
                diagnostics.push(error_node_diagnostic(uri, source, child, known_names));
                descend_into_error(
                    uri,
                    source,
                    child,
                    known_names,
                    child.start_position().row,
                    diagnostics,
                );
            }
            continue;
        }
        collect_node_diagnostics(uri, source, child, known_names, diagnostics);
    }
}

fn missing_node_diagnostic(source: &str, node: Node<'_>) -> Diagnostic {
    // MISSING nodes are zero-width; extend the range over the next
    // character so editors render a visible squiggle. `get` instead of
    // slicing — this path must never panic on odd byte offsets.
    let start = node.start_byte();
    let end = source
        .get(start..)
        .and_then(|rest| rest.chars().next())
        .map(|ch| start + ch.len_utf8())
        .unwrap_or(start);
    Diagnostic {
        range: byte_range_to_lsp(source, start, end),
        severity: Some(DiagnosticSeverity::ERROR),
        code: codes::as_code(codes::PARSE),
        source: Some("surreal-language-server".to_string()),
        // `grammar_name` bypasses the `Keyword` alias, so a missing
        // `_kw_then` renders as "Expected `THEN`." instead of leaking
        // internal rule names.
        message: format!("Expected {}.", friendly_symbol_name(node.grammar_name())),
        ..Diagnostic::default()
    }
}

fn error_node_diagnostic(
    uri: Option<&Uri>,
    source: &str,
    node: Node<'_>,
    known_names: &std::collections::HashSet<String>,
) -> Diagnostic {
    let full_start = node.start_byte();
    let full_end = node.end_byte();
    let spans_multiple_lines = node.end_position().row > node.start_position().row;

    // One giant multi-line squiggle hides everything under it; clamp
    // the reported range to the error's first line and point at the
    // full extent through relatedInformation.
    let range_end = if spans_multiple_lines {
        source
            .get(full_start..full_end)
            .and_then(|region| region.find('\n'))
            .map(|offset| full_start + offset)
            .unwrap_or(full_end)
    } else {
        full_end
    };

    let mut message = syntax_error_message(source, node);
    if let Some(hint) =
        keyword_typo_hint(source, node, known_names).or_else(|| expected_tokens_hint(node))
    {
        message.push(' ');
        message.push_str(&hint);
    }

    let related_information = match (spans_multiple_lines, uri) {
        (true, Some(uri)) => Some(vec![DiagnosticRelatedInformation {
            location: Location::new(uri.clone(), byte_range_to_lsp(source, full_start, full_end)),
            message: format!(
                "The invalid region continues to line {}.",
                node.end_position().row + 1
            ),
        }]),
        _ => None,
    };

    Diagnostic {
        range: byte_range_to_lsp(source, full_start, range_end),
        severity: Some(DiagnosticSeverity::ERROR),
        code: codes::as_code(codes::PARSE),
        source: Some("surreal-language-server".to_string()),
        message,
        related_information,
        ..Diagnostic::default()
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

/// Keywords worth offering as "did you mean" targets. Deliberately
/// restricted to high-traffic statement/clause words: the full
/// grammar keyword list contains obscure entries (`PEARSON`,
/// `EUCLIDEAN`, …) that ordinary identifiers like `person` resemble
/// closely enough to produce absurd suggestions.
const HINTABLE_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "CREATE",
    "UPDATE",
    "UPSERT",
    "DELETE",
    "RELATE",
    "INSERT",
    "INTO",
    "DEFINE",
    "REMOVE",
    "TABLE",
    "FIELD",
    "INDEX",
    "EVENT",
    "FUNCTION",
    "PARAM",
    "ANALYZER",
    "ACCESS",
    "NAMESPACE",
    "DATABASE",
    "SCHEMAFULL",
    "SCHEMALESS",
    "PERMISSIONS",
    "TYPE",
    "VALUE",
    "VALUES",
    "DEFAULT",
    "ASSERT",
    "FLEXIBLE",
    "CONTENT",
    "MERGE",
    "PATCH",
    "RETURN",
    "GROUP",
    "ORDER",
    "LIMIT",
    "START",
    "FETCH",
    "SPLIT",
    "TIMEOUT",
    "PARALLEL",
    "BEGIN",
    "COMMIT",
    "CANCEL",
    "TRANSACTION",
    "THEN",
    "WHEN",
    "COLUMNS",
    "FIELDS",
    "UNIQUE",
    "COMMENT",
];

/// The most common cause of an ERROR node is a misspelled keyword
/// (`FRO`, `PERMISSION`, `WHRE`). Scan the error region's words for a
/// near-miss of a common keyword and suggest the fix.
///
/// The *leftmost* qualifying word wins, not the best-scoring one: the
/// parser fails at the earliest problem, so a later near-miss must
/// not outbid the actual typo.
fn keyword_typo_hint(
    source: &str,
    node: Node<'_>,
    known_names: &std::collections::HashSet<String>,
) -> Option<String> {
    let text = node.utf8_text(source.as_bytes()).ok()?;

    for word in text
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|word| word.len() >= 2)
    {
        let upper = word.to_ascii_uppercase();
        // Identifiers defined in this document are not keyword typos.
        if crate::grammar::KEYWORDS.contains(&upper.as_str()) || known_names.contains(&upper) {
            continue;
        }
        let best = HINTABLE_KEYWORDS
            .iter()
            // Never suggest a keyword the compiled grammar doesn't have.
            .filter(|keyword| crate::grammar::KEYWORDS.contains(*keyword))
            // Typos are a letter or two off; a large length gap means
            // the similarity score is prefix-bonus noise (`person` vs
            // `PERMISSIONS`).
            .filter(|keyword| keyword.len().abs_diff(upper.len()) <= 2)
            .map(|keyword| (strsim::jaro_winkler(&upper, keyword), keyword))
            .filter(|(score, _)| *score >= 0.88)
            .max_by(|left, right| {
                left.0
                    .partial_cmp(&right.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        if let Some((_, keyword)) = best {
            return Some(format!("Did you mean `{keyword}`?"));
        }
    }

    None
}

/// Derive an "Expected …" hint from the parser state after the last
/// token consumed inside the error via tree-sitter's lookahead
/// iterator. Every failure path returns `None`, degrading to exactly
/// the pre-hint message. Hints are omitted when the valid-token set
/// is large or dominated by the anonymous `Keyword` alias (the
/// grammar erases *which* keyword at the symbol level) — a vague list
/// helps nobody.
fn expected_tokens_hint(node: Node<'_>) -> Option<String> {
    const MAX_SHOWN: usize = 4;

    // ERROR nodes report parse state 0 and their prev sibling reports
    // u16::MAX; the informative state is the one after the last leaf
    // the parser managed to shift inside the error region.
    let state = last_usable_parse_state(node)?;

    let language = language();
    let mut iterator = language.lookahead_iterator(state)?;
    let mut names: Vec<String> = Vec::new();
    for name in iterator.iter_names() {
        if matches!(name, "end" | "ERROR" | "Comment" | "BlockComment")
            || (name.starts_with('_') && !name.starts_with("_kw_"))
        {
            continue;
        }
        // The keyword tokens all alias to the bare name "Keyword" —
        // there is no way to say *which* keyword, so a set containing
        // them is too vague to show.
        if name == "Keyword" {
            return None;
        }
        let friendly = friendly_symbol_name(name);
        if names.contains(&friendly) {
            continue;
        }
        names.push(friendly);
        if names.len() > MAX_SHOWN {
            return None;
        }
    }
    if names.is_empty() {
        return None;
    }

    Some(match names.len() {
        1 => format!("Expected {}.", names[0]),
        2 => format!("Expected {} or {}.", names[0], names[1]),
        _ => {
            let (last, rest) = names.split_last().expect("len >= 3");
            format!("Expected {}, or {last}.", rest.join(", "))
        }
    })
}

/// The parse state after the last leaf inside `node` that carries a
/// real state (0 = none, u16::MAX = sentinel inside error subtrees).
fn last_usable_parse_state(node: Node<'_>) -> Option<u16> {
    fn walk(node: Node<'_>, found: &mut Option<u16>) {
        if node.child_count() == 0 {
            let state = node.next_parse_state();
            if state != 0 && state != u16::MAX {
                *found = Some(state);
            }
            return;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk(child, found);
        }
    }

    let mut found = None;
    walk(node, &mut found);
    found
}

/// Render a grammar symbol name as something a SurrealQL user
/// recognises: `_kw_then` → ``` `THEN` ```, `Ident` → "an
/// identifier", punctuation passes through in backticks.
fn friendly_symbol_name(raw: &str) -> String {
    if let Some(word) = raw.strip_prefix("_kw_") {
        return format!("`{}`", word.to_ascii_uppercase());
    }
    match raw {
        k::IDENT | "RecordTbIdent" | "RecordIdIdent" | "KeyName" => "an identifier".to_string(),
        k::STRING | k::PREFIXED_STRING => "a string".to_string(),
        k::NUMBER | k::INT | k::FLOAT | k::DECIMAL => "a number".to_string(),
        k::VARIABLE_NAME => "a `$parameter`".to_string(),
        k::DURATION => "a duration".to_string(),
        k::FUNCTION_NAME => "a function name".to_string(),
        k::TYPE_NAME | k::TYPE => "a type".to_string(),
        k::OBJECT => "an object".to_string(),
        k::ARRAY => "an array".to_string(),
        k::RECORD_ID => "a record id".to_string(),
        "Keyword" => "a keyword".to_string(),
        "BraceOpen" => "`{`".to_string(),
        "BraceClose" => "`}`".to_string(),
        "Colon" => "`:`".to_string(),
        other if !other.is_empty() && other.chars().all(|ch| ch.is_ascii_punctuation()) => {
            format!("`{other}`")
        }
        other => {
            // Humanize leftover PascalCase rule names ("WhereClause" →
            // "a where clause") instead of leaking them verbatim.
            let mut words = String::new();
            for (index, ch) in other.chars().enumerate() {
                if ch.is_uppercase() && index > 0 {
                    words.push(' ');
                }
                words.push(ch.to_ascii_lowercase());
            }
            format!("a {words}")
        }
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

/// The type payload of a `type_clause` — `(keyword_type, type)` or
/// `(keyword_flexible, keyword_type, type)`. The `type` node wraps the
/// concrete `type_name`/`parameterized_type`/union, so its text is the
/// full type expression.
fn second_type_payload(clause: Node<'_>) -> Option<Node<'_>> {
    k::find_child_any(clause, &[k::TYPE, k::TYPE_NAME, k::PARAMETERIZED_TYPE])
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
