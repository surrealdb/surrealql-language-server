use std::collections::HashMap;

use strsim::jaro_winkler;
use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CompletionItem, CompletionItemKind,
    Diagnostic, DiagnosticSeverity, DocumentChanges, Documentation, Location, MarkupContent,
    MarkupKind, OneOf, OptionalVersionedTextDocumentIdentifier, Position, Range, TextDocumentEdit,
    TextEdit, Url, WorkspaceEdit,
};

use crate::config::{AuthContext, ServerSettings};
use crate::grammar::{
    BUILTIN_FUNCTIONS, BUILTIN_NAMESPACES, BuiltinFunction, KEYWORDS, SPECIAL_VARIABLES,
    builtin_function, builtin_namespace,
};
use crate::semantic::text::compact_preview;
use crate::semantic::type_expr::TypeExpr;
use crate::semantic::types::{
    AccessDef, AccessResult, DocumentAnalysis, EventDef, FieldDef, FunctionDef, FunctionLanguage,
    IndexDef, LiveMetadataSnapshot, MergedSemanticModel, ParamDef, PermissionMode, PermissionRule,
    QueryAction, QueryFact, SymbolOrigin, TableDef, WorkspaceIndex,
};

impl MergedSemanticModel {
    pub fn build(workspace: &WorkspaceIndex, live: &LiveMetadataSnapshot) -> Self {
        let mut model = Self::default();

        for analysis in workspace.documents.values() {
            model.absorb_analysis(analysis);
        }
        for analysis in live.documents.values() {
            model.absorb_analysis(analysis);
        }

        for analysis in workspace.documents.values() {
            for reference in &analysis.references {
                if reference.kind == tower_lsp::lsp_types::SymbolKind::FUNCTION {
                    model
                        .function_references
                        .entry(reference.name.clone())
                        .or_default()
                        .push(reference.location.clone());
                }
            }
        }

        let function_names = model.functions.keys().cloned().collect::<Vec<_>>();
        for name in function_names {
            if let Some(function) = model.functions.get(&name) {
                for callee in &function.called_functions {
                    model
                        .function_callers
                        .entry(callee.clone())
                        .or_default()
                        .push(name.clone());
                }
            }
        }

        model
    }

    pub fn table_names_by_priority(&self) -> Vec<&TableDef> {
        let mut tables = self.tables.values().collect::<Vec<_>>();
        tables.sort_by(|left, right| {
            symbol_priority(right.origin)
                .cmp(&symbol_priority(left.origin))
                .then_with(|| left.name.cmp(&right.name))
        });
        tables
    }

    pub fn fields_for_table(&self, table: &str) -> Vec<&FieldDef> {
        let mut fields = self
            .fields
            .values()
            .filter(|field| field.table == table)
            .collect::<Vec<_>>();
        fields.sort_by(|left, right| {
            symbol_priority(right.origin)
                .cmp(&symbol_priority(left.origin))
                .then_with(|| left.name.cmp(&right.name))
        });
        fields
    }

    pub fn events_for_table(&self, table: &str) -> Vec<&EventDef> {
        let mut events = self
            .events
            .values()
            .filter(|event| event.table == table)
            .collect::<Vec<_>>();
        events.sort_by(|left, right| {
            symbol_priority(right.origin)
                .cmp(&symbol_priority(left.origin))
                .then_with(|| left.name.cmp(&right.name))
        });
        events
    }

    pub fn indexes_for_table(&self, table: &str) -> Vec<&IndexDef> {
        let mut indexes = self
            .indexes
            .values()
            .filter(|index| index.table == table)
            .collect::<Vec<_>>();
        indexes.sort_by(|left, right| {
            symbol_priority(right.origin)
                .cmp(&symbol_priority(left.origin))
                .then_with(|| left.name.cmp(&right.name))
        });
        indexes
    }

