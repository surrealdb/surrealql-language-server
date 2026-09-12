use ls_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, DocumentSymbol, InlayHint,
    InlayHintKind, InlayHintLabel, Location, SymbolKind, Uri,
};
use tree_sitter::{Node, Parser, Tree};

use crate::grammar::language;
use crate::semantic::codes;
use crate::semantic::limits;
use crate::semantic::node_kind as k;
use crate::semantic::text::{LineIndex, compact_preview};
use crate::semantic::type_expr::TypeExpr;
use crate::semantic::type_name;
use crate::semantic::types::{
    AccessDef, AnalyzerDef, DocumentAnalysis, EdgeObservation, EventDef, FieldDef, FunctionDef,
    FunctionLanguage, FunctionParam, IndexDef, InferenceFact, MergedSemanticModel, NamedRange,
    ParamDef, PermissionMode, PermissionRule, QueryAction, QueryFact, RelationDef, SymbolOrigin,
    SymbolReference, TableDef, TargetResolution,
};

pub fn analyze_document(uri: Uri, text: &str, origin: SymbolOrigin) -> Option<DocumentAnalysis> {
    analyze_document_with_limit(uri, text, origin, DEFAULT_MAX_SYNTAX_DIAGNOSTICS)
}

/// [`analyze_document`], with the per-document syntax-diagnostic cap supplied
/// by the caller so `analysis.maxSyntaxDiagnostics` can drive it. `0` means no
/// cap. Everything else about the analysis is identical — the limit only
/// bounds how many `parse`/`unknown-type` diagnostics the walk collects.
pub fn analyze_document_with_limit(
    uri: Uri,
    text: impl Into<String>,
    origin: SymbolOrigin,
    limit: usize,
) -> Option<DocumentAnalysis> {
    analyze_document_bounded(
        uri,
        text,
        origin,
        limit,
        crate::config::DEFAULT_MAX_DOCUMENT_BYTES,
    )
}

/// [`analyze_document_with_limit`], with the document size cap supplied by the
/// caller so `analysis.maxDocumentBytes` can drive it. `0` removes the cap.
///
/// A document over the cap is **still returned** (with its text, its line
/// index, and one informational diagnostic explaining the silence), because the
/// server's record of an open buffer is not optional. Only the analysis is
/// skipped.
pub fn analyze_document_bounded(
    uri: Uri,
    text: impl Into<String>,
    origin: SymbolOrigin,
    limit: usize,
    max_bytes: usize,
) -> Option<DocumentAnalysis> {
    analyze_document_incremental(uri, text, origin, limit, max_bytes, None)
}

/// [`analyze_document_bounded`], reusing `old_tree` for the parse.
///
/// `old_tree` must be the tree of the *previous* text with a
/// [`tree_sitter::Tree::edit`] applied for every change since, which is what
/// `OpenBuffer::pending_tree` maintains. Reparsing against it costs 0.77 ms on a
/// 3,200-line document where a fresh parse costs 16.4 ms.
///
/// Passing a tree of unrelated text is not unsafe, but it is slower than `None`
/// and produces a tree that is merely *a* parse of the new text rather than the
/// one a fresh parse gives, so every caller that cannot maintain the invariant
/// passes `None` instead. Checked over SurrealDB's 1,897-file corpus with ten
/// random edits each: the incremental tree matched a fresh parse in every case,
/// including the files whose final text contains ERROR nodes.
pub fn analyze_document_incremental(
    uri: Uri,
    text: impl Into<String>,
    origin: SymbolOrigin,
    limit: usize,
    max_bytes: usize,
    old_tree: Option<&Tree>,
) -> Option<DocumentAnalysis> {
    // Owned once. The document text used to be copied into the analysis with
    // `to_string()` even though every caller already owns a `String` and drops
    // it — a whole-document memcpy per keystroke. It is moved in at the end
    // instead, after the walks are done borrowing it.
    let owned_text: String = text.into();
    let text: &str = &owned_text;

    // Checked before the parser runs, because the tree-depth guard further down
    // is too late for the very worst input: tree-sitter frees a tree by
    // recursing through it, so a document deep enough overflows the stack in
    // tree-sitter's own `Drop`: after every walk of ours has correctly
    // declined it. Counting brackets in the text is one linear pass and lets
    // such a document be refused without ever building the tree.
    let mut parser = Parser::new();
    parser.set_language(&language()).ok()?;

    // Too large to be worth analysing. The workspace walk has skipped oversize
    // files since 0.3, but what an editor *pushes* was never bounded, so the
    // widest input door was the one nothing guarded.
    if max_bytes > 0 && text.len() > max_bytes {
        let tree = parser.parse("", None)?;
        let line_index = LineIndex::new(text);
        let mut analysis = blank_analysis(uri, tree);
        analysis.syntax_diagnostics =
            vec![too_large_diagnostic(&line_index, text.len(), max_bytes)];
        analysis.line_index = line_index;
        analysis.text = owned_text;
        return Some(analysis);
    }

    if limits::too_deep(limits::max_bracket_depth(text)) {
        // Parse an empty document instead of this one. The analysis still needs
        // a `Tree` (request handlers read it unconditionally), and an empty one
        // is a single node that costs nothing to build or to free.
        let tree = parser.parse("", None)?;
        let line_index = LineIndex::new(text);
        let mut analysis = blank_analysis(uri, tree);
        analysis.syntax_diagnostics = vec![too_deeply_nested_text_diagnostic(&line_index)];
        analysis.line_index = line_index;
        analysis.text = owned_text;
        return Some(analysis);
    }

    let tree = parser.parse(text, old_tree)?;
    let root = tree.root_node();

    // Built once, before the walk. Every range the walk records goes through
    // this index, which is what keeps the walk linear in the document size.
    let line_index = LineIndex::new(text);
    // A statement cannot be shorter than a line, so the line count is a sound
    // upper bound and a good first guess. Without it a 3200-statement document
    // grows each of these through a dozen reallocations.
    let statement_hint = line_index.line_count();

    let mut analysis = DocumentAnalysis {
        uri: uri.clone(),
        // Both replaced below, once the walks stop borrowing them: they need
        // `text` and `line_index` by reference while holding `&mut analysis`.
        text: String::new(),
        // Shallow (ref-counted) copy; `root` keeps borrowing the local
        // `tree` for the `collect_statements` walk below.
        tree: tree.clone(),
        line_index: LineIndex::default(),
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: Vec::new(),
        params: Vec::new(),
        accesses: Vec::new(),
        analyzers: Vec::new(),
        query_facts: Vec::with_capacity(statement_hint),
        // Most documents hold no RELATE at all, so this one starts empty
        // rather than at the statement hint.
        edge_observations: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::with_capacity(statement_hint),
    };

    // Bound the whole analysis in one place rather than guarding each of the
    // forty-odd recursive walks that read this tree.
    //
    // Tree-sitter's parser is iterative, so it builds a tree as deep as the text
    // asks for; almost everything that *reads* that tree descends it by
    // recursion, and `panic = 'abort'` turns the first walk to run out of stack
    // into a dead process that takes every open document with it. Measured on
    // the real binary before this guard: a `didOpen` carrying `RETURN` and six
    // thousand nested parentheses (a 12 KB file) aborted a worker thread.
    //
    // Rejecting once, here, is what makes every walk below provably bounded. A
    // document this deep is not one SurrealDB would run either, so the honest
    // answer is a syntax error and no extracted facts. See `semantic::limits`
    // for where the number comes from.
    if limits::too_deep(limits::tree_depth(root, limits::MAX_NODE_DEPTH)) {
        analysis.syntax_diagnostics = vec![too_deeply_nested_diagnostic(text, &line_index, root)];
        analysis.line_index = line_index;
        analysis.text = owned_text;
        return Some(analysis);
    }

    collect_statements(root, text, &line_index, &uri, origin, 0, &mut analysis);
    // One sweep over the whole tree rather than per-statement calls: a
    // `fn::` call can appear anywhere (a LET value, a RETURN expression,
    // an IF condition), and collecting per-statement both missed those
    // and risked double-counting once containers started descending.
    collect_function_references(root, text, &line_index, &uri, &mut analysis);

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
    analysis.syntax_diagnostics = collect_syntax_diagnostics_at(
        Some(&uri),
        text,
        &line_index,
        root,
        &known_names,
        syntax_diagnostic_limit(limit),
    );
    analysis.line_index = line_index;
    analysis.text = owned_text;
    Some(analysis)
}