    pub fn find_nearest_table(&self, unknown: &str) -> Option<&TableDef> {
        self.tables
            .values()
            .map(|table| (table, jaro_winkler(unknown, &table.name)))
            .filter(|(_, score)| *score > 0.86)
            .max_by(|left, right| {
                left.1
                    .partial_cmp(&right.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(table, _)| table)
    }

    pub fn hover_markdown_for_token(
        &self,
        token: &str,
        active_context: Option<&AuthContext>,
    ) -> Option<String> {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Some(table) = self.tables.get(trimmed) {
            return Some(format_table_hover(table, self, active_context));
        }
        if let Some(function) = self.functions.get(trimmed) {
            return Some(format_function_hover(function));
        }
        if let Some(function) = builtin_function(trimmed) {
            return Some(format_builtin_function_hover(function, trimmed));
        }
        if let Some(param) = self.params.get(trimmed) {
            return Some(format_param_hover(param));
        }
        if let Some(access) = self.accesses.get(trimmed) {
            return Some(format_access_hover(access));
        }
        let parsed_type = TypeExpr::parse(trimmed);
        let record_tables = parsed_type.record_tables();
        if record_tables.len() == 1 {
            if let Some(table) = self.tables.get(&record_tables[0]) {
                return Some(join_hover_blocks([
                    hover_block(
                        format!("`{parsed_type}`"),
                        None,
                        vec!["Source: type expression".to_string()],
                        vec!["Resolves to:".to_string()],
                    ),
                    format_table_hover(table, self, active_context),
                ]));
            }
        }
        if KEYWORDS
            .iter()
            .any(|keyword| keyword.eq_ignore_ascii_case(trimmed))
        {
            return Some(hover_block(
                format!("`{trimmed}`"),
                Some("SurrealQL keyword.".to_string()),
                vec!["Source: builtin".to_string()],
                Vec::new(),
            ));
        }
        if let Some(namespace) = builtin_namespace(trimmed) {
            return Some(hover_block(
                format!("`{}` builtin namespace", namespace.name),
                Some(namespace.summary.to_string()),
                vec!["Source: builtin".to_string()],
                vec![format!("[Docs]({})", namespace.documentation_url)],
            ));
        }
        if BUILTIN_NAMESPACES
            .iter()
            .any(|namespace| namespace.eq_ignore_ascii_case(trimmed))
        {
            return Some(hover_block(
                format!("`{trimmed}` builtin namespace"),
                None,
                vec!["Source: builtin".to_string()],
                Vec::new(),
            ));
        }
        if let Some((_, description)) = SPECIAL_VARIABLES
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(trimmed))
        {
            return Some(hover_block(
                format!("`{trimmed}`"),
                Some((*description).to_string()),
                vec!["Source: builtin".to_string()],
                Vec::new(),
            ));
        }
        None
    }

    pub fn completion_items(
        &self,
        prefix: &str,
        record_type_context: bool,
        active_context: Option<&AuthContext>,
        statement_fact: Option<&QueryFact>,
        qualifier: Option<&str>,
    ) -> Vec<CompletionItem> {
        let mut items = Vec::new();
        let normalized = prefix.to_ascii_uppercase();
        let normalized_builtin = prefix.to_ascii_lowercase();

        if !record_type_context {
            for keyword in KEYWORDS {
                if normalized.is_empty() || keyword.starts_with(&normalized) {
                    items.push(CompletionItem {
                        label: keyword.to_string(),
                        kind: Some(CompletionItemKind::KEYWORD),
                        detail: Some("SurrealQL keyword".to_string()),
                        insert_text: Some(keyword.to_string()),
                        ..CompletionItem::default()
                    });
                }
            }

            for namespace in BUILTIN_NAMESPACES {
                if prefix.is_empty() || namespace.starts_with(&normalized_builtin) {
                    items.push(CompletionItem {
                        label: namespace.to_string(),
                        kind: Some(CompletionItemKind::MODULE),
                        detail: Some("Builtin function namespace".to_string()),
                        insert_text: Some(namespace.to_string()),
                        ..CompletionItem::default()
                    });
                }
            }

            for function in self.functions.values() {
                if prefix.is_empty() || function.name.starts_with(prefix) {
                    items.push(CompletionItem {
                        label: function.name.clone(),
                        kind: Some(CompletionItemKind::FUNCTION),
                        detail: Some(function_signature(function)),
                        documentation: Some(Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format_function_hover(function),
                        })),
                        sort_text: Some(format!("1-{}", function.name)),
                        ..CompletionItem::default()
                    });
                }
            }

            for function in BUILTIN_FUNCTIONS {
                if prefix.is_empty() || function.name.starts_with(&normalized_builtin) {
                    items.push(CompletionItem {
                        label: function.name.to_string(),
                        kind: Some(CompletionItemKind::FUNCTION),
                        detail: Some(function.signature.to_string()),
                        documentation: Some(Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format_builtin_function_hover(function, function.name),
                        })),
                        sort_text: Some(format!("2-{}", function.name)),
                        ..CompletionItem::default()
                    });
                }
            }
        }

        for table in self.table_names_by_priority() {
            if prefix.is_empty() || table.name.starts_with(prefix) {
                items.push(CompletionItem {
                    label: table.name.clone(),
                    kind: Some(if record_type_context {
                        CompletionItemKind::TYPE_PARAMETER
                    } else {
                        CompletionItemKind::STRUCT
                    }),
                    detail: Some(format!(
                        "{} schema, source: {}",
                        table
                            .schema_mode
                            .clone()
                            .unwrap_or_else(|| "inferred".to_string()),
                        origin_label(table.origin)
                    )),
                    documentation: Some(Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: format_table_hover(table, self, active_context),
                    })),
                    sort_text: Some(format!(
                        "0-{}-{}",
                        symbol_priority(table.origin),
                        table.name
                    )),
                    ..CompletionItem::default()
                });
            }
        }

        if !record_type_context {
            let field_tables = field_completion_tables(statement_fact, qualifier);
            let multi_table_context = qualifier.is_none() && field_tables.len() > 1;

            for table_name in field_tables {
                for field in self.fields_for_table(&table_name) {
                    let qualified_label = format!("{}.{}", field.table, field.name);
                    let matches_prefix = prefix.is_empty()
                        || field.name.starts_with(prefix)
                        || (multi_table_context && qualified_label.starts_with(prefix));
                    if !matches_prefix {
                        continue;
                    }

                    let label = if multi_table_context {
                        qualified_label.clone()
                    } else {
                        field.name.clone()
                    };
                    let insert_text = if multi_table_context {
                        qualified_label
                    } else {
                        field.name.clone()
                    };
                    let mut detail = vec![format!("table: {}", field.table)];
                    if let Some(type_expr) = &field.type_expr {
                        detail.push(format!("type: {type_expr}"));
                    }
                    detail.push(format!("source: {}", origin_label(field.origin)));

                    items.push(CompletionItem {
                        label,
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some(detail.join(" | ")),
                        insert_text: Some(insert_text),
                        documentation: Some(Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format_field_hover(field),
                        })),
                        sort_text: Some(format!(
                            "0f-{}-{}-{}",
                            symbol_priority(field.origin),
                            field.table,
                            field.name
                        )),
                        ..CompletionItem::default()
                    });
                }
            }
        }

        for (name, description) in SPECIAL_VARIABLES {
            if prefix.is_empty() || name.starts_with(prefix) {
                items.push(CompletionItem {
                    label: (*name).to_string(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some("Special SurrealQL variable".to_string()),
                    documentation: Some(Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: (*description).to_string(),
                    })),
                    ..CompletionItem::default()
                });
            }
        }

        items.sort_by(|left, right| {
            left.sort_text
                .cmp(&right.sort_text)
                .then_with(|| left.label.cmp(&right.label))
        });
        items
    }

    pub fn semantic_diagnostics(
        &self,
        analysis: &DocumentAnalysis,
        settings: &ServerSettings,
    ) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        let active_context = settings.active_auth_context();

        for fact in analysis.query_facts.iter() {
            if fact.target_tables.is_empty() {
                diagnostics.push(Diagnostic {
                    range: fact.location.range,
                    severity: Some(DiagnosticSeverity::WARNING),
                    source: Some("surreal-language-server".to_string()),
                    message: format!(
                        "{} target could not be resolved statically.",
                        action_label(fact.action)
                    ),
                    ..Diagnostic::default()
                });
                continue;
            }

            for table in &fact.target_tables {
                let Some(table_def) = self.tables.get(table) else {
                    diagnostics.push(Diagnostic {
                        range: fact.location.range,
                        severity: Some(DiagnosticSeverity::WARNING),
                        source: Some("surreal-language-server".to_string()),
                        message: format!("Unknown table `{table}`."),
                        ..Diagnostic::default()
                    });
                    continue;
                };

                let permission = self.evaluate_permissions(fact, table_def, active_context);
                match permission.result {
                    AccessResult::Denied => diagnostics.push(Diagnostic {
                        range: fact.location.range,
                        severity: Some(DiagnosticSeverity::ERROR),
                        source: Some("surreal-language-server".to_string()),
                        message: permission.message,
                        ..Diagnostic::default()
                    }),
                    AccessResult::Unknown => diagnostics.push(Diagnostic {
                        range: fact.location.range,
                        severity: Some(DiagnosticSeverity::WARNING),
                        source: Some("surreal-language-server".to_string()),
                        message: permission.message,
                        ..Diagnostic::default()
                    }),
                    AccessResult::Allowed => {}
                }

                for field in &fact.touched_fields {
                    if self.fields.get(&(table.clone(), field.clone())).is_none()
                        && table_def.explicit
                    {
                        diagnostics.push(Diagnostic {
                            range: fact.location.range,
                            severity: Some(DiagnosticSeverity::WARNING),
                            source: Some("surreal-language-server".to_string()),
                            message: format!("Unknown field `{table}.{field}`."),
                            ..Diagnostic::default()
                        });
                    }
                }
            }
        }

        diagnostics
    }

    pub fn code_actions(
        &self,
        uri: &Url,
        analysis: &DocumentAnalysis,
        diagnostics: &[Diagnostic],
    ) -> Vec<CodeActionOrCommand> {
        let mut actions = Vec::new();

        for diagnostic in diagnostics {
            if let Some(table) = diagnostic
                .message
                .strip_prefix("Unknown table `")
                .and_then(|message| message.strip_suffix("`."))
            {
                if let Some(replacement) = self.find_nearest_table(table) {
                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title: format!("Replace `{table}` with `{}`", replacement.name),
                        kind: Some(CodeActionKind::QUICKFIX),
                        diagnostics: Some(vec![diagnostic.clone()]),
                        edit: Some(WorkspaceEdit {
                            document_changes: Some(DocumentChanges::Operations(vec![
                                tower_lsp::lsp_types::DocumentChangeOperation::Edit(
                                    TextDocumentEdit {
                                        text_document: OptionalVersionedTextDocumentIdentifier {
                                            uri: uri.clone(),
                                            version: None,
                                        },
                                        edits: vec![OneOf::Left(TextEdit {
                                            range: diagnostic.range,
                                            new_text: replacement.name.clone(),
                                        })],
                                    },
                                ),
                            ])),
                            ..WorkspaceEdit::default()
                        }),
                        ..CodeAction::default()
                    }));
                }
            }
        }

        for table in analysis
            .tables
            .iter()
            .filter(|table| table.permissions.is_empty() && table.explicit)
        {
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Add PERMISSIONS clause to table `{}`", table.name),
                kind: Some(CodeActionKind::REFACTOR_REWRITE),
                edit: Some(WorkspaceEdit {
                    document_changes: Some(DocumentChanges::Operations(vec![tower_lsp::lsp_types::DocumentChangeOperation::Edit(
                        TextDocumentEdit {
                            text_document: OptionalVersionedTextDocumentIdentifier {
                                uri: uri.clone(),
                                version: None,
                            },
                            edits: vec![OneOf::Left(TextEdit {
                                range: Range {
                                    start: table.location.range.end,
                                    end: table.location.range.end,
                                },
                                new_text: " PERMISSIONS FOR select FULL, create NONE, update NONE, delete NONE".to_string(),
                            })],
                        },
                    )])),
                    ..WorkspaceEdit::default()
                }),
                ..CodeAction::default()
            }));
        }

        actions
    }

    pub fn definition_for_function(&self, name: &str) -> Option<Location> {
        self.functions
            .get(name)
            .filter(|function| function.origin == SymbolOrigin::Local)
            .map(|function| Location::new(function.location.uri.clone(), function.selection_range))
    }

    pub fn definition_for_token(&self, token: &str) -> Option<Location> {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            return None;
        }

        self.definition_for_function(trimmed)
            .or_else(|| {
                self.tables
                    .get(trimmed)
                    .filter(|table| table.origin == SymbolOrigin::Local)
                    .map(|table| table.location.clone())
            })
            .or_else(|| {
                self.params
                    .get(trimmed)
                    .filter(|param| param.origin == SymbolOrigin::Local)
                    .map(|param| param.location.clone())
            })
            .or_else(|| {
                let parsed_type = TypeExpr::parse(trimmed);
                let record_tables = parsed_type.record_tables();
                (record_tables.len() == 1)
                    .then(|| record_tables.into_iter().next())
                    .flatten()
                    .and_then(|table_name| {
                        self.tables
                            .get(&table_name)
                            .filter(|table| table.origin == SymbolOrigin::Local)
                            .map(|table| table.location.clone())
                    })
            })
    }

    pub fn references_for_function(&self, name: &str) -> Vec<Location> {
        self.function_references
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    pub fn rename_edits(&self, name: &str, new_name: &str) -> Option<HashMap<Url, Vec<TextEdit>>> {
        let function = self.functions.get(name)?;
        if function.origin != SymbolOrigin::Local {
            return None;
        }

        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        changes
            .entry(function.location.uri.clone())
            .or_default()
            .push(TextEdit {
                range: function.selection_range,
                new_text: new_name.to_string(),
            });

        for location in self.references_for_function(name) {
            changes
                .entry(location.uri.clone())
                .or_default()
                .push(TextEdit {
                    range: location.range,
                    new_text: new_name.to_string(),
                });
        }

        Some(changes)
    }

    pub fn workspace_symbol_items(
        &self,
        query: &str,
    ) -> Vec<tower_lsp::lsp_types::SymbolInformation> {
        let needle = query.to_ascii_lowercase();
        let mut items = Vec::new();
        for table in self.tables.values() {
            if needle.is_empty() || table.name.to_ascii_lowercase().contains(&needle) {
                items.push(symbol_information(
                    &table.name,
                    tower_lsp::lsp_types::SymbolKind::STRUCT,
                    &table.location,
                ));
            }
        }
        for field in self.fields.values() {
            let label = format!("{}.{}", field.table, field.name);
            if needle.is_empty() || label.to_ascii_lowercase().contains(&needle) {
                items.push(symbol_information(
                    &label,
                    tower_lsp::lsp_types::SymbolKind::FIELD,
                    &field.location,
                ));
            }
        }
        for event in self.events.values() {
            let label = format!("{}.{}", event.table, event.name);
            if needle.is_empty() || label.to_ascii_lowercase().contains(&needle) {
                items.push(symbol_information(
                    &label,
                    tower_lsp::lsp_types::SymbolKind::EVENT,
                    &event.location,
                ));
            }
        }
        for index in self.indexes.values() {
            let label = format!("{}.{}", index.table, index.name);
            if needle.is_empty() || label.to_ascii_lowercase().contains(&needle) {
                items.push(symbol_information(
                    &label,
                    tower_lsp::lsp_types::SymbolKind::KEY,
                    &index.location,
                ));
            }
        }
        for function in self.functions.values() {
            if needle.is_empty() || function.name.to_ascii_lowercase().contains(&needle) {
                items.push(symbol_information(
                    &function.name,
                    tower_lsp::lsp_types::SymbolKind::FUNCTION,
                    &function.location,
                ));
            }
        }
        items
    }

    fn absorb_analysis(&mut self, analysis: &DocumentAnalysis) {
        self.query_facts
            .entry(analysis.uri.clone())
            .or_default()
            .extend(analysis.query_facts.clone());
        self.workspace_symbols
            .extend(analysis.document_symbols.iter().cloned());

        for table in &analysis.tables {
            merge_table(&mut self.tables, table.clone());
        }
        for event in &analysis.events {
            merge_event(&mut self.events, event.clone());
        }
        for index in &analysis.indexes {
            merge_index(&mut self.indexes, index.clone());
        }
        for field in &analysis.fields {
            merge_field(&mut self.fields, field.clone());
        }
        for function in &analysis.functions {
            merge_function(&mut self.functions, function.clone());
        }
        for param in &analysis.params {
            merge_param(&mut self.params, param.clone());
        }
        for access in &analysis.accesses {
            merge_access(&mut self.accesses, access.clone());
        }
    }

    fn evaluate_permissions(
        &self,
        fact: &QueryFact,
        table: &TableDef,
        active_context: Option<&AuthContext>,
    ) -> PermissionOutcome {
        let table_rule = table
            .permissions
            .iter()
            .find(|rule| rule.actions.contains(&fact.action))
            .cloned();

        let mut field_rule = None;
        for field in &fact.touched_fields {
            if let Some(rule) = self
                .fields
                .get(&(table.name.clone(), field.clone()))
                .and_then(|field| {
                    field
                        .permissions
                        .iter()
                        .find(|rule| rule.actions.contains(&fact.action))
                })
                .cloned()
            {
                field_rule = Some(rule);
                break;
            }
        }

        let rule = field_rule.or(table_rule);
        let Some(rule) = rule else {
            return PermissionOutcome {
                result: AccessResult::Unknown,
                message: format!(
                    "No explicit permission rule found for {} on `{}`.",
                    action_label(fact.action),
                    table.name
                ),
            };
        };

        let result = evaluate_permission_rule(&rule, active_context);
        let message = match result {
            AccessResult::Allowed => format!(
                "{} is allowed on `{}` for `{}`.",
                action_label(fact.action),
                table.name,
                active_context
                    .map(|context| context.name.as_str())
                    .unwrap_or("default")
            ),
            AccessResult::Denied => format!(
                "{} is denied on `{}` by `{}`.",
                action_label(fact.action),
                table.name,
                compact_preview(&rule.raw)
            ),
            AccessResult::Unknown => format!(
                "{} on `{}` depends on unresolved permission expression `{}`.",
                action_label(fact.action),
                table.name,
                compact_preview(&rule.raw)
            ),
        };

        PermissionOutcome { result, message }
    }
}

struct PermissionOutcome {
    result: AccessResult,
    message: String,
}

fn merge_table(target: &mut HashMap<String, TableDef>, candidate: TableDef) {
    let replace = target
        .get(&candidate.name)
        .map(|current| should_replace_table(current, &candidate))
        .unwrap_or(true);
    if replace {
        target.insert(candidate.name.clone(), candidate);
    }
}

fn merge_event(target: &mut HashMap<(String, String), EventDef>, candidate: EventDef) {
    let key = (candidate.table.clone(), candidate.name.clone());
    let replace = target
        .get(&key)
        .map(|current| symbol_priority(candidate.origin) >= symbol_priority(current.origin))
        .unwrap_or(true);
    if replace {
        target.insert(key, candidate);
    }
}

fn merge_index(target: &mut HashMap<(String, String), IndexDef>, candidate: IndexDef) {
    let key = (candidate.table.clone(), candidate.name.clone());
    let replace = target
        .get(&key)
        .map(|current| symbol_priority(candidate.origin) >= symbol_priority(current.origin))
        .unwrap_or(true);
    if replace {
        target.insert(key, candidate);
    }
}

fn merge_field(target: &mut HashMap<(String, String), FieldDef>, candidate: FieldDef) {
    let key = (candidate.table.clone(), candidate.name.clone());
    let replace = target
        .get(&key)
        .map(|current| should_replace_field(current, &candidate))
        .unwrap_or(true);
    if replace {
        target.insert(key, candidate);
    }
}