pub fn collect_syntax_diagnostics(source: &str, node: Node<'_>) -> Vec<Diagnostic> {
    // A one-shot entry point with no cached analysis, so it builds its own
    // index. That is one linear pass, not one per diagnostic.
    collect_syntax_diagnostics_at(
        None,
        source,
        &LineIndex::new(source),
        node,
        &Default::default(),
        DEFAULT_MAX_SYNTAX_DIAGNOSTICS,
    )
}

/// Like [`collect_syntax_diagnostics`], but able to attach
/// `relatedInformation` (which needs a document URI) when a clamped
/// error span continues beyond its first line, to suppress
/// keyword-typo hints for `known_names` (uppercased identifiers
/// defined in the document), and to take the per-document cap.
///
/// `limit` is the resolved cap, so callers that accept the `0`-means-no-cap
/// setting must pass it through [`syntax_diagnostic_limit`] first.
pub fn collect_syntax_diagnostics_at(
    uri: Option<&Uri>,
    source: &str,
    lines: &LineIndex,
    node: Node<'_>,
    known_names: &std::collections::HashSet<String>,
    limit: usize,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    // One slot per line, marked as `parse` diagnostics are pushed.
    let mut parse_rows = vec![false; lines.line_count()];
    let walk = DiagnosticWalk {
        uri,
        source,
        lines,
        known_names,
        limit,
    };
    collect_node_diagnostics(&walk, node, 0, &mut parse_rows, &mut diagnostics);
    diagnostics
}

fn collect_statements(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
    uri: &Uri,
    origin: SymbolOrigin,
    depth: u32,
    analysis: &mut DocumentAnalysis,
) {
    // The extraction walk descends every container, so a document nested past
    // anything SurrealDB would parse can run the stack out here. Stopping costs
    // the definitions inside that subtree; the syntax pass still reports the
    // nesting itself. See `semantic::limits`.
    if limits::too_deep(depth) {
        return;
    }

    let kind = node.kind();

    if kind == k::DEFINE_STATEMENT {
        match define_form(node, source).as_deref() {
            Some("table") => extract_table(node, source, lines, uri, origin, analysis),
            Some("field") => extract_field(node, source, lines, uri, origin, analysis),
            Some("event") => extract_event(node, source, lines, uri, origin, analysis),
            Some("function") => extract_function(node, source, lines, uri, origin, analysis),
            Some("index") => extract_index(node, source, lines, uri, origin, analysis),
            Some("param") => extract_param(node, source, lines, uri, origin, analysis),
            Some("access" | "scope") => extract_access(node, source, lines, uri, origin, analysis),
            Some("analyzer") => extract_analyzer(node, source, lines, uri, origin, analysis),
            _ => {
                if let Some(symbol) = statement_symbol(node, source, lines, uri) {
                    analysis.document_symbols.push(symbol);
                }
            }
        }
        // A DEFINE FUNCTION body can hold nested statements worth
        // indexing; every other DEFINE form is a leaf.
        if define_form(node, source).as_deref() == Some("function")
            && let Some(body) = k::find_child(node, k::BLOCK)
        {
            collect_statements(body, source, lines, uri, origin, depth + 1, analysis);
        }
        return;
    }

    match kind {
        k::SELECT_STATEMENT => {
            extract_query_fact(node, source, lines, uri, QueryAction::Select, analysis);
            return;
        }
        k::CREATE_STATEMENT => {
            extract_query_fact(node, source, lines, uri, QueryAction::Create, analysis);
            return;
        }
        k::UPDATE_STATEMENT | k::UPSERT_STATEMENT => {
            extract_query_fact(node, source, lines, uri, QueryAction::Update, analysis);
            return;
        }
        k::DELETE_STATEMENT => {
            extract_query_fact(node, source, lines, uri, QueryAction::Delete, analysis);
            return;
        }
        k::RELATE_STATEMENT => {
            extract_query_fact(node, source, lines, uri, QueryAction::Relate, analysis);
            return;
        }
        k::INSERT_STATEMENT => {
            extract_query_fact(node, source, lines, uri, QueryAction::Create, analysis);
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
            if let Some(symbol) = statement_symbol(node, source, lines, uri) {
                analysis.document_symbols.push(symbol);
            }
        }
        // Any other leaf statement (USE, INFO, KILL, …) carries nothing
        // nested that we index, so record it and stop.
        kind if kind.ends_with("Statement") => {
            if let Some(symbol) = statement_symbol(node, source, lines, uri) {
                analysis.document_symbols.push(symbol);
            }
            return;
        }
        _ => {}
    }

    // Descend into containers (SurrealQL root, Block, SubQuery, etc.).
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_statements(child, source, lines, uri, origin, depth + 1, analysis);
    }
}

/// Which `DEFINE` this is — `table`, `field`, `access`, and so on.
///
/// Normally the sub-form keyword is the second direct keyword child. `ACCESS`
/// and `SCOPE` are the exceptions: the grammar wraps each one's keyword and name
/// in an `AccessDefinition` / `ScopeDefinition` node
/// (`grammar.js:520`), so the `DefineStatement` has only *one* direct keyword
/// child and the second-keyword lookup returned `None`.
///
/// That made the `Some("access" | "scope")` arm unreachable, so `extract_access`
/// never ran and no `DEFINE ACCESS` was ever indexed. Look inside the wrapper.
pub(crate) fn define_form(node: Node<'_>, source: &str) -> Option<String> {
    let children = k::named_children(node);
    if let Some(wrapper) = children
        .iter()
        .find(|child| matches!(child.kind(), k::ACCESS_DEFINITION | k::SCOPE_DEFINITION))
        && let Some(keyword) = k::named_children(*wrapper)
            .into_iter()
            .find(|child| k::is_keyword(*child))
        && let Some(text) = text_of(source, keyword)
    {
        return Some(text.to_ascii_lowercase());
    }
    children
        .into_iter()
        .filter(|child| k::is_keyword(*child))
        .nth(1)
        .and_then(|child| text_of(source, child))
        .map(|text| text.to_ascii_lowercase())
}

fn extract_table(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
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
        comment: extract_comment(node, source, lines),
        permissions: children
            .iter()
            .filter(|child| child.kind() == k::PERMISSIONS_FOR_CLAUSE)
            .map(|child| parse_permission_rule(*child, source, lines, origin, uri))
            .collect(),
        origin,
        explicit: true,
        inference: None,
        location: location(uri, source, lines, node),
        relation: children
            .iter()
            .find(|child| child.kind() == k::TABLE_TYPE_CLAUSE)
            .and_then(|clause| parse_relation_clause(*clause, source)),
    };

    for inferred in infer_record_types_from_table(&table, uri, source, node) {
        upsert_inferred_table(analysis, inferred, uri, source, lines, node);
    }

    analysis.document_symbols.push(definition_symbol(
        &format!("TABLE {name}"),
        SymbolKind::STRUCT,
        source,
        lines,
        node,
    ));
    analysis.tables.push(table);
}

/// Read `TYPE RELATION IN a|b OUT c|d ENFORCED` out of a `TableTypeClause`.
///
/// Returns `None` for `TYPE NORMAL`, `TYPE ANY`, and anything else that is not
/// a relation — the caller stores that as "this table is not an edge".
///
/// The clause exposes no tree-sitter fields: `RELATION`, `IN`, `FROM`, `OUT`
/// and `TO` all arrive as generic `Keyword` nodes, and both table lists as bare
/// `Ident`s. So this walks the children in source order and lets each keyword
/// select which list the following identifiers belong to. `IN`/`FROM` and
/// `OUT`/`TO` are the two accepted spellings of each side.
fn parse_relation_clause(clause: Node<'_>, source: &str) -> Option<RelationDef> {
    /// Which side of the relation the walk is currently filling.
    enum Side {
        /// Before any `IN`/`OUT` keyword — a bare `TYPE RELATION`.
        Neither,
        In,
        Out,
    }

    let mut is_relation = false;
    let mut side = Side::Neither;
    let mut relation = RelationDef::default();

    for child in k::named_children(clause) {
        if child.kind() == k::ENFORCED_CLAUSE {
            relation.enforced = true;
            continue;
        }
        if k::is_keyword(child) {
            match text_of(source, child)
                .map(|text| text.to_ascii_uppercase())
                .as_deref()
            {
                Some("RELATION") => is_relation = true,
                Some("IN" | "FROM") => side = Side::In,
                Some("OUT" | "TO") => side = Side::Out,
                // `TYPE`, and the `NORMAL` / `ANY` that make this not a
                // relation at all.
                _ => {}
            }
            continue;
        }
        if child.kind() != k::IDENT {
            continue;
        }
        let Some(table) = text_of(source, child) else {
            continue;
        };
        match side {
            Side::In => relation.in_tables.push(table.to_string()),
            Side::Out => relation.out_tables.push(table.to_string()),
            // An `Ident` before either keyword cannot be placed. The grammar
            // does not produce one here, so this is a guard, not a case.
            Side::Neither => {}
        }
    }

    is_relation.then_some(relation)
}