fn merge_function(target: &mut HashMap<String, FunctionDef>, candidate: FunctionDef) {
    let replace = target
        .get(&candidate.name)
        .map(|current| should_replace_function(current, &candidate))
        .unwrap_or(true);
    if replace {
        target.insert(candidate.name.clone(), candidate);
    }
}

fn merge_param(target: &mut HashMap<String, ParamDef>, candidate: ParamDef) {
    let replace = target
        .get(&candidate.name)
        .map(|current| symbol_priority(candidate.origin) >= symbol_priority(current.origin))
        .unwrap_or(true);
    if replace {
        target.insert(candidate.name.clone(), candidate);
    }
}

fn merge_access(target: &mut HashMap<String, AccessDef>, candidate: AccessDef) {
    let replace = target
        .get(&candidate.name)
        .map(|current| symbol_priority(candidate.origin) >= symbol_priority(current.origin))
        .unwrap_or(true);
    if replace {
        target.insert(candidate.name.clone(), candidate);
    }
}

fn should_replace_table(current: &TableDef, candidate: &TableDef) -> bool {
    replacement_score(
        candidate.explicit,
        candidate.origin,
        candidate
            .inference
            .as_ref()
            .map(|fact| fact.confidence)
            .unwrap_or(1.0),
    ) >= replacement_score(
        current.explicit,
        current.origin,
        current
            .inference
            .as_ref()
            .map(|fact| fact.confidence)
            .unwrap_or(1.0),
    )
}