fn extract_field(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
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
        comment: extract_comment(node, source, lines),
        permissions: children
            .iter()
            .filter(|child| child.kind() == k::PERMISSIONS_FOR_CLAUSE)
            .map(|child| parse_permission_rule(*child, source, lines, origin, uri))
            .collect(),
        origin,
        explicit: true,
        inference: None,
        location: location(uri, source, lines, node),
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
                location: location(uri, source, lines, node),
                relation: None,
            };
            upsert_inferred_table(analysis, inferred, uri, source, lines, node);
        }
    }

    analysis.document_symbols.push(definition_symbol(
        &format!("FIELD {table}.{name}"),
        SymbolKind::FIELD,
        source,
        lines,
        node,
    ));
    analysis.fields.push(field);
}

fn extract_event(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
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
        lines,
        node,
    ));
    analysis.events.push(EventDef {
        table,
        name,
        comment: extract_comment(node, source, lines),
        when_clause,
        then_clause,
        origin,
        location: location(uri, source, lines, node),
    });
}

fn extract_function(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
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
        .map(|child| parse_permission_rule(*child, source, lines, origin, uri))
        .collect::<Vec<_>>();

    let body_node = children
        .iter()
        .find(|child| child.kind() == k::BLOCK)
        .copied();
    let called_functions = body_node
        .map(|body| collect_called_functions(body, source))
        .unwrap_or_default();

    let selection_range = lines.range(source, name_node.start_byte(), name_node.end_byte());

    analysis.document_symbols.push(definition_symbol(
        &format!("FUNCTION {name}"),
        SymbolKind::FUNCTION,
        source,
        lines,
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
        comment: extract_comment(node, source, lines),
        permissions,
        origin,
        explicit: true,
        inference: None,
        location: location(uri, source, lines, node),
        selection_range,
        body_range: body_node.map(|body| lines.range(source, body.start_byte(), body.end_byte())),
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
    lines: &LineIndex,
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

    // Any remaining index-variant clause (FULLTEXT/COUNT/HNSW/DISKANN/…) is
    // captured verbatim as an option string. `OVERWRITE` and `IF NOT EXISTS`
    // end in `Clause` too but modify the definition, not the index.
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
                        | k::OVERWRITE_CLAUSE
                        | k::IF_NOT_EXISTS_CLAUSE
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
        lines,
        node,
    ));
    analysis.indexes.push(IndexDef {
        table,
        name,
        fields,
        unique,
        options,
        origin,
        location: location(uri, source, lines, node),
    });
}

fn extract_param(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
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
        lines,
        node,
    ));
    analysis.params.push(ParamDef {
        name,
        value_preview,
        comment: extract_comment(node, source, lines),
        origin,
        location: location(uri, source, lines, node),
    });
}

/// `DEFINE ANALYZER <name> …`
///
/// Indexed so completion can offer the name where one is referenced —
/// `DEFINE INDEX … FULLTEXT ANALYZER <name>`. Before this, nothing extracted
/// analyzers, so that slot had nothing to offer.
fn extract_analyzer(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
    uri: &Uri,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    let Some(name) = k::named_children(node)
        .into_iter()
        .find(|child| child.kind() == k::IDENT)
        .and_then(|child| text_of(source, child))
    else {
        return;
    };

    analysis.document_symbols.push(definition_symbol(
        &format!("ANALYZER {name}"),
        SymbolKind::OBJECT,
        source,
        lines,
        node,
    ));
    analysis.analyzers.push(AnalyzerDef {
        name,
        comment: None,
        origin,
        location: location(uri, source, lines, node),
    });
}

fn extract_access(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
    uri: &Uri,
    origin: SymbolOrigin,
    analysis: &mut DocumentAnalysis,
) {
    // `DEFINE ACCESS x …` / `DEFINE SCOPE x …` name the access inside the
    // `AccessDefinition` / `ScopeDefinition` wrapper, not as a direct child of
    // the statement — so search the wrapper when there is one.
    let scope = k::named_children(node)
        .into_iter()
        .find(|child| matches!(child.kind(), k::ACCESS_DEFINITION | k::SCOPE_DEFINITION))
        .unwrap_or(node);
    let Some(name) = k::named_children(scope)
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
        lines,
        node,
    ));
    analysis.accesses.push(AccessDef {
        name,
        comment: None,
        origin,
        location: location(uri, source, lines, node),
    });
}

fn extract_query_fact(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
    uri: &Uri,
    action: QueryAction,
    analysis: &mut DocumentAnalysis,
) {
    let target_nodes = target_nodes_for_statement(node, source);
    let target_refs = target_refs_from_nodes(&target_nodes, source, lines);
    let targets: Vec<String> = target_refs.iter().map(|entry| entry.name.clone()).collect();
    let field_refs = collect_field_refs(node, source, lines);
    let touched_fields: Vec<String> = field_refs.iter().map(|entry| entry.name.clone()).collect();
    let target_resolution = if targets.is_empty() {
        classify_unresolved_targets(&target_nodes)
    } else {
        TargetResolution::Static
    };
    let dynamic = targets.is_empty();

    // `statement_symbol` answers for every statement kind the grammar names, so
    // the fallback only runs for one it does not. Build the preview inside the
    // `unwrap_or_else` rather than ahead of it — it is two allocations and a
    // character count, and on the common path nothing reads it.
    analysis
        .document_symbols
        .push(
            statement_symbol(node, source, lines, uri).unwrap_or_else(|| {
                let preview = node
                    .utf8_text(source.as_bytes())
                    .ok()
                    .map(compact_preview)
                    .unwrap_or_default();
                definition_symbol(&preview, SymbolKind::EVENT, source, lines, node)
            }),
        );

    // One location for the statement, shared by the fact and by every inferred
    // table below. It used to be recomputed per use.
    let statement_location = location(uri, source, lines, node);

    for table in &targets {
        // The guard first: `upsert_inferred_table` discards the definition when
        // the document already has this table, which for a file that queries the
        // same table repeatedly is almost every statement. Building it first meant
        // two `format!`s, a `Uri` clone and a range conversion thrown away.
        if already_has_table(analysis, table) {
            continue;
        }
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
            location: statement_location.clone(),
            relation: None,
        };
        upsert_inferred_table(analysis, inferred, uri, source, lines, node);
    }

    if action == QueryAction::Relate
        && let Some(observation) = relate_edge_observation(node, source)
    {
        analysis.edge_observations.push(observation);
    }

    for inferred_field in
        infer_fields_from_statement(node, source, lines, uri, action, &targets, &touched_fields)
    {
        analysis.fields.push(inferred_field);
    }

    analysis.query_facts.push(QueryFact {
        action,
        target_tables: targets,
        touched_fields,
        dynamic,
        location: statement_location,
        target_refs,
        field_refs,
        target_resolution,
    });
}

/// Read `RELATE from->edge->to` as one graph edge.
///
/// The grammar spells a RELATE's arrows as *direct* children of the statement
/// (`LookupRight` / `LookupLeft`), not wrapped in a [`k::LOOKUP`] the way a
/// traversal is, so this cannot share the traversal walk.
///
/// A left arrow reverses the reading: `RELATE b<-knows<-a` records the same
/// edge as `RELATE a->knows->b`. Returns `None` unless the statement has all
/// three subjects and the middle one names a table.
fn relate_edge_observation(node: Node<'_>, source: &str) -> Option<EdgeObservation> {
    let mut subjects: Vec<Option<String>> = Vec::with_capacity(3);
    let mut points_right: Option<bool> = None;

    for child in k::named_children(node) {
        match child.kind() {
            k::LOOKUP_RIGHT | k::LOOKUP_LEFT => {
                // Only the first arrow decides the reading. The grammar lets
                // the two arrows differ; SurrealDB reads the first.
                points_right.get_or_insert(child.kind() == k::LOOKUP_RIGHT);
            }
            // A `$param`, a call, or an array names no table we can pin down.
            k::VARIABLE_NAME | k::FUNCTION_CALL | k::ARRAY => subjects.push(None),
            k::IDENT | k::RECORD_ID => {
                subjects.push(text_of(source, child).and_then(|text| normalize_table_name(&text)));
            }
            _ => {}
        }
    }

    let [first, edge, third] = subjects.as_slice() else {
        return None;
    };
    // Without a named edge table there is no edge to record.
    let edge = edge.clone()?;
    let (from, to) = if points_right.unwrap_or(true) {
        (first.clone(), third.clone())
    } else {
        (third.clone(), first.clone())
    };
    Some(EdgeObservation { edge, from, to })
}

fn infer_fields_from_statement(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
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
        let Some((name, target)) = field_assignment_target(assignment, source) else {
            continue;
        };
        // The right-hand side of `field = value` is the last named child
        // (the hidden `_value` rule means its children appear directly
        // under the `FieldAssignment`). The target is excluded by identity,
        // not by kind: once `FieldAssignment` takes an `Idiom`, `SET a = b.c`
        // has an `Idiom` on both sides and a kind test would skip the value
        // too. A bare `Ident` right-hand side stays excluded — it names
        // another field rather than carrying a literal to type.
        let type_expr = children
            .iter()
            .rev()
            .find(|child| {
                child.id() != target.id() && !matches!(child.kind(), k::IDENT | k::OPERATOR)
            })
            .map(|child| infer_type_from_value(*child, source));

        fields.push(inferred_field(
            &target_table,
            &name,
            type_expr,
            uri,
            source,
            lines,
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
                lines,
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
            lines,
            node,
            action,
        ));
    }

    fields
}

// Each argument is a separate fact about the field being recorded; grouping
// them would only move the same list behind a struct literal at every call.
#[allow(clippy::too_many_arguments)]
fn inferred_field(
    table: &str,
    field: &str,
    type_expr: Option<TypeExpr>,
    uri: &Uri,
    source: &str,
    lines: &LineIndex,
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
        location: location(uri, source, lines, node),
    }
}

fn collect_function_references(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
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
        let selection_range = lines.range(source, reference.start_byte(), reference.end_byte());
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
pub(crate) fn function_return_type(children: &[Node<'_>], source: &str) -> Option<TypeExpr> {
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
    lines: &LineIndex,
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
        location: Some(location(uri, source, lines, node)),
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
        k::IDENT | k::RECORD_ID | k::RANGE_RECORD_ID | k::PATH | k::ARRAY | k::TYPE_CAST
    )
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
            // There is no `FromClause` node in the grammar: the targets
            // are laid out as direct children after the `FROM` keyword,
            // up to the first trailing clause (`WHERE`, `GROUP`, …).
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
fn target_refs_from_nodes(
    relevant_nodes: &[Node<'_>],
    source: &str,
    lines: &LineIndex,
) -> Vec<NamedRange> {
    let mut refs: Vec<NamedRange> = Vec::new();
    for relevant in relevant_nodes {
        // Read the kind once. This runs for every target of every statement in
        // the document, and `Node::kind` crosses into the C parser each time.
        let kind = relevant.kind();
        if !is_target_name_kind(kind) {
            continue;
        }
        // A graph traversal names one target — where the walk *ends* — not
        // every table it passes through. Without this, `FROM person->likes->
        // product` reports `person`, `likes` and `product` as three co-equal
        // targets: the edge draws a false `unknown-table`, and field
        // completion offers all three tables' columns.
        if kind == k::PATH
            && let Some(hop) = traversal_last_hop(*relevant)
        {
            if let Some(terminal) = traversal_terminal(hop)
                && let Some(name) = terminal
                    .utf8_text(source.as_bytes())
                    .ok()
                    .and_then(normalize_table_name)
                && !refs.iter().any(|existing| existing.name == name)
            {
                refs.push(NamedRange {
                    name,
                    range: lines.range(source, terminal.start_byte(), terminal.end_byte()),
                });
            }
            // Never fall through to the sweep below: a wildcard hop names no
            // table, and naming the ones it passed through would be worse than
            // naming none.
            continue;
        }
        for candidate in descendants_of_kind(*relevant, k::IDENT)
            .into_iter()
            .chain(descendants_of_kind(*relevant, k::RECORD_ID))
            .chain(descendants_of_kind(*relevant, k::RANGE_RECORD_ID))
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
                range: lines.range(source, candidate.start_byte(), end_byte),
            });
        }
    }
    refs
}

/// The tables a statement reads from, for a statement node.
///
/// Exposed for [`crate::semantic::infer`], which needs it twice: for the anchor
/// of a traversal that writes no base (`SELECT ->knows->person FROM person`),
/// and for the schema a projection's columns are read from.
///
/// Only *statically named* tables count. A `$parameter`, a function call or a
/// subquery target names no table until run time, and answering with the source
/// text — `"$target"` — would have the caller look up columns on a table that
/// cannot exist, and then claim a row shape built from what it did not find.
///
/// Resolves a traversal the same way [`target_refs_from_nodes`] does, so this
/// and the query facts can never disagree about what `FROM` means.
pub fn select_target_tables(statement: Node<'_>, source: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for node in target_nodes_for_statement(statement, source) {
        // A traversal reads the table it lands on, not the one it starts from.
        let named = match traversal_last_hop(node) {
            Some(hop) => match traversal_terminal(hop) {
                Some(terminal) => terminal,
                // A wildcard hop lands nowhere nameable.
                None => continue,
            },
            None => node,
        };
        if !matches!(named.kind(), k::IDENT | k::RECORD_ID | k::RANGE_RECORD_ID) {
            continue;
        }
        if let Some(name) = named
            .utf8_text(source.as_bytes())
            .ok()
            .and_then(normalize_table_name)
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names
}

/// The last hop of a graph traversal, for a node that is one.
///
/// `None` when the node is not a traversal at all — an ordinary target, which
/// the caller reads whole.
fn traversal_last_hop<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    if node.kind() != k::PATH {
        return None;
    }
    k::named_children(node)
        .into_iter()
        .rfind(|child| child.kind() == k::LOOKUP)
}

/// The table a graph traversal *lands on*.
///
/// The rows a statement reads come from where the walk ends, so that is its
/// one target — `person->likes->product` selects products.
///
/// `None` when the last hop is `->?`, `->*` or a parenthesised selection,
/// which names no single table. The caller reports no target at all in that
/// case, rather than inventing one or falling back to naming every table the
/// traversal passes through.
fn traversal_terminal<'tree>(hop: Node<'tree>) -> Option<Node<'tree>> {
    k::find_child(hop, k::IDENT)
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
    // A traversal that ends on `->?`, `->*` or a parenthesised selection. The
    // walk is perfectly valid SurrealQL; the server just cannot name where it
    // lands, which is exactly what `Expression` means — and what keeps the
    // "target could not be resolved" warning quiet.
    if relevant_nodes
        .iter()
        .filter_map(|node| traversal_last_hop(*node))
        .any(|hop| traversal_terminal(hop).is_none())
    {
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

/// The assigned-to side of a `FieldAssignment`, as `(dotted name, node)`.
///
/// Accepts both grammar shapes. The pinned grammar declares
/// `FieldAssignment: seq($.Ident, …)`, so a nested target like
/// `SET name.first = …` does not parse and the whole `.first` lands in an
/// ERROR node. Once `FieldAssignment` takes an `Idiom`, the target arrives as
/// one `Idiom` wrapping the dotted parts, and a plain `SET age = …` arrives as
/// an `Idiom` around a single `Ident` rather than a bare `Ident`.
///
/// Reading both keeps this correct either side of that grammar bump.
fn field_assignment_target<'tree>(
    assignment: Node<'tree>,
    source: &str,
) -> Option<(String, Node<'tree>)> {
    let target = k::named_children(assignment)
        .into_iter()
        .find(|child| matches!(child.kind(), k::IDIOM | k::PATH | k::IDENT))?;
    k::dotted_name(source, target).map(|name| (name, target))
}