fn should_replace_field(current: &FieldDef, candidate: &FieldDef) -> bool {
    replacement_score(
        candidate.explicit,
        candidate.origin,
        candidate
            .inference
            .as_ref()
            .map(|fact| fact.confidence)
            .unwrap_or(1.0),
    ) >= replacement_score(
        current.explicit,
        current.origin,
        current
            .inference
            .as_ref()
            .map(|fact| fact.confidence)
            .unwrap_or(1.0),
    )
}

fn should_replace_function(current: &FunctionDef, candidate: &FunctionDef) -> bool {
    replacement_score(
        candidate.explicit,
        candidate.origin,
        candidate
            .inference
            .as_ref()
            .map(|fact| fact.confidence)
            .unwrap_or(1.0),
    ) >= replacement_score(
        current.explicit,
        current.origin,
        current
            .inference
            .as_ref()
            .map(|fact| fact.confidence)
            .unwrap_or(1.0),
    )
}

fn replacement_score(explicit: bool, origin: SymbolOrigin, confidence: f32) -> i32 {
    let explicit_score = if explicit { 1000 } else { 0 };
    explicit_score + (symbol_priority(origin) as i32 * 100) + (confidence * 10.0) as i32
}

fn symbol_priority(origin: SymbolOrigin) -> usize {
    match origin {
        SymbolOrigin::Local => 4,
        SymbolOrigin::Remote => 3,
        SymbolOrigin::Inferred => 2,
        SymbolOrigin::Builtin => 1,
    }
}

fn format_table_hover(
    table: &TableDef,
    model: &MergedSemanticModel,
    active_context: Option<&AuthContext>,
) -> String {
    let mut metadata = vec![format!("Source: {}", origin_label(table.origin))];
    if let Some(mode) = &table.schema_mode {
        metadata.push(format!("Schema: `{mode}`"));
    }
    metadata.push(format!(
        "Permissions: {}",
        table_permission_posture(&table.permissions)
    ));
    let mut sections = Vec::new();
    let field_count = model.fields_for_table(&table.name).len();
    if field_count > 0 {
        sections.push(list_section("Known fields", vec![field_count.to_string()]));
    }
    let indexes = model.indexes_for_table(&table.name);
    if !indexes.is_empty() {
        sections.push(list_section(
            "Known indexes",
            indexes
                .iter()
                .map(|index| {
                    let mut details = Vec::new();
                    if !index.fields.is_empty() {
                        details.push(index.fields.join(", "));
                    }
                    if index.unique {
                        details.push("unique".to_string());
                    }
                    details.extend(index.options.iter().cloned());

                    if details.is_empty() {
                        index.name.clone()
                    } else {
                        format!("{} ({})", index.name, details.join(" | "))
                    }
                })
                .collect::<Vec<_>>(),
        ));
    }
    let events = model.events_for_table(&table.name);
    if !events.is_empty() {
        sections.push(list_section(
            "Known events",
            events
                .iter()
                .map(|event| event.name.clone())
                .collect::<Vec<_>>(),
        ));
    }
    if let Some(context) = active_context {
        let actions = table
            .permissions
            .iter()
            .map(|rule| {
                let action_list = rule
                    .actions
                    .iter()
                    .map(|action| action_label(*action))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{action_list}: {}", permission_summary(rule, Some(context)))
            })
            .collect::<Vec<_>>();
        if !actions.is_empty() {
            sections.push(list_section(
                &format!("Permissions for `{}`", context.name),
                actions,
            ));
        }
    }
    if let Some(inference) = &table.inference {
        metadata.push(format!("Confidence: {:.2}", inference.confidence));
    }
    hover_block(
        format!("TABLE {}", table.name),
        table.comment.clone(),
        metadata,
        sections,
    )
}

fn format_function_hover(function: &FunctionDef) -> String {
    let mut metadata = vec![format!("Source: {}", origin_label(function.origin))];
    match function.language {
        FunctionLanguage::JavaScript => metadata.push("Language: JavaScript".to_string()),
        FunctionLanguage::SurrealQL => {}
    }
    let mut sections = Vec::new();
    if !function.called_functions.is_empty() {
        sections.push(list_section("Calls", function.called_functions.clone()));
    }
    hover_block(function_signature(function), function.comment.clone(), metadata, sections)
}

fn format_builtin_function_hover(function: &BuiltinFunction, token: &str) -> String {
    let mut metadata = vec!["Source: builtin".to_string()];
    if !token.eq_ignore_ascii_case(function.name) {
        metadata.push(format!("Canonical name: `{}`", function.name));
    }
    hover_block(
        function.signature.to_string(),
        Some(function.summary.to_string()),
        metadata,
        vec![list_section(
            "Docs",
            vec![format!(
                "[SurrealDB reference]({})",
                function.documentation_url
            )],
        )],
    )
}

fn format_param_hover(param: &ParamDef) -> String {
    let mut sections = Vec::new();
    if let Some(value_preview) = &param.value_preview {
        sections.push(list_section("Default", vec![format!("`{value_preview}`")]));
    }
    hover_block(
        format!("PARAM {}", param.name),
        param.comment.clone(),
        vec![format!("Source: {}", origin_label(param.origin))],
        sections,
    )
}

fn format_access_hover(access: &AccessDef) -> String {
    hover_block(
        format!("ACCESS {}", access.name),
        access.comment.clone(),
        vec![format!("Source: {}", origin_label(access.origin))],
        Vec::new(),
    )
}

fn format_field_hover(field: &FieldDef) -> String {
    let mut metadata = vec![
        format!("Source: {}", origin_label(field.origin)),
        format!(
            "Permissions: {}",
            table_permission_posture(&field.permissions)
        ),
    ];
    if let Some(type_expr) = &field.type_expr {
        metadata.push(format!("Type: `{type_expr}`"));
    }
    if let Some(inference) = &field.inference {
        metadata.push(format!("Confidence: {:.2}", inference.confidence));
    }
    hover_block(
        format!("FIELD {}.{}", field.table, field.name),
        field.comment.clone(),
        metadata,
        Vec::new(),
    )
}