fn collect_field_refs(node: Node<'_>, source: &str, lines: &LineIndex) -> Vec<NamedRange> {
    let mut fields: Vec<NamedRange> = Vec::new();
    for assignment in descendants_of_kind(node, k::FIELD_ASSIGNMENT) {
        if let Some((name, target)) = field_assignment_target(assignment, source)
            && !fields.iter().any(|existing| existing.name == name)
        {
            fields.push(NamedRange {
                name,
                range: lines.range(source, target.start_byte(), target.end_byte()),
            });
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

pub(crate) fn identifier_from_on_table_clause(node: Node<'_>, source: &str) -> Option<String> {
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

fn extract_comment(node: Node<'_>, source: &str, lines: &LineIndex) -> Option<String> {
    let clause = k::named_children(node)
        .into_iter()
        .find(|child| child.kind() == k::COMMENT_CLAUSE)
        .and_then(|child| k::find_child(child, k::STRING))
        .and_then(|child| text_of(source, child))
        .map(|value| unquote(&value));

    clause.or_else(|| leading_comment_text(node, source, lines))
}

/// The run of `--` / `//` / `#` comment lines directly above `node`, if any.
///
/// Reads lines through [`LineIndex`] rather than `source.lines().collect()`. The
/// `Vec` version allocated one entry per line of the *whole document* on every
/// call, and this runs for every DEFINE that has no `COMMENT` clause — the common
/// case — so a large schema file scanned itself once per definition.
fn leading_comment_text(node: Node<'_>, source: &str, lines: &LineIndex) -> Option<String> {
    let start_row = node.start_position().row;
    if start_row == 0 || start_row >= lines.line_count() {
        return None;
    }

    let mut comments = Vec::new();
    let mut row = start_row;
    while row > 0 {
        row -= 1;
        let Some(line) = lines.line_text(source, row) else {
            break;
        };
        let trimmed = line.trim();
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

fn definition_symbol(
    name: &str,
    kind: SymbolKind,
    source: &str,
    lines: &LineIndex,
    node: Node<'_>,
) -> DocumentSymbol {
    #[allow(deprecated)]
    DocumentSymbol {
        name: name.to_string(),
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range: lines.range(source, node.start_byte(), node.end_byte()),
        selection_range: lines.range(source, node.start_byte(), node.start_byte()),
        children: None,
    }
}

fn statement_symbol(
    node: Node<'_>,
    source: &str,
    lines: &LineIndex,
    uri: &Uri,
) -> Option<DocumentSymbol> {
    let preview = node
        .utf8_text(source.as_bytes())
        .ok()
        .map(compact_preview)
        .filter(|preview| !preview.is_empty())?;
    let _ = uri;
    Some(definition_symbol(
        &preview,
        SymbolKind::EVENT,
        source,
        lines,
        node,
    ))
}

fn upsert_inferred_table(
    analysis: &mut DocumentAnalysis,
    inferred: TableDef,
    source_uri: &Uri,
    source: &str,
    lines: &LineIndex,
    source_node: Node<'_>,
) {
    // One scan, not two: the previous pair tested `explicit` and `!explicit`
    // separately, which is just "is this name present at all".
    if already_has_table(analysis, &inferred.name) {
        return;
    }
    analysis.document_symbols.push(definition_symbol(
        &format!("TABLE {}", inferred.name),
        SymbolKind::STRUCT,
        source,
        lines,
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

/// Whether the document already records a table by this name, explicit or
/// inferred.
///
/// The caller uses it as a guard *before* building an inferred [`TableDef`], so a
/// file that queries the same table on every line pays one name comparison per
/// statement instead of a discarded definition.
fn already_has_table(analysis: &DocumentAnalysis, name: &str) -> bool {
    analysis.tables.iter().any(|table| table.name == name)
}

fn location(uri: &Uri, source: &str, lines: &LineIndex, node: Node<'_>) -> Location {
    Location::new(
        uri.clone(),
        lines.range(source, node.start_byte(), node.end_byte()),
    )
}

/// Default upper bound on syntax diagnostics per document. A pathological
/// buffer (generated dumps, pasted binaries) shouldn't flood the
/// editor's problems panel.
///
/// This counts *diagnostics*, not lines: nothing here limits how long a
/// document may be. It also covers the syntax pass only — `parse` and
/// `unknown-type`. Semantic and type diagnostics are bounded by the query
/// facts and definitions they are derived from, so they need no cap.
///
/// Overridable per client through `analysis.maxSyntaxDiagnostics`; raised from
/// 100 in 0.6.0, because a large schema file mid-edit legitimately exceeds a
/// hundred parse errors and the truncation read as "the server stopped working".
pub const DEFAULT_MAX_SYNTAX_DIAGNOSTICS: usize = 2000;

/// `0` in the setting means "report every one". Kept as a saturating sentinel
/// rather than an `Option` so the comparison in the hot walk stays a plain
/// integer test.
pub fn syntax_diagnostic_limit(configured: usize) -> usize {
    if configured == 0 {
        usize::MAX
    } else {
        configured
    }
}

/// The parts of the syntax-diagnostic walk that do not change as it descends.
///
/// Hoisting them out of the parameter list is what lets the two mutually
/// recursive halves of the walk ([`collect_node_diagnostics`] and
/// [`descend_into_error`]) share **one** depth counter. A guard that each half
/// incremented separately would not bound the stack, because the pair alternates
/// on malformed input: an ERROR node's children are walked by one and its nested
/// errors by the other.
struct DiagnosticWalk<'a> {
    uri: Option<&'a Uri>,
    source: &'a str,
    lines: &'a LineIndex,
    known_names: &'a std::collections::HashSet<String>,
    limit: usize,
}

fn collect_node_diagnostics(
    walk: &DiagnosticWalk<'_>,
    node: Node<'_>,
    depth: u32,
    parse_rows: &mut [bool],
    diagnostics: &mut Vec<Diagnostic>,
) {
    if diagnostics.len() >= walk.limit {
        return;
    }

    // Deeper than any query SurrealDB would run. Stop rather than overflow the
    // stack: see `semantic::limits`.
    if limits::too_deep(depth) {
        push_parse_diagnostic(
            too_deeply_nested_diagnostic(walk.source, walk.lines, node),
            parse_rows,
            diagnostics,
        );
        return;
    }

    if node.is_missing() {
        push_parse_diagnostic(
            missing_node_diagnostic(walk.source, walk.lines, node),
            parse_rows,
            diagnostics,
        );
        return;
    }

    if node.is_error() {
        push_parse_diagnostic(
            error_node_diagnostic(walk.uri, walk.source, walk.lines, node, walk.known_names),
            parse_rows,
            diagnostics,
        );
        // A single typo often makes tree-sitter emit one ERROR node
        // spanning a large region that still contains more precise
        // nested MISSING/ERROR nodes — surface those too instead of
        // hiding them behind one giant squiggle.
        descend_into_error(
            walk,
            node,
            node.start_position().row,
            depth + 1,
            parse_rows,
            diagnostics,
        );
        return;
    }

    // A type position holding a word the engine's kind grammar does not have.
    // This sits in the syntax pass rather than beside the type checks in
    // `infer` because the engine refuses to *parse* such a query, so the
    // report must not disappear when `enable_type_checking` is off.
    if node.kind() == k::TYPE_NAME
        && !parse_failure_on_line(parse_rows, node)
        && let Some(diagnostic) = unknown_type_diagnostic(walk.source, walk.lines, node)
    {
        diagnostics.push(diagnostic);
        return;
    }

    // `node.walk()` allocates and frees a tree-sitter cursor, so it is not worth
    // paying for a node that has nothing to iterate. This walk covers every node
    // in the tree, anonymous ones included, and over half of them are leaves.
    if node.child_count() == 0 {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_node_diagnostics(walk, child, depth + 1, parse_rows, diagnostics);
    }
}

/// Reported once where a walk stopped because the tree nests deeper than
/// [`limits::MAX_NODE_DEPTH`].
///
/// It is deliberately a `parse` error rather than a new code. SurrealDB refuses
/// to parse a query this deep too (its `expr_recursion_limit` and
/// `object_recursion_limit` are far lower), so "this does not parse" is the
/// truthful thing to say, and an agent keying on `parse` already knows to fix
/// the query rather than the schema.
/// A `DocumentAnalysis` holding nothing but the URI and a tree.
///
/// Used by the refusal paths, which have a document to account for but no facts
/// to report about it.
fn blank_analysis(uri: Uri, tree: tree_sitter::Tree) -> DocumentAnalysis {
    DocumentAnalysis {
        uri,
        text: String::new(),
        tree,
        line_index: LineIndex::default(),
        tables: Vec::new(),
        events: Vec::new(),
        indexes: Vec::new(),
        fields: Vec::new(),
        functions: Vec::new(),
        params: Vec::new(),
        accesses: Vec::new(),
        analyzers: Vec::new(),
        query_facts: Vec::new(),
        edge_observations: Vec::new(),
        references: Vec::new(),
        syntax_diagnostics: Vec::new(),
        document_symbols: Vec::new(),
    }
}

/// Reported once when a document exceeds `analysis.maxDocumentBytes`.
///
/// INFORMATION rather than ERROR: nothing is wrong with the file, the server has
/// simply declined to read it. Saying nothing at all would be worse: the user
/// would see a document with no diagnostics and reasonably conclude it is clean.
fn too_large_diagnostic(lines: &LineIndex, size: usize, max_bytes: usize) -> Diagnostic {
    Diagnostic {
        range: lines.range("", 0, 0),
        severity: Some(DiagnosticSeverity::INFORMATION),
        code: codes::as_code(codes::DOCUMENT_TOO_LARGE),
        source: Some("surreal-language-server".to_string()),
        message: format!(
            "Document is {} KB, over the {} KB analysis limit, so it was not \
             analysed. Raise `analysis.maxDocumentBytes` (or set it to 0) to \
             include it.",
            size / 1024,
            max_bytes / 1024,
        ),
        ..Diagnostic::default()
    }
}

/// The same report as [`too_deeply_nested_diagnostic`], for the pre-parse
/// bracket check, which has no tree and therefore no node to point at.
///
/// Anchored at the start of the document: there is no meaningful narrower range
/// when the whole file is one runaway nest.
fn too_deeply_nested_text_diagnostic(lines: &LineIndex) -> Diagnostic {
    Diagnostic {
        range: lines.range("", 0, 0),
        severity: Some(DiagnosticSeverity::ERROR),
        code: codes::as_code(codes::TOO_DEEPLY_NESTED),
        source: Some("surreal-language-server".to_string()),
        message: format!(
            "Brackets nest more than {} levels deep. SurrealDB will not parse \
             this either, and the analyzer does not attempt it.",
            limits::MAX_NODE_DEPTH
        ),
        ..Diagnostic::default()
    }
}

fn too_deeply_nested_diagnostic(source: &str, lines: &LineIndex, node: Node<'_>) -> Diagnostic {
    // The node's own first line, the same clamping every other parse diagnostic
    // uses: a range spanning the whole nest would underline most of the file.
    let start = node.start_byte();
    let end = source
        .get(start..node.end_byte())
        .and_then(|region| region.find('\n'))
        .map(|offset| start + offset)
        .unwrap_or_else(|| node.end_byte());

    Diagnostic {
        range: lines.range(source, start, end),
        severity: Some(DiagnosticSeverity::ERROR),
        code: codes::as_code(codes::TOO_DEEPLY_NESTED),
        source: Some("surreal-language-server".to_string()),
        message: format!(
            "Expression nests more than {} levels deep. SurrealDB will not parse \
             it either, and the analyzer stops descending here.",
            limits::MAX_NODE_DEPTH
        ),
        ..Diagnostic::default()
    }
}

/// `LET $x: xxx = 2` — a type position naming a type SurrealQL does not have.
///
/// Every `TypeName` node in the tree is a candidate, which is sound because the
/// grammar produces that node from exactly one rule
/// (`alias($._rawident, $.TypeName)` in `_singleType`), and every parent that can
/// hold one is a type position: `Type`, `TypeClause`, `TypeCast`,
/// `ParameterizedType`, `UnionType`, `ArrayType`, `ObjectTypeProperty`, `Closure`
/// and `DefineStatement`. So the check needs no per-position plumbing, and it
/// reaches nested arguments, union members, tuple elements and object-type field
/// types for free.
fn unknown_type_diagnostic(source: &str, lines: &LineIndex, node: Node<'_>) -> Option<Diagnostic> {
    let name = k::text_of(source, node)?;
    if name.is_empty() || type_name::is_known(name) {
        return None;
    }
    // `record<person>` names a table, not a type.
    if names_a_foreign_argument(source, node) {
        return None;
    }
    // A name the parser only guessed at says nothing about what the author
    // meant, and a `parse` diagnostic already covers the region.
    if has_error_ancestor(node) {
        return None;
    }

    let suggestion = type_name::nearest(name);
    Some(Diagnostic {
        range: lines.range(source, node.start_byte(), node.end_byte()),
        severity: Some(DiagnosticSeverity::ERROR),
        code: codes::as_code(codes::UNKNOWN_TYPE),
        source: Some("surreal-language-server".to_string()),
        message: match suggestion {
            Some(candidate) => {
                format!("Unknown type `{name}`. Did you mean `{candidate}`?")
            }
            None => format!("Unknown type `{name}`."),
        },
        // Structured payload for the quick fix, with the message text as the
        // fallback for clients that strip non-standard fields. `table` is
        // additive and read only by the schemaless policy — `unknown_type_payload`
        // in `model.rs` looks up `type`/`suggestion` by key and ignores it.
        data: Some({
            let mut payload = match suggestion {
                Some(candidate) => {
                    serde_json::json!({ "type": name, "suggestion": candidate })
                }
                None => serde_json::json!({ "type": name }),
            };
            if let Some(table) = enclosing_define_field_table(source, node)
                && let Some(object) = payload.as_object_mut()
            {
                object.insert("table".to_string(), serde_json::Value::String(table));
            }
            payload
        }),
        ..Diagnostic::default()
    })
}

/// True when this `TypeName` is an argument of a constructor whose arguments are
/// not types, and so names a table, a bucket, or a geometry.
///
/// Without this, the check reports `person` in `record<person>`, `bucket` in
/// `file<bucket>` and `multipoint` in `geometry<multipoint>` — all valid
/// SurrealQL. The four constructors parse to the same shape as `array<int>`, so
/// the argument is indistinguishable from a type by node kind alone; only the
/// constructor name separates them.
fn names_a_foreign_argument(source: &str, node: Node<'_>) -> bool {
    // Climb out of any enclosing `UnionType` first. `record<a | b>` nests the
    // table names one level deeper than `record<a>` does —
    // `ParameterizedType(TypeName, UnionType(TypeName, Pipe, TypeName))` — so a
    // check that only reads the immediate parent reports both names as unknown
    // types. `_type` admits a `UnionType` and nothing else as a wrapper here,
    // which is why this loop is the whole story.
    let mut child = node;
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        if current.kind() != k::UNION_TYPE {
            break;
        }
        child = current;
        ancestor = current.parent();
    }

    let Some(parent) = ancestor else {
        return false;
    };
    if parent.kind() != k::PARAMETERIZED_TYPE {
        return false;
    }
    // The first named child is the constructor, and that one *is* a type name:
    // `xxx<int>` must still be reported.
    let Some(head) = parent.named_child(0) else {
        return false;
    };
    if head.id() == child.id() {
        return false;
    }
    k::text_of(source, head).is_some_and(type_name::takes_foreign_arguments)
}

/// True when a `parse` diagnostic already covers this node's line.
///
/// [`has_error_ancestor`] is not enough on its own, because tree-sitter can
/// recover from a statement the grammar does not know into a shape that looks
/// perfectly well-formed. `ALTER FIELD author ON comment TYPE record<person>` —
/// a statement the grammar has no rule for — leaves an `ERROR` covering the head
/// of the statement and then reads the tail as a `TypeCast`, so `person` arrives
/// here as a `TypeName` with no `ERROR` anywhere above it. Reporting it claims
/// `person` is a misspelled type when it is really a table name in a statement
/// the grammar could not read.
///
/// Reading the diagnostics collected so far is sound because
/// [`collect_node_diagnostics`] walks in source order, so an earlier failure on
/// the line is already recorded by the time the type name is reached.
/// Whether a `parse` diagnostic already covers `node`'s line.
///
/// One indexed load. The previous version scanned every diagnostic collected so
/// far, comparing a `String` code on each, for *every* `TypeName` node in the
/// tree — so a schema file full of `TYPE` clauses and parse errors paid
/// `diagnostics x type names` string comparisons.
fn parse_failure_on_line(parse_rows: &[bool], node: Node<'_>) -> bool {
    parse_rows
        .get(node.start_position().row)
        .copied()
        .unwrap_or(false)
}

/// Push a `parse`-coded diagnostic and mark the rows it covers, so
/// [`parse_failure_on_line`] stays a lookup.
fn push_parse_diagnostic(
    diagnostic: Diagnostic,
    parse_rows: &mut [bool],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let start = diagnostic.range.start.line as usize;
    if start < parse_rows.len() {
        let end = (diagnostic.range.end.line as usize).min(parse_rows.len() - 1);
        for row in &mut parse_rows[start..=end] {
            *row = true;
        }
    }
    diagnostics.push(diagnostic);
}

/// True when `node` sits inside an `ERROR` node.
///
/// [`collect_node_diagnostics`] returns at an `ERROR` node, but
/// [`descend_into_error`] re-enters it for the *children* of one, so a node
/// inside a failed region is still reachable from here.
/// The table named by the `DEFINE FIELD` statement enclosing `node`, if any.
///
/// Recorded on the `unknown-type` diagnostic so the publish path can apply
/// `analysis.schemalessDiagnostics` to it. This pass sees one document and has
/// no merged model, so it can only state *which* table the diagnostic belongs
/// to — [`crate::semantic::model::MergedSemanticModel::apply_schemaless_policy`]
/// decides what that table's schema mode means.
///
/// `DefineStatement` is one node kind for every DEFINE form, so the form has to
/// be checked too: without it a `DEFINE FUNCTION fn::f() -> xxx` would be
/// attributed to whatever table happened to be nearby.
fn enclosing_define_field_table(source: &str, node: Node<'_>) -> Option<String> {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if ancestor.kind() == k::DEFINE_STATEMENT {
            if define_form(ancestor, source).as_deref() != Some("field") {
                return None;
            }
            return k::named_children(ancestor)
                .into_iter()
                .find(|child| child.kind() == k::ON_TABLE_CLAUSE)
                .and_then(|clause| identifier_from_on_table_clause(clause, source));
        }
        current = ancestor.parent();
    }
    None
}

fn has_error_ancestor(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if ancestor.is_error() {
            return true;
        }
        current = ancestor.parent();
    }
    false
}

/// Walk the children of a reported ERROR node. Nested errors starting
/// on the same line as the already-reported parent would only repeat
/// the same underline, so they're descended through without their own
/// diagnostic; errors on later lines get reported (the parent's range
/// was clamped to its first line).
fn descend_into_error(
    walk: &DiagnosticWalk<'_>,
    node: Node<'_>,
    reported_row: usize,
    depth: u32,
    parse_rows: &mut [bool],
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Shares the counter with `collect_node_diagnostics`; see `DiagnosticWalk`.
    if limits::too_deep(depth) {
        push_parse_diagnostic(
            too_deeply_nested_diagnostic(walk.source, walk.lines, node),
            parse_rows,
            diagnostics,
        );
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if diagnostics.len() >= walk.limit {
            return;
        }
        if child.is_missing() {
            push_parse_diagnostic(
                missing_node_diagnostic(walk.source, walk.lines, child),
                parse_rows,
                diagnostics,
            );
            continue;
        }
        if child.is_error() {
            if child.start_position().row == reported_row {
                descend_into_error(
                    walk,
                    child,
                    reported_row,
                    depth + 1,
                    parse_rows,
                    diagnostics,
                );
            } else {
                push_parse_diagnostic(
                    error_node_diagnostic(
                        walk.uri,
                        walk.source,
                        walk.lines,
                        child,
                        walk.known_names,
                    ),
                    parse_rows,
                    diagnostics,
                );
                descend_into_error(
                    walk,
                    child,
                    child.start_position().row,
                    depth + 1,
                    parse_rows,
                    diagnostics,
                );
            }
            continue;
        }
        collect_node_diagnostics(walk, child, depth + 1, parse_rows, diagnostics);
    }
}

fn missing_node_diagnostic(source: &str, lines: &LineIndex, node: Node<'_>) -> Diagnostic {
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
        range: lines.range(source, start, end),
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
    lines: &LineIndex,
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
            location: Location::new(uri.clone(), lines.range(source, full_start, full_end)),
            message: format!(
                "The invalid region continues to line {}.",
                node.end_position().row + 1
            ),
        }]),
        _ => None,
    };

    Diagnostic {
        range: lines.range(source, full_start, range_end),
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
        k::STRING | k::FORMAT_STRING => "a string".to_string(),
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
    let mut cursor = node.walk();
    collect_descendants(node, kind, &mut cursor, &mut matches);
    matches
}

/// Every named descendant of `node` (and `node` itself) whose kind is `kind`, in
/// pre-order.
///
/// Driven by one reused cursor rather than recursion. The recursive form called
/// `node.walk()` at every node, and each of those allocates and frees a
/// tree-sitter cursor — measured at 7.53 ms against 4.25 ms for the same
/// traversal with a single cursor, on a 70,000-node tree. This function is
/// reached several times per statement, so that difference is most of what
/// `analyze_document` spent above the parse.
///
/// Visits **named** nodes only, matching the `named_children` walk it replaced:
/// an anonymous child is neither visited nor descended into.
/// `descendants_of_kind_recursive` is the oracle for that in the tests.
fn collect_descendants<'tree>(
    node: Node<'tree>,
    kind: &str,
    cursor: &mut tree_sitter::TreeCursor<'tree>,
    matches: &mut Vec<Node<'tree>>,
) {
    cursor.reset(node);
    loop {
        let current = cursor.node();
        if current.kind() == kind {
            matches.push(current);
        }
        if goto_first_named_child(cursor) {
            continue;
        }
        // No children left to descend: step sideways, climbing until a sibling
        // exists or we are back at the root of this walk.
        loop {
            if cursor.node() == node {
                return;
            }
            if goto_next_named_sibling(cursor) {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

/// Move `cursor` to the first *named* child, leaving it put and returning false
/// when there is none.
fn goto_first_named_child(cursor: &mut tree_sitter::TreeCursor<'_>) -> bool {
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        if cursor.node().is_named() {
            return true;
        }
        if !cursor.goto_next_sibling() {
            cursor.goto_parent();
            return false;
        }
    }
}

/// Move `cursor` to the next *named* sibling. Returns false with the cursor
/// left on the last sibling when there is none.
fn goto_next_named_sibling(cursor: &mut tree_sitter::TreeCursor<'_>) -> bool {
    while cursor.goto_next_sibling() {
        if cursor.node().is_named() {
            return true;
        }
    }
    false
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
    walk_inlay_hints(
        root,
        source,
        &LineIndex::new(source),
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
    lines: &LineIndex,
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
        {
            let name = raw.trim();
            // `model.functions` is keyed by the *full* name including the
            // `fn::` prefix (see `merge_function`), so look up the raw text.
            // Stripping the prefix first meant this never matched and custom
            // function inlay hints never appeared.
            let names = if name.starts_with("fn::") {
                model.functions.get(name).map(|function| {
                    function
                        .params
                        .iter()
                        .map(|param| param.name.clone())
                        .collect::<Vec<_>>()
                })
            } else {
                builtin_parameter_names(name)
            };
            if let Some(names) = names
                && !names.is_empty()
            {
                let mut cursor = node.walk();
                let arg_list = node
                    .children(&mut cursor)
                    .find(|child| child.kind() == k::ARGUMENT_LIST);
                if let Some(arg_list) = arg_list {
                    emit_argument_hints(arg_list, source, lines, &names, hints);
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_inlay_hints(child, source, lines, range_start, range_end, model, hints);
    }
}

/// The parameter names of a builtin, when a hint would earn its space.
///
/// Two rules, both about noise rather than correctness:
///
/// * A signature the generator could not read has no names to show.
/// * A single-parameter call gains nothing from `arg:` — the reader can already
///   see there is one argument. Most builtins take one, so without this the
///   viewport fills with hints that say nothing.
fn builtin_parameter_names(name: &str) -> Option<Vec<String>> {
    let signature = crate::grammar::builtin_signature(name)?;
    if !signature.generated.signature_known || signature.generated.params.len() < 2 {
        return None;
    }
    Some(
        signature
            .generated
            .params
            .iter()
            .map(|param| param.name.to_string())
            .collect(),
    )
}

fn emit_argument_hints(
    arg_list: Node<'_>,
    source: &str,
    lines: &LineIndex,
    param_names: &[String],
    hints: &mut Vec<InlayHint>,
) {
    let mut cursor = arg_list.walk();
    let arguments: Vec<Node<'_>> = arg_list
        .children(&mut cursor)
        .filter(|child| child.is_named())
        .collect();

    for (index, argument) in arguments.iter().enumerate() {
        let Some(name) = param_names.get(index) else {
            break;
        };
        hints.push(InlayHint {
            position: lines.position(source, argument.start_byte()),
            label: InlayHintLabel::String(format!("{name}:")),
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

    use crate::semantic::node_kind as k;

    /// The recursive walk `collect_descendants` replaced, kept as its oracle.
    ///
    /// The rewrite is an allocation change — one reused cursor instead of one
    /// per node — and must visit exactly the same nodes in the same order. This
    /// is the only specification for that.
    fn descendants_of_kind_recursive<'tree>(
        node: tree_sitter::Node<'tree>,
        kind: &str,
    ) -> Vec<tree_sitter::Node<'tree>> {
        fn walk<'tree>(
            node: tree_sitter::Node<'tree>,
            kind: &str,
            out: &mut Vec<tree_sitter::Node<'tree>>,
        ) {
            if node.kind() == kind {
                out.push(node);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk(child, kind, out);
            }
        }
        let mut out = Vec::new();
        walk(node, kind, &mut out);
        out
    }

    #[test]
    fn the_cursor_walk_visits_exactly_what_the_recursive_walk_did() {
        let sources = [
            "",
            "SELECT name FROM person;",
            "SELECT name, email FROM person WHERE age > 21;\nUPDATE person SET age = 30;\n",
            "DEFINE TABLE person SCHEMAFULL;\nDEFINE FIELD name ON person TYPE string;\n",
            "CREATE person SET name = 'a', tags = ['x', 'y'], meta = { k: 1, j: { n: 2 } };",
            "DEFINE FUNCTION fn::f($a: int) { LET $b = $a + 1; RETURN fn::g($b); };",
            "UPDATE person SET a = 1, b = 2, c = 3 WHERE id = person:1;",
            // Deliberately broken, so ERROR and MISSING nodes are in the tree.
            "SELCT nmae FRM WHERE ;;;\nDEFINE FIELD ON TYPE;\n",
            "SET sym = '₹';\nCREATE t SET emoji = '🚀';\n",
        ];
        // Every kind the extraction walk actually searches for, plus a couple
        // that exercise deeper nesting.
        let kinds = [
            k::FIELD_ASSIGNMENT,
            k::OBJECT,
            k::FUNCTION_NAME,
            k::CUSTOM_FUNCTION_NAME,
            k::IDENT,
            k::TYPE_NAME,
            k::VARIABLE_NAME,
            k::BLOCK,
        ];

        for source in sources {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&crate::grammar::language())
                .expect("grammar loads");
            let tree = parser.parse(source, None).expect("parses");
            let root = tree.root_node();

            for kind in kinds {
                let expected: Vec<(usize, usize)> = descendants_of_kind_recursive(root, kind)
                    .iter()
                    .map(|node| (node.start_byte(), node.end_byte()))
                    .collect();
                let actual: Vec<(usize, usize)> = super::descendants_of_kind(root, kind)
                    .iter()
                    .map(|node| (node.start_byte(), node.end_byte()))
                    .collect();
                assert_eq!(actual, expected, "kind {kind:?} differs for {source:?}");
            }
        }
    }

    /// A nested search must work when the walk starts below the root, since the
    /// extraction walk calls it per statement.
    #[test]
    fn the_cursor_walk_is_correct_from_a_nested_start_node() {
        let source = "CREATE a SET x = 1; CREATE b SET y = 2, z = { inner: 3 }; SELECT * FROM c;";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::grammar::language())
            .expect("grammar loads");
        let tree = parser.parse(source, None).expect("parses");

        let mut cursor = tree.root_node().walk();
        let statements: Vec<tree_sitter::Node<'_>> =
            tree.root_node().named_children(&mut cursor).collect();
        assert!(statements.len() >= 3, "expected several statements");

        for statement in statements {
            for kind in [k::FIELD_ASSIGNMENT, k::OBJECT, k::IDENT] {
                let expected: Vec<(usize, usize)> = descendants_of_kind_recursive(statement, kind)
                    .iter()
                    .map(|node| (node.start_byte(), node.end_byte()))
                    .collect();
                let actual: Vec<(usize, usize)> = super::descendants_of_kind(statement, kind)
                    .iter()
                    .map(|node| (node.start_byte(), node.end_byte()))
                    .collect();
                assert_eq!(actual, expected, "kind {kind:?} differs within a statement");
            }
        }
    }

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
    fn accepts_fulltext_index_variants() {
        // Reported ``Invalid SurrealQL syntax near `FULLTEXT ANALYZER english
        // BM25`.`` at the `df12d94` grammar pin, whose `IndexClause` knew only
        // the pre-3.0 `SEARCH ANALYZER` spelling. The engine reads `ANALYZER`,
        // `BM25 [(k1, b)]` and `HIGHLIGHTS` in any order and requires none.
        let uri = Uri::from_str("file:///workspace/search.surql").expect("valid uri");
        let text = r#"
        DEFINE ANALYZER english TOKENIZERS class FILTERS snowball(english);
        DEFINE INDEX OVERWRITE article_body_search  ON article FIELDS body FULLTEXT ANALYZER english BM25;
        DEFINE INDEX blog_title ON blog FIELDS title FULLTEXT ANALYZER english BM25(1.2,0.75) HIGHLIGHTS;
        DEFINE INDEX i ON b FIELDS t FULLTEXT HIGHLIGHTS BM25 ANALYZER english;
        DEFINE INDEX j ON b FIELDS t FULLTEXT BM25 HIGHLIGHTS;
        "#;

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");

        assert!(
            analysis.syntax_diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            analysis.syntax_diagnostics
        );
        assert_eq!(analysis.analyzers.len(), 1);
        assert_eq!(analysis.indexes.len(), 4);
        assert_eq!(analysis.indexes[0].name, "article_body_search");
        assert_eq!(analysis.indexes[0].table, "article");
        assert_eq!(analysis.indexes[0].fields, vec!["body".to_string()]);
        assert!(!analysis.indexes[0].unique);
        let options: Vec<Vec<String>> = analysis
            .indexes
            .iter()
            .map(|index| index.options.clone())
            .collect();
        assert_eq!(
            options,
            vec![
                vec!["FULLTEXT ANALYZER english BM25".to_string()],
                vec!["FULLTEXT ANALYZER english BM25(1.2,0.75) HIGHLIGHTS".to_string()],
                vec!["FULLTEXT HIGHLIGHTS BM25 ANALYZER english".to_string()],
                vec!["FULLTEXT BM25 HIGHLIGHTS".to_string()],
            ]
        );
    }

    #[test]
    fn accepts_count_index_variants() {
        // `COUNT [WHERE <condition>]` takes no field list — the engine rejects
        // one. `CONCURRENTLY` is a clause of its own and is captured as an
        // option, as it is for every other index kind.
        let uri = Uri::from_str("file:///workspace/count.surql").expect("valid uri");
        let text = r#"
        DEFINE INDEX idx_count ON t COUNT;
        DEFINE INDEX item_active_count ON item COUNT WHERE status = "active" CONCURRENTLY;
        DEFINE INDEX idx ON users COUNT COMMENT "Users expected to grow" CONCURRENTLY;
        "#;

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");

        assert!(
            analysis.syntax_diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            analysis.syntax_diagnostics
        );
        assert_eq!(analysis.indexes.len(), 3);
        assert!(
            analysis
                .indexes
                .iter()
                .all(|index| index.fields.is_empty() && !index.unique)
        );
        let options: Vec<Vec<String>> = analysis
            .indexes
            .iter()
            .map(|index| index.options.clone())
            .collect();
        assert_eq!(
            options,
            vec![
                vec!["COUNT".to_string()],
                vec![
                    "COUNT WHERE status = \"active\"".to_string(),
                    "CONCURRENTLY".to_string()
                ],
                vec!["COUNT".to_string(), "CONCURRENTLY".to_string()],
            ]
        );
    }

    #[test]
    fn accepts_diskann_and_hashed_vector_index_variants() {
        // `DISKANN` was a declared completion the grammar could not parse;
        // `HASHED_VECTOR` and the `DISTANCE` spelling of `DIST` were missing
        // from `HNSW` too. The first option string is longer than the preview
        // cap, so it is checked by prefix.
        let uri = Uri::from_str("file:///workspace/vector.surql").expect("valid uri");
        let text = r#"
        DEFINE INDEX diskann_pts ON pts FIELDS point DISKANN DIMENSION 4 DIST EUCLIDEAN TYPE F32 DEGREE 8 L_BUILD 20 ALPHA 1.4 HASHED_VECTOR;
        DEFINE INDEX emb ON embeddings FIELDS vec DISKANN DIMENSION 8 DISTANCE INNER_PRODUCT TYPE F16;
        DEFINE INDEX idx_embedding ON TABLE test FIELDS embedding HNSW DIMENSION 3 DISTANCE COSINE HASHED_VECTOR;
        "#;

        let analysis = analyze_document(uri, text, SymbolOrigin::Local).expect("analysis");

        assert!(
            analysis.syntax_diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            analysis.syntax_diagnostics
        );
        assert_eq!(analysis.indexes.len(), 3);
        assert_eq!(analysis.indexes[0].fields, vec!["point".to_string()]);
        assert_eq!(analysis.indexes[0].options.len(), 1);
        assert!(
            analysis.indexes[0].options[0].starts_with(
                "DISKANN DIMENSION 4 DIST EUCLIDEAN TYPE F32 DEGREE 8 L_BUILD 20 ALPHA 1.4"
            ),
            "got {:?}",
            analysis.indexes[0].options
        );
        assert_eq!(
            analysis.indexes[1].options,
            vec!["DISKANN DIMENSION 8 DISTANCE INNER_PRODUCT TYPE F16".to_string()]
        );
        assert_eq!(
            analysis.indexes[2].options,
            vec!["HNSW DIMENSION 3 DISTANCE COSINE HASHED_VECTOR".to_string()]
        );
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