fn function_signature(function: &FunctionDef) -> String {
    let params = function
        .params
        .iter()
        .map(|param| match &param.type_expr {
            Some(type_expr) => format!("{}: {}", param.name, type_expr),
            None => param.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let base = format!("{}({params})", function.name);
    match &function.return_type {
        Some(ret) => format!("{base} -> {ret}"),
        None => base,
    }
}

fn table_permission_posture(permissions: &[PermissionRule]) -> &'static str {
    if permissions.is_empty() {
        "no explicit rules"
    } else if permissions
        .iter()
        .all(|rule| matches!(rule.mode, PermissionMode::Full))
    {
        "public"
    } else {
        "gated"
    }
}

fn hover_block(
    title: String,
    summary: Option<String>,
    metadata: Vec<String>,
    sections: Vec<String>,
) -> String {
    let mut blocks = vec![format!("### {title}")];
    if let Some(summary) = summary.filter(|value| !value.trim().is_empty()) {
        blocks.push(summary);
    }
    if !metadata.is_empty() {
        blocks.push(list_section("Details", metadata));
    }
    blocks.extend(
        sections
            .into_iter()
            .filter(|value| !value.trim().is_empty()),
    );
    join_hover_blocks(blocks)
}

fn join_hover_blocks<I>(blocks: I) -> String
where
    I: IntoIterator<Item = String>,
{
    blocks
        .into_iter()
        .filter(|block| !block.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn list_section(title: &str, items: Vec<String>) -> String {
    let mut lines = vec![format!("**{title}**")];
    lines.extend(
        items
            .into_iter()
            .filter(|item| !item.trim().is_empty())
            .map(|item| format!("- {item}")),
    );
    lines.join("\n")
}

fn permission_summary(rule: &PermissionRule, active_context: Option<&AuthContext>) -> String {
    match evaluate_permission_rule(rule, active_context) {
        AccessResult::Allowed => "allowed".to_string(),
        AccessResult::Denied => "denied".to_string(),
        AccessResult::Unknown => compact_preview(&rule.raw),
    }
}

fn evaluate_permission_rule(
    rule: &PermissionRule,
    active_context: Option<&AuthContext>,
) -> AccessResult {
    match &rule.mode {
        PermissionMode::Full => AccessResult::Allowed,
        PermissionMode::None => AccessResult::Denied,
        PermissionMode::Expression(expression) => {
            evaluate_permission_expression(expression, active_context)
        }
    }
}

fn evaluate_permission_expression(
    expression: &str,
    active_context: Option<&AuthContext>,
) -> AccessResult {
    let Some(context) = active_context else {
        return AccessResult::Unknown;
    };
    let lower = expression.to_ascii_lowercase();

    if lower.contains("$auth.roles") {
        let candidates = quoted_literals(expression);
        if candidates.is_empty() {
            return AccessResult::Unknown;
        }
        if candidates
            .iter()
            .any(|role| context.roles.iter().any(|owned| owned == role))
        {
            return AccessResult::Allowed;
        }
        return AccessResult::Denied;
    }

    if lower.contains("$auth.id") || lower.contains("$session") || lower.contains("$auth") {
        return AccessResult::Unknown;
    }

    AccessResult::Unknown
}

fn quoted_literals(input: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;

    for ch in input.chars() {
        match ch {
            '\'' if in_quote => {
                values.push(current.clone());
                current.clear();
                in_quote = false;
            }
            '\'' => in_quote = true,
            _ if in_quote => current.push(ch),
            _ => {}
        }
    }

    values
}

fn symbol_information(
    name: &str,
    kind: tower_lsp::lsp_types::SymbolKind,
    location: &Location,
) -> tower_lsp::lsp_types::SymbolInformation {
    #[allow(deprecated)]
    tower_lsp::lsp_types::SymbolInformation {
        name: name.to_string(),
        kind,
        tags: None,
        deprecated: None,
        location: location.clone(),
        container_name: None,
    }
}

fn origin_label(origin: SymbolOrigin) -> &'static str {
    match origin {
        SymbolOrigin::Builtin => "builtin",
        SymbolOrigin::Inferred => "inferred",
        SymbolOrigin::Remote => "remote",
        SymbolOrigin::Local => "local",
    }
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

fn field_completion_tables(
    statement_fact: Option<&QueryFact>,
    qualifier: Option<&str>,
) -> Vec<String> {
    if let Some(qualified) = qualifier.and_then(normalize_completion_table_name) {
        return vec![qualified];
    }

    let Some(statement_fact) = statement_fact else {
        return Vec::new();
    };
    if !matches!(
        statement_fact.action,
        QueryAction::Select | QueryAction::Create | QueryAction::Update
    ) {
        return Vec::new();
    }

    let mut tables = Vec::new();
    for table in &statement_fact.target_tables {
        if let Some(normalized) = normalize_completion_table_name(table) {
            if !tables.contains(&normalized) {
                tables.push(normalized);
            }
        }
    }
    tables
}

fn normalize_completion_table_name(value: &str) -> Option<String> {
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
    if candidate.is_empty() {
        None
    } else {
        Some(candidate)
    }
}

pub fn is_record_type_context(source: &str, position: Position) -> bool {
    let prefix = &source[..crate::semantic::text::position_to_offset(source, position)];
    prefix
        .rsplit_once("record<")
        .map(|(_, suffix)| !suffix.contains('>'))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use tower_lsp::lsp_types::{DiagnosticSeverity, Location, Position, Range, Url};

    use crate::config::{AuthContext, ServerSettings};
    use crate::semantic::types::{
        DocumentAnalysis, EventDef, FunctionDef, IndexDef, PermissionMode, PermissionRule,
        QueryAction, SymbolOrigin, TableDef, WorkspaceIndex,
    };

    use super::{MergedSemanticModel, is_record_type_context};

    #[test]
    fn local_definitions_override_inferred() {
        let uri = Url::parse("file:///workspace/schema.surql").expect("valid uri");
        let explicit = TableDef {
            name: "person".to_string(),
            schema_mode: Some("schemafull".to_string()),
            comment: None,
            permissions: Vec::new(),
            origin: SymbolOrigin::Local,
            explicit: true,
            inference: None,
            location: Location::new(uri.clone(), Range::default()),
        };
        let inferred = TableDef {
            name: "person".to_string(),
            schema_mode: None,
            comment: None,
            permissions: Vec::new(),
            origin: SymbolOrigin::Inferred,
            explicit: false,
            inference: None,
            location: Location::new(uri.clone(), Range::default()),
        };
        let analysis = DocumentAnalysis {
            uri,
            text: String::new(),
            tables: vec![inferred, explicit.clone()],
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
        let mut workspace = WorkspaceIndex::default();
        workspace.documents.insert(analysis.uri.clone(), analysis);
        let model = MergedSemanticModel::build(&workspace, &Default::default());
        assert_eq!(model.tables["person"].schema_mode, explicit.schema_mode);
    }

    #[test]
    fn evaluates_role_based_permissions() {
        let rule = PermissionRule {
            actions: vec![QueryAction::Select],
            mode: PermissionMode::Expression("WHERE $auth.roles CONTAINS 'viewer'".to_string()),
            raw: "WHERE $auth.roles CONTAINS 'viewer'".to_string(),
            origin: SymbolOrigin::Local,
            location: None,
        };
        let context = AuthContext {
            name: "viewer".to_string(),
            roles: vec!["viewer".to_string()],
            auth_record: None,
            claims: serde_json::Value::Object(Default::default()),
            session: serde_json::Value::Object(Default::default()),
            variables: serde_json::Value::Object(Default::default()),
        };
        let settings = ServerSettings {
            auth_contexts: vec![context.clone()],
            active_auth_context: Some("viewer".to_string()),
            ..ServerSettings::default()
        };
        let table = TableDef {
            name: "person".to_string(),
            schema_mode: None,
            comment: None,
            permissions: vec![rule],
            origin: SymbolOrigin::Local,
            explicit: true,
            inference: None,
            location: Location::new(
                Url::parse("file:///workspace/schema.surql").expect("valid uri"),
                Range::default(),
            ),
        };
        let mut model = MergedSemanticModel::default();
        model.tables.insert("person".to_string(), table);
        let fact = crate::semantic::types::QueryFact {
            action: QueryAction::Select,
            target_tables: vec!["person".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: Location::new(
                Url::parse("file:///workspace/query.surql").expect("valid uri"),
                Range::default(),
            ),
            source_preview: "SELECT * FROM person".to_string(),
        };
        let result = model.semantic_diagnostics(
            &DocumentAnalysis {
                uri: Url::parse("file:///workspace/query.surql").expect("valid uri"),
                text: String::new(),
                tables: Vec::new(),
                events: Vec::new(),
                indexes: Vec::new(),
                fields: Vec::new(),
                functions: Vec::new(),
                params: Vec::new(),
                accesses: Vec::new(),
                query_facts: vec![fact],
                references: Vec::new(),
                syntax_diagnostics: Vec::new(),
                document_symbols: Vec::new(),
            },
            &settings,
        );
        assert!(result.is_empty());
    }

    #[test]
    fn denied_permissions_produce_error_diagnostic() {
        let settings = ServerSettings::default();
        let table = TableDef {
            name: "person".to_string(),
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
            location: Location::new(
                Url::parse("file:///workspace/schema.surql").expect("valid uri"),
                Range::default(),
            ),
        };
        let mut model = MergedSemanticModel::default();
        model.tables.insert("person".to_string(), table);

        let diagnostics = model.semantic_diagnostics(
            &DocumentAnalysis {
                uri: Url::parse("file:///workspace/query.surql").expect("valid uri"),
                text: String::new(),
                tables: Vec::new(),
                events: Vec::new(),
                indexes: Vec::new(),
                fields: Vec::new(),
                functions: Vec::new(),
                params: Vec::new(),
                accesses: Vec::new(),
                query_facts: vec![crate::semantic::types::QueryFact {
                    action: QueryAction::Select,
                    target_tables: vec!["person".to_string()],
                    touched_fields: Vec::new(),
                    dynamic: false,
                    location: Location::new(
                        Url::parse("file:///workspace/query.surql").expect("valid uri"),
                        Range::default(),
                    ),
                    source_preview: "SELECT * FROM person".to_string(),
                }],
                references: Vec::new(),
                syntax_diagnostics: Vec::new(),
                document_symbols: Vec::new(),
            },
            &settings,
        );

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
    }

    #[test]
    fn record_type_hover_resolves_underlying_table() {
        let uri = Url::parse("file:///workspace/schema.surql").expect("valid uri");
        let mut workspace = WorkspaceIndex::default();
        workspace.documents.insert(
            uri.clone(),
            DocumentAnalysis {
                uri: uri.clone(),
                text: String::new(),
                tables: vec![TableDef {
                    name: "person".to_string(),
                    schema_mode: Some("schemafull".to_string()),
                    comment: Some("People".to_string()),
                    permissions: Vec::new(),
                    origin: SymbolOrigin::Local,
                    explicit: true,
                    inference: None,
                    location: Location::new(uri, Range::default()),
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
        let model = MergedSemanticModel::build(&workspace, &Default::default());
        let hover = model
            .hover_markdown_for_token("record<person>", None)
            .expect("hover");
        assert!(hover.contains("record<person>"));
        assert!(hover.contains("People"));
    }

    #[test]
    fn record_type_definition_resolves_underlying_table() {
        let uri = Url::parse("file:///workspace/schema.surql").expect("valid uri");
        let location = Location::new(
            uri.clone(),
            Range {
                start: Position::new(0, 0),
                end: Position::new(0, 18),
            },
        );
        let mut workspace = WorkspaceIndex::default();
        workspace.documents.insert(
            uri,
            DocumentAnalysis {
                uri: Url::parse("file:///workspace/schema.surql").expect("valid uri"),
                text: String::new(),
                tables: vec![TableDef {
                    name: "person".to_string(),
                    schema_mode: Some("schemafull".to_string()),
                    comment: Some("People".to_string()),
                    permissions: Vec::new(),
                    origin: SymbolOrigin::Local,
                    explicit: true,
                    inference: None,
                    location: location.clone(),
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
        let model = MergedSemanticModel::build(&workspace, &Default::default());

        assert_eq!(model.definition_for_token("record<person>"), Some(location));
    }

    #[test]
    fn table_hover_lists_indexes_events_and_permission_posture() {
        let uri = Url::parse("file:///workspace/schema.surql").expect("valid uri");
        let analysis = DocumentAnalysis {
            uri: uri.clone(),
            text: String::new(),
            tables: vec![TableDef {
                name: "person".to_string(),
                schema_mode: Some("schemafull".to_string()),
                comment: Some("People".to_string()),
                permissions: vec![PermissionRule {
                    actions: vec![QueryAction::Select],
                    mode: PermissionMode::Expression(
                        "WHERE $auth.roles CONTAINS 'viewer'".to_string(),
                    ),
                    raw: "PERMISSIONS FOR select WHERE $auth.roles CONTAINS 'viewer'".to_string(),
                    origin: SymbolOrigin::Local,
                    location: None,
                }],
                origin: SymbolOrigin::Local,
                explicit: true,
                inference: None,
                location: Location::new(uri.clone(), Range::default()),
            }],
            events: vec![EventDef {
                table: "person".to_string(),
                name: "audit_person".to_string(),
                comment: None,
                when_clause: None,
                then_clause: None,
                origin: SymbolOrigin::Local,
                location: Location::new(uri.clone(), Range::default()),
            }],
            indexes: vec![IndexDef {
                table: "person".to_string(),
                name: "person_email".to_string(),
                fields: vec!["email".to_string()],
                unique: true,
                options: Vec::new(),
                origin: SymbolOrigin::Local,
                location: Location::new(uri, Range::default()),
            }],
            fields: Vec::new(),
            functions: Vec::new(),
            params: Vec::new(),
            accesses: Vec::new(),
            query_facts: Vec::new(),
            references: Vec::new(),
            syntax_diagnostics: Vec::new(),
            document_symbols: Vec::new(),
        };
        let mut workspace = WorkspaceIndex::default();
        workspace.documents.insert(analysis.uri.clone(), analysis);
        let model = MergedSemanticModel::build(&workspace, &Default::default());
        let hover = model
            .hover_markdown_for_token("person", None)
            .expect("hover");

        assert!(hover.contains("Permissions: gated"));
        assert!(hover.contains("**Known indexes**"));
        assert!(hover.contains("person_email (email | unique)"));
        assert!(hover.contains("**Known events**"));
        assert!(hover.contains("audit_person"));
    }

    #[test]
    fn builtin_function_hover_uses_canonical_signature() {
        let model = MergedSemanticModel::default();
        let hover = model
            .hover_markdown_for_token("type::is::record", None)
            .expect("hover");
        assert!(hover.contains("type::is_record(any, table?: string) -> bool"));
        assert!(hover.contains("Canonical name: `type::is_record`"));
    }

    #[test]
    fn builtin_function_completion_includes_string_and_type_families() {
        let model = MergedSemanticModel::default();
        let items = model.completion_items("type::is_", false, None, None, None);
        assert!(items.iter().any(|item| item.label == "type::is_record"));

        let items = model.completion_items("string::low", false, None, None, None);
        assert!(items.iter().any(|item| item.label == "string::lowercase"));
    }

    #[test]
    fn completion_items_include_statement_fields_for_select_update_create() {
        let uri = Url::parse("file:///workspace/schema.surql").expect("valid uri");
        let mut model = MergedSemanticModel::default();
        model.fields.insert(
            ("person".to_string(), "email".to_string()),
            crate::semantic::types::FieldDef {
                table: "person".to_string(),
                name: "email".to_string(),
                type_expr: Some(crate::semantic::type_expr::TypeExpr::Scalar(
                    "string".to_string(),
                )),
                comment: None,
                permissions: Vec::new(),
                origin: SymbolOrigin::Local,
                explicit: true,
                inference: None,
                location: Location::new(uri.clone(), Range::default()),
            },
        );
        model.fields.insert(
            ("company".to_string(), "email".to_string()),
            crate::semantic::types::FieldDef {
                table: "company".to_string(),
                name: "email".to_string(),
                type_expr: Some(crate::semantic::type_expr::TypeExpr::Scalar(
                    "string".to_string(),
                )),
                comment: None,
                permissions: Vec::new(),
                origin: SymbolOrigin::Local,
                explicit: true,
                inference: None,
                location: Location::new(uri.clone(), Range::default()),
            },
        );

        let single_table = crate::semantic::types::QueryFact {
            action: QueryAction::Select,
            target_tables: vec!["person".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: Location::new(uri.clone(), Range::default()),
            source_preview: "SELECT email FROM person".to_string(),
        };

        let items = model.completion_items("em", false, None, Some(&single_table), None);
        assert!(items.iter().any(|item| {
            item.label == "email"
                && item
                    .detail
                    .as_deref()
                    .unwrap_or_default()
                    .contains("table: person")
        }));

        let multi_table = crate::semantic::types::QueryFact {
            action: QueryAction::Select,
            target_tables: vec!["person".to_string(), "company".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: Location::new(uri.clone(), Range::default()),
            source_preview: "SELECT * FROM person, company".to_string(),
        };
        let items = model.completion_items("em", false, None, Some(&multi_table), None);
        assert!(items.iter().any(|item| item.label == "person.email"));
        assert!(items.iter().any(|item| item.label == "company.email"));

        let items = model.completion_items("em", false, None, None, Some("person"));
        assert!(items.iter().any(|item| {
            item.label == "email"
                && item
                    .detail
                    .as_deref()
                    .unwrap_or_default()
                    .contains("table: person")
        }));
    }

    #[test]
    fn remote_functions_cannot_be_renamed() {
        let uri = Url::parse("file:///workspace/schema.surql").expect("valid uri");
        let mut model = MergedSemanticModel::default();
        model.functions.insert(
            "fn::remote".to_string(),
            FunctionDef {
                name: "fn::remote".to_string(),
                params: Vec::new(),
                return_type: None,
                language: crate::semantic::types::FunctionLanguage::SurrealQL,
                comment: None,
                permissions: Vec::new(),
                origin: SymbolOrigin::Remote,
                explicit: true,
                inference: None,
                location: Location::new(uri, Range::default()),
                selection_range: Range::default(),
                body_range: None,
                called_functions: Vec::new(),
            },
        );

        assert!(model.rename_edits("fn::remote", "fn::renamed").is_none());
    }

    #[test]
    fn detects_nested_record_type_context() {
        let source = "DEFINE FIELD friends ON TABLE person TYPE array<record<per";
        let position = Position::new(0, source.len() as u32);
        assert!(is_record_type_context(source, position));
    }
}
