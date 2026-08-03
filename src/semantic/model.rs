use std::collections::HashMap;

use ls_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CompletionItem, CompletionItemKind,
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, DocumentChanges, Documentation,
    Location, MarkupContent, MarkupKind, OneOf, OptionalVersionedTextDocumentIdentifier, Position,
    Range, TextDocumentEdit, TextEdit, Uri, WorkspaceEdit,
};
use strsim::jaro_winkler;

use crate::config::{AuthContext, ServerSettings};
use crate::grammar::{
    BUILTIN_FUNCTIONS, BuiltinFunction, GENERATED_CONSTANTS, GENERATED_FUNCTION_TABLE,
    GENERATED_NAMESPACES, KEYWORDS, SPECIAL_VARIABLES, builtin_function, builtin_namespace,
    builtin_signature,
};
use crate::semantic::codes;
use crate::semantic::text::compact_preview;
use crate::semantic::type_expr::TypeExpr;
use crate::semantic::types::{
    AccessDef, AccessResult, AnalyzerDef, DocumentAnalysis, EventDef, FieldDef, FunctionDef,
    FunctionLanguage, FunctionParam, IndexDef, LiveMetadataSnapshot, MergedSemanticModel,
    NamedRange, ParamDef, PermissionMode, PermissionRule, QueryAction, QueryFact, SymbolOrigin,
    TableDef, TargetResolution, WorkspaceIndex,
};

impl MergedSemanticModel {
    pub fn build(workspace: &WorkspaceIndex, live: &LiveMetadataSnapshot) -> Self {
        let mut model = Self::default();
        // A failing (or partially failing) metadata fetch means remote
        // tables are missing from this model — judgments like "this
        // inferred name must be a typo" can't be trusted until the
        // connection recovers.
        model.metadata_degraded = !live.errors.is_empty();

        for analysis in workspace.documents.values() {
            model.absorb_analysis(analysis.as_ref());
        }
        for analysis in live.documents.values() {
            model.absorb_analysis(analysis.as_ref());
        }

        for analysis in workspace.documents.values() {
            for reference in &analysis.references {
                if reference.kind == ls_types::SymbolKind::FUNCTION {
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

        // Derive a return type for every `DEFINE FUNCTION` that omits `-> T`.
        // Must run last: it judges each definition against the one that won the
        // merge, so every document has to be absorbed first.
        //
        // Live documents are included deliberately. `INFO FOR DB` returns the
        // engine's own `DEFINE FUNCTION` text, body and all, and
        // `SurrealDbMetadataProvider` re-parses it through `analyze_document` —
        // so a remote function is as inferrable as a local one, and excluding
        // them would make a remote `fn::x` hover `unknown` while a byte-identical
        // local one hovers `string`.
        let documents: Vec<&DocumentAnalysis> = workspace
            .documents
            .values()
            .chain(live.documents.values())
            .map(|analysis| analysis.as_ref())
            .collect();
        crate::semantic::infer::infer_function_return_types(&documents, &mut model);

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

    /// Returns *only* column (field) completion items for the given target
    /// tables. Use when the cursor is positioned in a slot that syntactically
    /// only accepts a column name (e.g. between `SELECT` and `FROM`, after
    /// `UPDATE tbl SET `, or after a `tbl.` qualifier).
    ///
    /// Mirrors the field branch of [`Self::completion_items`] but emits no
    /// keywords / functions / params / namespaces.
    pub fn column_completion_items(
        &self,
        prefix: &str,
        tables: &[String],
        multi_table_context: bool,
        _active_context: Option<&AuthContext>,
    ) -> Vec<CompletionItem> {
        let mut items = Vec::new();
        for table_name in tables {
            for field in self.fields_for_table(table_name) {
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
                    sort_text: Some(format!("0-fld-{}-{}", field.table, field.name)),
                    ..CompletionItem::default()
                });
            }
        }
        items
    }

    /// Returns *only* table-name completion items (no keywords, functions,
    /// fields, params, etc). Use when the cursor is positioned in a slot
    /// that syntactically only accepts a table name (e.g. right after
    /// `SELECT * FROM `, `INSERT INTO `, `UPDATE `).
    pub fn table_completion_items(
        &self,
        prefix: &str,
        active_context: Option<&AuthContext>,
    ) -> Vec<CompletionItem> {
        self.table_names_by_priority()
            .into_iter()
            .filter(|table| prefix.is_empty() || table.name.starts_with(prefix))
            .map(|table| CompletionItem {
                label: table.name.clone(),
                kind: Some(CompletionItemKind::STRUCT),
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
            })
            .collect()
    }

    /// The `DEFINE ANALYZER` names, for the slots that reference one.
    pub fn analyzer_completion_items(&self, prefix: &str) -> Vec<CompletionItem> {
        let mut items: Vec<CompletionItem> = self
            .analyzers
            .values()
            .filter(|analyzer| prefix.is_empty() || analyzer.name.starts_with(prefix))
            .map(|analyzer| CompletionItem {
                label: analyzer.name.clone(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some(format!(
                    "Analyzer, source: {}",
                    origin_label(analyzer.origin)
                )),
                sort_text: Some(format!(
                    "0-{}-{}",
                    symbol_priority(analyzer.origin),
                    analyzer.name
                )),
                ..CompletionItem::default()
            })
            .collect();
        items.sort_by(|left, right| left.label.cmp(&right.label));
        items
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

    /// Completion items for the `$variables` in scope at `position`.
    ///
    /// These were never offered before — only `SPECIAL_VARIABLES` and
    /// `DEFINE PARAM` entries reached the dropdown, so a `LET` binding two
    /// lines up was invisible.
    pub fn variable_completion_items(
        &self,
        analysis: &DocumentAnalysis,
        position: Position,
        prefix: &str,
    ) -> Vec<CompletionItem> {
        let offset = crate::semantic::text::position_to_offset(&analysis.text, position);
        let bindings = crate::semantic::infer::resolve_bindings(analysis, self);
        bindings
            .visible_at(offset)
            .into_iter()
            .filter(|binding| prefix.is_empty() || binding.name.starts_with(prefix))
            .map(|binding| CompletionItem {
                label: binding.name.clone(),
                kind: Some(CompletionItemKind::VARIABLE),
                // `CompletionItemKind::VARIABLE` already says "variable";
                // the useful detail is the type it holds.
                detail: Some(binding.ty.to_string()),
                insert_text: Some(binding.name.clone()),
                sort_text: Some(format!("0-var-{}", binding.name)),
                ..CompletionItem::default()
            })
            .collect()
    }

    /// Completion items for a `value.` position: the methods that receiver
    /// accepts.
    ///
    /// SurrealQL admits both a field and a method after a `.`, so these are meant
    /// to be *added* to whatever the position already offers, never to replace
    /// it.
    ///
    /// When the receiver's type is known, only that receiver's methods are
    /// offered and they sort alongside the fields. When it is not — which is
    /// still common, since a field access or a statement result types as
    /// `unknown` — every method is offered, sorted below everything else. An
    /// empty list would read as "this feature is broken" in exactly the positions
    /// people use most.
    pub fn method_completion_items(
        &self,
        analysis: &DocumentAnalysis,
        position: Position,
        prefix: &str,
    ) -> Vec<CompletionItem> {
        let offset = crate::semantic::text::position_to_offset(&analysis.text, position);
        let Some(dot) = method_dot_offset(&analysis.text, offset) else {
            return Vec::new();
        };

        // Type whatever sits immediately left of the dot.
        let receiver = analysis
            .tree
            .root_node()
            .named_descendant_for_byte_range(dot.saturating_sub(1), dot);
        let receiver_type = match receiver {
            Some(node) => {
                let bindings = crate::semantic::infer::resolve_bindings(analysis, self);
                let ctx = crate::semantic::infer::TypeCtx {
                    model: self,
                    source: &analysis.text,
                    bindings: &bindings,
                };
                crate::semantic::infer::infer_expr_type(node, &ctx)
            }
            None => TypeExpr::Unknown,
        };

        let known = crate::semantic::method::receiver_kind(&receiver_type);
        let (methods, rank): (Vec<_>, &str) = match known {
            Some(kind) => (
                crate::semantic::method::methods_for(kind).iter().collect(),
                "0-mtd",
            ),
            None => (
                crate::grammar::GENERATED_RECEIVERS
                    .iter()
                    .flat_map(|receiver| receiver.methods.iter())
                    .collect(),
                "3-mtd",
            ),
        };

        let mut seen: Vec<&str> = Vec::new();
        let mut items = Vec::new();
        for method in methods {
            if !prefix.is_empty() && !method.method.starts_with(prefix) {
                continue;
            }
            // The fallback list draws from twelve tables, and `to_string` is on
            // all of them.
            if seen.contains(&method.method) {
                continue;
            }
            seen.push(method.method);

            let signature = builtin_signature(method.function);
            let mut detail = method.function.to_string();
            if let Some(rendered) = signature
                .as_ref()
                .and_then(|found| found.display_signature())
            {
                detail = rendered;
            }
            if let Some(target) = method.experimental {
                detail.push_str(&format!(" (experimental: {target})"));
            }

            items.push(CompletionItem {
                label: method.method.to_string(),
                kind: Some(CompletionItemKind::METHOD),
                detail: Some(detail),
                documentation: builtin_function(method.function).map(|curated| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: format_builtin_function_hover(curated, method.function),
                    })
                }),
                insert_text: Some(method.method.to_string()),
                sort_text: Some(format!("{rank}-{}", method.method)),
                ..CompletionItem::default()
            });
        }
        items
    }

    /// Position-aware hover.
    ///
    /// A `$variable` can only be resolved with a position: the same name
    /// may be bound several times in one document, and the enclosing
    /// scope decides which one is meant. Everything else is
    /// position-independent and falls through to
    /// [`Self::hover_markdown_for_token`].
    pub fn hover_markdown_at(
        &self,
        analysis: &DocumentAnalysis,
        position: Position,
        token: &str,
        active_context: Option<&AuthContext>,
    ) -> Option<String> {
        let offset = crate::semantic::text::position_to_offset(&analysis.text, position);

        // A method resolves through its receiver, not through the global function
        // tables. This must run before `hover_markdown_for_token`, which sees only
        // the bare word and would answer with the `AT` / `SPLIT` keyword.
        if let Some(hover) = self.method_hover(analysis, offset) {
            return Some(hover);
        }

        if token.starts_with('$') {
            let bindings = crate::semantic::infer::resolve_bindings(analysis, self);
            if let Some(binding) = bindings.at(token, offset) {
                return Some(format_binding_hover(binding));
            }
        }
        self.hover_markdown_for_token(token, active_context)
    }

    /// Hover for a method call, resolved through the engine's receiver tables.
    fn method_hover(&self, analysis: &DocumentAnalysis, offset: usize) -> Option<String> {
        let (idiom, method) = method_at(analysis, offset)?;
        let receiver = crate::semantic::infer::method_receiver(idiom)?;

        let bindings = crate::semantic::infer::resolve_bindings(analysis, self);
        let ctx = crate::semantic::infer::TypeCtx {
            model: self,
            source: &analysis.text,
            bindings: &bindings,
        };
        let receiver_type = crate::semantic::infer::infer_expr_type(receiver, &ctx);
        let resolved = crate::semantic::method::resolve(&receiver_type, &method)?;

        let mut metadata = vec![format!("Resolves to `{}`", resolved.function)];
        if let Some(target) = resolved.experimental {
            metadata.push(format!("Experimental: requires `{target}`"));
        }

        let signature = builtin_signature(resolved.function)
            .and_then(|signature| signature.display_signature());
        // The signature ends in `-> type` whenever one is known, so saying it
        // again here would only repeat the title. State it when there is no
        // signature to carry it.
        if signature.is_none()
            && let Some(returns) = crate::semantic::method::return_type(resolved.function)
        {
            metadata.push(format!("Returns: `{returns}`"));
        }

        let title = signature.unwrap_or_else(|| format!("{}()", resolved.function));
        let summary =
            builtin_function(resolved.function).map(|curated| curated.summary.to_string());
        let sections = builtin_function(resolved.function)
            .map(|curated| vec![format!("[Docs]({})", curated.documentation_url)])
            .unwrap_or_default();

        Some(hover_block(
            format!(".{method}() — {title}"),
            summary,
            metadata,
            sections,
        ))
    }

    /// Like [`Self::find_nearest_table`], but only explicitly defined
    /// tables qualify as "did you mean" candidates — suggesting an
    /// inferred name would just echo another usage site back.
    fn find_nearest_explicit_table(&self, unknown: &str) -> Option<&TableDef> {
        self.tables
            .values()
            .filter(|table| table.explicit && table.name != unknown)
            .map(|table| (table, jaro_winkler(unknown, &table.name)))
            .filter(|(_, score)| *score > 0.86)
            .max_by(|left, right| {
                left.1
                    .partial_cmp(&right.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(table, _)| table)
    }

    /// Like [`Self::find_nearest_explicit_table`], but restricted to
    /// candidates that plausibly indicate a *typo* rather than a
    /// deliberate sibling name: bare singular/plural pairs
    /// (`orders`/`order`, `categories`/`category`) score ~0.96 on
    /// jaro-winkler yet are the most common intentional naming
    /// pattern in schemas, so they are excluded here.
    fn find_probable_typo_of_explicit_table(&self, unknown: &str) -> Option<&TableDef> {
        self.find_nearest_explicit_table(unknown)
            .filter(|candidate| !is_plural_variant(unknown, &candidate.name))
    }

    /// How many query facts across the workspace target `name`. A
    /// name used in several statements is a deliberate table, not a
    /// one-off typo.
    fn target_usage_count(&self, name: &str) -> usize {
        self.query_facts
            .values()
            .flatten()
            .filter(|fact| fact.target_tables.iter().any(|table| table == name))
            .count()
    }

    /// Nearest explicitly defined field on `table` — the unknown-field
    /// "did you mean" candidate.
    fn find_nearest_explicit_field(&self, table: &str, unknown: &str) -> Option<&FieldDef> {
        self.fields
            .iter()
            .filter(|((field_table, _), field)| {
                field_table == table && field.explicit && field.name != unknown
            })
            .map(|(_, field)| (field, jaro_winkler(unknown, &field.name)))
            .filter(|(_, score)| *score > 0.86)
            .max_by(|left, right| {
                left.1
                    .partial_cmp(&right.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(field, _)| field)
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
            return Some(format_function_hover(
                function,
                self.inferred_function_returns.get(trimmed),
            ));
        }
        // The curated table first: its 79 entries carry prose and a docs link
        // that no generator can produce.
        if let Some(function) = builtin_function(trimmed) {
            return Some(format_builtin_function_hover(function, trimmed));
        }
        // Then the generated catalogue, which covers the other 18 namespaces.
        // Before this, hovering `math::abs` answered nothing at all.
        if let Some(signature) = builtin_signature(trimmed) {
            return Some(format_generated_function_hover(signature, trimmed));
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
        if GENERATED_NAMESPACES
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

            for namespace in GENERATED_NAMESPACES {
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
                        detail: Some(function_signature_with_return(
                            function,
                            self.inferred_function_returns.get(&function.name),
                        )),
                        documentation: Some(Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format_function_hover(
                                function,
                                self.inferred_function_returns.get(&function.name),
                            ),
                        })),
                        sort_text: Some(format!("1-{}", function.name)),
                        ..CompletionItem::default()
                    });
                }
            }

            // The curated table first: its 79 entries carry prose and a docs
            // link that no generator can produce.
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

            // Then the generated catalogue, which is the other 355. Without this
            // the dropdown only ever held `string::` and `type::` — the two
            // namespaces the curated table happens to cover — so typing `rand::`
            // offered the namespace and then nothing inside it, and the same for
            // `array::` (62 functions), `math::` (42) and `time::` (37).
            //
            // Curated entries win, so a function with prose keeps it.
            for function in GENERATED_FUNCTION_TABLE {
                if !prefix.is_empty() && !function.name.starts_with(&normalized_builtin) {
                    continue;
                }
                if builtin_function(function.name).is_some() {
                    continue;
                }
                let signature = builtin_signature(function.name);
                let detail = signature
                    .as_ref()
                    .and_then(|found| found.display_signature())
                    .unwrap_or_else(|| format!("{}(…)", function.name));
                items.push(CompletionItem {
                    label: function.name.to_string(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail: Some(detail),
                    // A name the parser accepts that nothing implements. Offering
                    // it silently would hand the user a query that parses and
                    // then fails.
                    deprecated: Some(function.not_callable),
                    sort_text: Some(format!("2-{}", function.name)),
                    ..CompletionItem::default()
                });
            }

            // Constants such as `math::PI`. They take no arguments, so they are
            // not in `GENERATED_FUNCTIONS` at all.
            for constant in GENERATED_CONSTANTS {
                // Compared case-insensitively: a constant is spelled in upper
                // case (`math::PI`) while `normalized_builtin` is lowered, so a
                // `starts_with` on the raw names never matches.
                if prefix.is_empty()
                    || constant
                        .to_ascii_lowercase()
                        .starts_with(&normalized_builtin)
                {
                    items.push(CompletionItem {
                        label: constant.to_string(),
                        kind: Some(CompletionItemKind::CONSTANT),
                        detail: Some("Builtin constant".to_string()),
                        sort_text: Some(format!("2-{constant}")),
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
                        // `0-fld-...` sorts above `1-` user functions, `2-`
                        // builtin functions, and unsorted keywords so that
                        // in loose contexts (WHERE / ORDER BY / GROUP BY)
                        // the relevant column names surface first.
                        sort_text: Some(format!("0-fld-{}-{}", field.table, field.name)),
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

        // `DEFINE PARAM` names. The model has held these all along — hover and
        // go-to-definition both resolve them — but nothing ever offered them,
        // so a parameter defined in another file was invisible while typing.
        for param in self.params.values() {
            if prefix.is_empty() || param.name.starts_with(prefix) {
                items.push(CompletionItem {
                    label: param.name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(format!(
                        "DEFINE PARAM, source: {}",
                        origin_label(param.origin)
                    )),
                    documentation: Some(Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: format_param_hover(param),
                    })),
                    // Above the special variables, which carry no sort text: a
                    // parameter the author defined is likelier than `$before`.
                    sort_text: Some(format!("0-1-{}", param.name)),
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
        let mut diagnostics = crate::semantic::infer::type_diagnostics(analysis, self, settings);
        let active_context = settings.active_auth_context();

        for fact in analysis.query_facts.iter() {
            if fact.target_tables.is_empty() {
                // `$param` / expression targets are resolvable only at
                // runtime — warning about them is pure noise.
                if matches!(
                    fact.target_resolution,
                    TargetResolution::Parameter | TargetResolution::Expression
                ) {
                    continue;
                }
                diagnostics.push(Diagnostic {
                    range: fact.location.range,
                    severity: Some(DiagnosticSeverity::WARNING),
                    code: codes::as_code(codes::DYNAMIC_TARGET),
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
                let table_range = range_for_name(&fact.target_refs, table, fact.location.range);
                let table_def = match self.tables.get(table) {
                    None => {
                        let suggestion = self.find_nearest_explicit_table(table);
                        diagnostics.push(self.unknown_table_diagnostic(
                            table,
                            table_range,
                            suggestion,
                        ));
                        continue;
                    }
                    // The statement being checked is itself enough to
                    // *infer* a table, so a typo'd name always "exists"
                    // by the time we validate it. An inferred-only def
                    // is treated as a typo only when ALL of these hold:
                    //
                    // 1. Live metadata is healthy. When the DB fetch
                    //    is failing (including partial per-table INFO
                    //    errors), previously-known remote tables drop
                    //    out of the model and would light up as
                    //    near-misses in bulk — right when the
                    //    "metadata unavailable" toast already fires.
                    // 2. The name is used only once across the
                    //    workspace. Repeated usage means a deliberate
                    //    (if undeclared) table; the trade-off is that
                    //    the same typo pasted twice goes silent.
                    // 3. An explicit table is a near-miss that is NOT
                    //    a bare singular/plural sibling — `orders`
                    //    next to `order` is a naming convention, not a
                    //    typo, and the quick fix would rewrite the
                    //    query against a different real table.
                    //
                    // Everything else stays untouched — schema
                    // inference from usage is a feature, not an error.
                    Some(table_def) if !table_def.explicit => {
                        if !self.metadata_degraded && self.target_usage_count(table) <= 1 {
                            if let Some(suggestion) =
                                self.find_probable_typo_of_explicit_table(table)
                            {
                                diagnostics.push(self.unknown_table_diagnostic(
                                    table,
                                    table_range,
                                    Some(suggestion),
                                ));
                                continue;
                            }
                        }
                        table_def
                    }
                    Some(table_def) => table_def,
                };

                // SELECT and RELATE are intentionally exempt from
                // static permission checking: their permission rules
                // routinely depend on row-level state (e.g.
                // `WHERE $auth.id = id`) that can't be evaluated
                // without the actual record, so the diagnostics tend
                // to be noisy false-positives in the editor.
                if !matches!(fact.action, QueryAction::Select | QueryAction::Relate) {
                    let permission = self.evaluate_permissions(fact, table_def, active_context);
                    match permission.result {
                        AccessResult::Denied => diagnostics.push(Diagnostic {
                            range: table_range,
                            severity: Some(DiagnosticSeverity::ERROR),
                            code: codes::as_code(codes::PERMISSION_DENIED),
                            source: Some("surreal-language-server".to_string()),
                            message: permission.message,
                            ..Diagnostic::default()
                        }),
                        AccessResult::Unknown => diagnostics.push(Diagnostic {
                            range: table_range,
                            severity: Some(DiagnosticSeverity::WARNING),
                            code: codes::as_code(codes::PERMISSION_UNKNOWN),
                            source: Some("surreal-language-server".to_string()),
                            message: permission.message,
                            ..Diagnostic::default()
                        }),
                        AccessResult::Allowed => {}
                    }
                }

                // Unknown-field only applies where the schema is
                // closed: on a SCHEMALESS (or unspecified) table any
                // ad-hoc field is legal and the warning would be a
                // false positive. RELATE is exempt as well — its
                // target list mixes the subject tables with the edge
                // table, so SET fields (which belong to the edge)
                // would be checked against the wrong schemas.
                let schemafull = table_def
                    .schema_mode
                    .as_deref()
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("schemafull"));
                if !(table_def.explicit && schemafull) || fact.action == QueryAction::Relate {
                    continue;
                }
                for field in &fact.touched_fields {
                    // Builtin fields exist on every record without a
                    // DEFINE FIELD (`in`/`out` are the relation
                    // endpoints).
                    if matches!(field.as_str(), "id" | "in" | "out") {
                        continue;
                    }
                    // Same masking hazard as tables: the statement
                    // under scrutiny *infers* a field def for every
                    // name it assigns, so only an explicit definition
                    // counts as "known" on a closed schema.
                    let explicitly_defined = self
                        .fields
                        .get(&(table.clone(), field.clone()))
                        .is_some_and(|field_def| field_def.explicit);
                    if !explicitly_defined {
                        let range = range_for_name(&fact.field_refs, field, fact.location.range);
                        diagnostics.push(self.unknown_field_diagnostic(table, field, range));
                    }
                }
            }
        }

        diagnostics
    }

    fn unknown_table_diagnostic(
        &self,
        table: &str,
        range: Range,
        suggestion: Option<&TableDef>,
    ) -> Diagnostic {
        let message = match suggestion {
            Some(candidate) => format!(
                "Unknown table `{table}`. Did you mean `{}`?",
                candidate.name
            ),
            None => format!("Unknown table `{table}`."),
        };
        let data = match suggestion {
            Some(candidate) => {
                serde_json::json!({ "table": table, "suggestion": candidate.name })
            }
            None => serde_json::json!({ "table": table }),
        };
        Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::WARNING),
            code: codes::as_code(codes::UNKNOWN_TABLE),
            source: Some("surreal-language-server".to_string()),
            message,
            data: Some(data),
            related_information: suggestion.map(|candidate| {
                vec![DiagnosticRelatedInformation {
                    location: candidate.location.clone(),
                    message: format!("`{}` is defined here.", candidate.name),
                }]
            }),
            ..Diagnostic::default()
        }
    }

    fn unknown_field_diagnostic(&self, table: &str, field: &str, range: Range) -> Diagnostic {
        let suggestion = self.find_nearest_explicit_field(table, field);
        let message = match suggestion {
            Some(candidate) => format!(
                "Unknown field `{table}.{field}`. Did you mean `{}`?",
                candidate.name
            ),
            None => format!("Unknown field `{table}.{field}`."),
        };
        let data = match suggestion {
            Some(candidate) => serde_json::json!({
                "table": table,
                "field": field,
                "suggestion": candidate.name,
            }),
            None => serde_json::json!({ "table": table, "field": field }),
        };
        Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::WARNING),
            code: codes::as_code(codes::UNKNOWN_FIELD),
            source: Some("surreal-language-server".to_string()),
            message,
            data: Some(data),
            related_information: suggestion.map(|candidate| {
                vec![DiagnosticRelatedInformation {
                    location: candidate.location.clone(),
                    message: format!("`{}` is defined here.", candidate.name),
                }]
            }),
            ..Diagnostic::default()
        }
    }

    pub fn code_actions(
        &self,
        uri: &Uri,
        analysis: &DocumentAnalysis,
        diagnostics: &[Diagnostic],
    ) -> Vec<CodeActionOrCommand> {
        let mut actions = Vec::new();

        for diagnostic in diagnostics {
            if let Some((table, suggestion)) = unknown_table_payload(diagnostic) {
                let replacement =
                    suggestion.or_else(|| self.find_nearest_table(&table).map(|t| t.name.clone()));
                if let Some(replacement) = replacement {
                    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                        title: format!("Replace `{table}` with `{replacement}`"),
                        kind: Some(CodeActionKind::QUICKFIX),
                        diagnostics: Some(vec![diagnostic.clone()]),
                        edit: Some(WorkspaceEdit {
                            document_changes: Some(DocumentChanges::Operations(vec![
                                ls_types::DocumentChangeOperation::Edit(TextDocumentEdit {
                                    text_document: OptionalVersionedTextDocumentIdentifier {
                                        uri: uri.clone(),
                                        version: None,
                                    },
                                    edits: vec![OneOf::Left(TextEdit {
                                        range: diagnostic.range,
                                        new_text: replacement.clone(),
                                    })],
                                }),
                            ])),
                            ..WorkspaceEdit::default()
                        }),
                        ..CodeAction::default()
                    }));
                }
            }

            // A renamed builtin. The old name sits in the diagnostic's own
            // range, and the engine records the replacement, so the fix needs no
            // payload beyond the text already there.
            if codes::has_code(diagnostic, codes::RENAMED_FUNCTION)
                && let Some(old) = text_in_range(&analysis.text, diagnostic.range)
                && let Some(current) = crate::grammar::renamed_builtin(old.trim())
            {
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: format!("Rename `{}` to `{current}`", old.trim()),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![diagnostic.clone()]),
                    is_preferred: Some(true),
                    edit: Some(WorkspaceEdit {
                        document_changes: Some(DocumentChanges::Operations(vec![
                            ls_types::DocumentChangeOperation::Edit(TextDocumentEdit {
                                text_document: OptionalVersionedTextDocumentIdentifier {
                                    uri: uri.clone(),
                                    version: None,
                                },
                                edits: vec![OneOf::Left(TextEdit {
                                    range: diagnostic.range,
                                    new_text: current.to_string(),
                                })],
                            }),
                        ])),
                        ..WorkspaceEdit::default()
                    }),
                    ..CodeAction::default()
                }));
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
                    document_changes: Some(DocumentChanges::Operations(vec![ls_types::DocumentChangeOperation::Edit(
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

    pub fn rename_edits(&self, name: &str, new_name: &str) -> Option<HashMap<Uri, Vec<TextEdit>>> {
        let function = self.functions.get(name)?;
        if function.origin != SymbolOrigin::Local {
            return None;
        }

        let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
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

    pub fn workspace_symbol_items(&self, query: &str) -> Vec<ls_types::SymbolInformation> {
        let needle = query.to_ascii_lowercase();
        let mut items = Vec::new();
        for table in self.tables.values() {
            if needle.is_empty() || table.name.to_ascii_lowercase().contains(&needle) {
                items.push(symbol_information(
                    &table.name,
                    ls_types::SymbolKind::STRUCT,
                    &table.location,
                ));
            }
        }
        for field in self.fields.values() {
            let label = format!("{}.{}", field.table, field.name);
            if needle.is_empty() || label.to_ascii_lowercase().contains(&needle) {
                items.push(symbol_information(
                    &label,
                    ls_types::SymbolKind::FIELD,
                    &field.location,
                ));
            }
        }
        for event in self.events.values() {
            let label = format!("{}.{}", event.table, event.name);
            if needle.is_empty() || label.to_ascii_lowercase().contains(&needle) {
                items.push(symbol_information(
                    &label,
                    ls_types::SymbolKind::EVENT,
                    &event.location,
                ));
            }
        }
        for index in self.indexes.values() {
            let label = format!("{}.{}", index.table, index.name);
            if needle.is_empty() || label.to_ascii_lowercase().contains(&needle) {
                items.push(symbol_information(
                    &label,
                    ls_types::SymbolKind::KEY,
                    &index.location,
                ));
            }
        }
        for function in self.functions.values() {
            if needle.is_empty() || function.name.to_ascii_lowercase().contains(&needle) {
                items.push(symbol_information(
                    &function.name,
                    ls_types::SymbolKind::FUNCTION,
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
            .extend(analysis.query_facts.iter().cloned());
        self.workspace_symbols
            .extend(analysis.document_symbols.iter().cloned());

        // Pass references to the merge functions; they clone the candidate
        // only when it actually wins over the current entry. For workspaces
        // with many overlapping definitions (saved + open + remote merged
        // together) this skips a lot of throwaway allocations.
        for table in &analysis.tables {
            merge_table(&mut self.tables, table);
        }
        for event in &analysis.events {
            merge_event(&mut self.events, event);
        }
        for index in &analysis.indexes {
            merge_index(&mut self.indexes, index);
        }
        for field in &analysis.fields {
            merge_field(&mut self.fields, field);
        }
        for function in &analysis.functions {
            merge_function(&mut self.functions, function);
        }
        for param in &analysis.params {
            merge_param(&mut self.params, param);
        }
        for access in &analysis.accesses {
            merge_access(&mut self.accesses, access);
        }
        for analyzer in &analysis.analyzers {
            merge_analyzer(&mut self.analyzers, analyzer);
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

/// True when the two names are bare singular/plural forms of each
/// other (`order`/`orders`, `box`/`boxes`, `category`/`categories`),
/// in either direction and case-insensitively.
fn is_plural_variant(left: &str, right: &str) -> bool {
    fn is_plural_of(plural: &str, singular: &str) -> bool {
        if let Some(stem) = plural.strip_suffix("ies") {
            if format!("{stem}y") == singular {
                return true;
            }
        }
        if let Some(stem) = plural.strip_suffix("es")
            && stem == singular
        {
            return true;
        }
        // Bare-`s` plurals only apply when the stem doesn't itself
        // end in `s`: s-ending nouns pluralise with `es`, so
        // `address`/`addres` is a real typo, not a plural pair.
        plural
            .strip_suffix('s')
            .is_some_and(|stem| stem == singular && !stem.ends_with('s'))
    }

    let left = left.to_ascii_lowercase();
    let right = right.to_ascii_lowercase();
    is_plural_of(&left, &right) || is_plural_of(&right, &left)
}

/// Tight token range for `name`, falling back to the statement range
/// for facts recorded before ranges were tracked.
fn range_for_name(refs: &[NamedRange], name: &str, fallback: Range) -> Range {
    refs.iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.range)
        .unwrap_or(fallback)
}

/// Extract the `(table, suggested_replacement)` payload from an
/// unknown-table diagnostic. Matches on the stable `unknown-table`
/// code + `data` first; falls back to parsing the legacy message text
/// so quick fixes keep working for diagnostics cached by pre-0.3
/// clients. Remove the string fallback in 0.4.
fn unknown_table_payload(diagnostic: &Diagnostic) -> Option<(String, Option<String>)> {
    // Primary path: stable code + structured data.
    if codes::has_code(diagnostic, codes::UNKNOWN_TABLE)
        && let Some(data) = diagnostic.data.as_ref()
        && let Some(table) = data.get("table").and_then(|value| value.as_str())
    {
        let suggestion = data
            .get("suggestion")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        return Some((table.to_string(), suggestion));
    }

    // Fallback: parse the message text. This keeps quick fixes alive
    // for clients that strip the non-standard `data` field (or, pre-
    // 0.3, the code too). Takes the table up to the closing backtick
    // so both "Unknown table `x`." and
    // "Unknown table `x`. Did you mean `y`?" parse.
    let rest = diagnostic.message.strip_prefix("Unknown table `")?;
    let (table, tail) = rest.split_once('`')?;
    let suggestion = tail
        .strip_prefix(". Did you mean `")
        .and_then(|tail| tail.split_once('`'))
        .map(|(suggestion, _)| suggestion.to_string());
    Some((table.to_string(), suggestion))
}

fn merge_table(target: &mut HashMap<String, TableDef>, candidate: &TableDef) {
    let replace = target
        .get(&candidate.name)
        .map(|current| should_replace_table(current, candidate))
        .unwrap_or(true);
    if replace {
        target.insert(candidate.name.clone(), candidate.clone());
    }
}

fn merge_event(target: &mut HashMap<(String, String), EventDef>, candidate: &EventDef) {
    if let Some(current) = target.get(&(candidate.table.clone(), candidate.name.clone())) {
        if symbol_priority(candidate.origin) < symbol_priority(current.origin) {
            return;
        }
    }
    target.insert(
        (candidate.table.clone(), candidate.name.clone()),
        candidate.clone(),
    );
}

fn merge_index(target: &mut HashMap<(String, String), IndexDef>, candidate: &IndexDef) {
    if let Some(current) = target.get(&(candidate.table.clone(), candidate.name.clone())) {
        if symbol_priority(candidate.origin) < symbol_priority(current.origin) {
            return;
        }
    }
    target.insert(
        (candidate.table.clone(), candidate.name.clone()),
        candidate.clone(),
    );
}

fn merge_field(target: &mut HashMap<(String, String), FieldDef>, candidate: &FieldDef) {
    let key = (candidate.table.clone(), candidate.name.clone());
    let replace = target
        .get(&key)
        .map(|current| should_replace_field(current, candidate))
        .unwrap_or(true);
    if replace {
        target.insert(key, candidate.clone());
    }
}

fn merge_function(target: &mut HashMap<String, FunctionDef>, candidate: &FunctionDef) {
    let replace = target
        .get(&candidate.name)
        .map(|current| should_replace_function(current, candidate))
        .unwrap_or(true);
    if replace {
        target.insert(candidate.name.clone(), candidate.clone());
    }
}

fn merge_param(target: &mut HashMap<String, ParamDef>, candidate: &ParamDef) {
    if let Some(current) = target.get(&candidate.name) {
        if symbol_priority(candidate.origin) < symbol_priority(current.origin) {
            return;
        }
    }
    target.insert(candidate.name.clone(), candidate.clone());
}

fn merge_analyzer(target: &mut HashMap<String, AnalyzerDef>, candidate: &AnalyzerDef) {
    if let Some(current) = target.get(&candidate.name)
        && symbol_priority(candidate.origin) < symbol_priority(current.origin)
    {
        return;
    }
    target.insert(candidate.name.clone(), candidate.clone());
}

fn merge_access(target: &mut HashMap<String, AccessDef>, candidate: &AccessDef) {
    if let Some(current) = target.get(&candidate.name) {
        if symbol_priority(candidate.origin) < symbol_priority(current.origin) {
            return;
        }
    }
    target.insert(candidate.name.clone(), candidate.clone());
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

fn format_function_hover(function: &FunctionDef, inferred_return: Option<&TypeExpr>) -> String {
    let mut metadata = vec![format!("Source: {}", origin_label(function.origin))];
    match function.language {
        FunctionLanguage::JavaScript => metadata.push("Language: JavaScript".to_string()),
        FunctionLanguage::SurrealQL => {}
    }
    // Say it plainly, so the `->` in the signature above cannot be read as an
    // annotation the author wrote. Same convention as `format_binding_hover`.
    if function.return_type.is_none() && inferred_return.is_some() {
        metadata.push("Return type inferred from the body.".to_string());
    }
    let mut sections = Vec::new();
    if !function.called_functions.is_empty() {
        sections.push(list_section("Calls", function.called_functions.clone()));
    }
    hover_block(
        function_signature_with_return(function, inferred_return),
        function.comment.clone(),
        metadata,
        sections,
    )
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

/// Hover for a builtin the curated table has no prose for.
///
/// Everything shown is derived from the engine's own source, so there is no
/// summary to give — the signature and the namespace's documentation page are
/// what we honestly have. Better than the nothing this used to return for 18 of
/// the 20 advertised namespaces.
/// The source text an LSP range covers.
fn text_in_range(source: &str, range: ls_types::Range) -> Option<&str> {
    let start = crate::semantic::text::position_to_offset(source, range.start);
    let end = crate::semantic::text::position_to_offset(source, range.end);
    source.get(start..end)
}

fn format_generated_function_hover(
    signature: &crate::grammar::BuiltinSignature,
    token: &str,
) -> String {
    let name = signature.generated.name;
    let mut metadata = vec!["Source: builtin".to_string()];
    if !token.eq_ignore_ascii_case(name) {
        metadata.push(format!("Canonical name: `{name}`"));
    }
    if signature.generated.not_callable {
        metadata.push(
            "The parser accepts this name, but no implementation is reachable in call form."
                .to_string(),
        );
    }

    // `display_signature` carries the return type in its arrow, so this states
    // it only when there is no signature to carry it. `rand::int` is the case
    // that needs it: its arity cannot be read from the engine's argument
    // wrappers, but the registry still declares that it returns an `int`.
    let title = signature.display_signature();
    if title.is_none()
        && let Some(returns) = crate::grammar::builtin_return_type(name)
    {
        metadata.push(format!("Returns: `{returns}`"));
    }

    let mut sections = Vec::new();
    if let Some(namespace) = name.split_once("::").map(|(namespace, _)| namespace) {
        sections.push(list_section(
            "Docs",
            vec![format!(
                "[SurrealDB reference](https://surrealdb.com/docs/surrealql/functions/database/{namespace})"
            )],
        ));
    }

    hover_block(
        title.unwrap_or_else(|| format!("{name}(…)")),
        None,
        metadata,
        sections,
    )
}

fn format_binding_hover(binding: &crate::semantic::infer::Binding) -> String {
    let mut facts = vec![format!("Type: `{}`", binding.ty)];
    // Only worth saying when the author didn't write the type themselves.
    if binding.declared.is_none() && binding.ty != TypeExpr::Unknown {
        facts.push("Inferred from the assigned value.".to_string());
    }
    hover_block(
        format!("{} {}", binding.kind.label(), binding.name),
        None,
        facts,
        Vec::new(),
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

/// How one parameter is spelled wherever a signature is shown to the user.
///
/// Shared by function hover and by signature help
/// ([`crate::core::LanguageServerCore::signature_help`]) so the two cannot
/// drift — they previously formatted this identically but separately.
pub fn param_label(param: &FunctionParam) -> String {
    match &param.type_expr {
        Some(type_expr) => format!("{}: {}", param.name, type_expr),
        None => param.name.clone(),
    }
}

/// `fn::name($a: type, …) -> type`, as rendered in hover and signature help.
///
/// Renders only what the source declares. Use
/// [`function_signature_with_return`] where a body-inferred return type should
/// show too.
pub fn function_signature(function: &FunctionDef) -> String {
    function_signature_with_return(function, None)
}

/// [`function_signature`], but falling back to a body-inferred return type.
///
/// `inferred` comes from [`MergedSemanticModel::inferred_function_returns`], so
/// only a caller holding the model can supply it. A declared type always wins.
///
/// The arrow alone cannot distinguish the two, so every caller that passes
/// `Some` is responsible for saying so nearby — [`format_function_hover`] adds a
/// line for exactly that reason.
pub fn function_signature_with_return(
    function: &FunctionDef,
    inferred: Option<&TypeExpr>,
) -> String {
    let params = function
        .params
        .iter()
        .map(param_label)
        .collect::<Vec<_>>()
        .join(", ");
    let base = format!("{}({params})", function.name);
    match function.return_type.as_ref().or(inferred) {
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
    kind: ls_types::SymbolKind,
    location: &Location,
) -> ls_types::SymbolInformation {
    #[allow(deprecated)]
    ls_types::SymbolInformation {
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

pub(crate) fn field_completion_tables(
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

/// The byte offset of the `.` that opens the method position at `offset`, if
/// there is one.
///
/// Accepts a partially typed name after the dot (`"abc".sl|`), because `.` is not
/// a token character and the completion prefix therefore arrives empty.
fn method_dot_offset(source: &str, offset: usize) -> Option<usize> {
    let before = source.get(..offset)?;
    let trailing = before
        .chars()
        .rev()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
        .count();
    let (at, ch) = before.char_indices().rev().nth(trailing)?;
    if ch == '.' { Some(at) } else { None }
}

/// The method a cursor sits on: the `IdiomFunction` node and the method name.
///
/// `token_at` treats `.` as a boundary, so hover on `'abc'.len()` only ever sees
/// the bare word `len`. That is why this works from the tree instead: the bare
/// word route answers with the SurrealQL *keyword* `AT` for `.at(0)` and `SPLIT`
/// for `.split(',')` — a wrong answer rather than a missing one.
pub(crate) fn method_at<'tree>(
    analysis: &'tree DocumentAnalysis,
    offset: usize,
) -> Option<(tree_sitter::Node<'tree>, String)> {
    let node = analysis
        .tree
        .root_node()
        .named_descendant_for_byte_range(offset, offset)?;
    if node.kind() != crate::semantic::node_kind::FUNCTION_NAME {
        return None;
    }
    let idiom = node.parent()?;
    if idiom.kind() != crate::semantic::node_kind::IDIOM_FUNCTION {
        return None;
    }
    let name = crate::semantic::node_kind::text_of(&analysis.text, node)?;
    Some((idiom, name.to_string()))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Arc;

    use ls_types::{DiagnosticSeverity, Location, Position, Range, Uri};

    use crate::config::{AuthContext, ServerSettings};
    use crate::semantic::types::{
        DocumentAnalysis, EventDef, FunctionDef, IndexDef, PermissionMode, PermissionRule,
        QueryAction, SymbolOrigin, TableDef, TargetResolution, WorkspaceIndex,
    };

    use super::{MergedSemanticModel, is_record_type_context};

    /// A placeholder parse tree for the `DocumentAnalysis` literals in
    /// these model tests, which exercise the derived fields, not the tree.
    fn empty_tree() -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::grammar::language())
            .expect("load grammar");
        parser.parse("", None).expect("parse empty")
    }

    #[test]
    fn local_definitions_override_inferred() {
        let uri = Uri::from_str("file:///workspace/schema.surql").expect("valid uri");
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
            tree: empty_tree(),
            tables: vec![inferred, explicit.clone()],
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
        let mut workspace = WorkspaceIndex::default();
        workspace
            .documents
            .insert(analysis.uri.clone(), Arc::new(analysis));
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
                Uri::from_str("file:///workspace/schema.surql").expect("valid uri"),
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
                Uri::from_str("file:///workspace/query.surql").expect("valid uri"),
                Range::default(),
            ),
            source_preview: "SELECT * FROM person".to_string(),
            target_refs: Vec::new(),
            field_refs: Vec::new(),
            target_resolution: TargetResolution::Static,
        };
        let result = model.semantic_diagnostics(
            &DocumentAnalysis {
                uri: Uri::from_str("file:///workspace/query.surql").expect("valid uri"),
                text: String::new(),
                tree: empty_tree(),
                tables: Vec::new(),
                events: Vec::new(),
                indexes: Vec::new(),
                fields: Vec::new(),
                functions: Vec::new(),
                params: Vec::new(),
                accesses: Vec::new(),
                analyzers: Vec::new(),
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
        // SELECT and RELATE are deliberately exempt from static
        // permission checks (their rules are usually row-level and
        // can't be evaluated without an actual record), so this test
        // uses CREATE to exercise the denied-permission code path.
        let settings = ServerSettings::default();
        let table = TableDef {
            name: "person".to_string(),
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
            location: Location::new(
                Uri::from_str("file:///workspace/schema.surql").expect("valid uri"),
                Range::default(),
            ),
        };
        let mut model = MergedSemanticModel::default();
        model.tables.insert("person".to_string(), table);

        let diagnostics = model.semantic_diagnostics(
            &DocumentAnalysis {
                uri: Uri::from_str("file:///workspace/query.surql").expect("valid uri"),
                text: String::new(),
                tree: empty_tree(),
                tables: Vec::new(),
                events: Vec::new(),
                indexes: Vec::new(),
                fields: Vec::new(),
                functions: Vec::new(),
                params: Vec::new(),
                accesses: Vec::new(),
                analyzers: Vec::new(),
                query_facts: vec![crate::semantic::types::QueryFact {
                    action: QueryAction::Create,
                    target_tables: vec!["person".to_string()],
                    touched_fields: Vec::new(),
                    dynamic: false,
                    location: Location::new(
                        Uri::from_str("file:///workspace/query.surql").expect("valid uri"),
                        Range::default(),
                    ),
                    source_preview: "CREATE person".to_string(),
                    target_refs: Vec::new(),
                    field_refs: Vec::new(),
                    target_resolution: TargetResolution::Static,
                }],
                references: Vec::new(),
                syntax_diagnostics: Vec::new(),
                document_symbols: Vec::new(),
            },
            &settings,
        );

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            diagnostics[0].source.as_deref(),
            Some("surreal-language-server")
        );
        assert!(crate::semantic::codes::has_code(
            &diagnostics[0],
            crate::semantic::codes::PERMISSION_DENIED
        ));
    }

    #[test]
    fn select_and_relate_skip_permission_checks() {
        // Even with `PERMISSIONS FOR select NONE` (which would block
        // every reader at runtime), the LSP should not flag SELECTs
        // because runtime row-level rules make static evaluation
        // unreliable. The same applies to RELATE.
        let settings = ServerSettings::default();
        let person = TableDef {
            name: "person".to_string(),
            schema_mode: None,
            comment: None,
            permissions: vec![
                PermissionRule {
                    actions: vec![QueryAction::Select],
                    mode: PermissionMode::None,
                    raw: "PERMISSIONS FOR select NONE".to_string(),
                    origin: SymbolOrigin::Local,
                    location: None,
                },
                PermissionRule {
                    actions: vec![QueryAction::Relate],
                    mode: PermissionMode::None,
                    raw: "PERMISSIONS FOR relate NONE".to_string(),
                    origin: SymbolOrigin::Local,
                    location: None,
                },
            ],
            origin: SymbolOrigin::Local,
            explicit: true,
            inference: None,
            location: Location::new(
                Uri::from_str("file:///workspace/schema.surql").expect("valid uri"),
                Range::default(),
            ),
        };
        let mut model = MergedSemanticModel::default();
        model.tables.insert("person".to_string(), person);

        let analysis_uri = Uri::from_str("file:///workspace/query.surql").expect("valid uri");
        let make_fact = |action: QueryAction| crate::semantic::types::QueryFact {
            action,
            target_tables: vec!["person".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: Location::new(analysis_uri.clone(), Range::default()),
            source_preview: String::new(),
            target_refs: Vec::new(),
            field_refs: Vec::new(),
            target_resolution: TargetResolution::Static,
        };

        let diagnostics = model.semantic_diagnostics(
            &DocumentAnalysis {
                uri: analysis_uri.clone(),
                text: String::new(),
                tree: empty_tree(),
                tables: Vec::new(),
                events: Vec::new(),
                indexes: Vec::new(),
                fields: Vec::new(),
                functions: Vec::new(),
                params: Vec::new(),
                accesses: Vec::new(),
                analyzers: Vec::new(),
                query_facts: vec![
                    make_fact(QueryAction::Select),
                    make_fact(QueryAction::Relate),
                ],
                references: Vec::new(),
                syntax_diagnostics: Vec::new(),
                document_symbols: Vec::new(),
            },
            &settings,
        );

        assert!(
            diagnostics.is_empty(),
            "expected no diagnostics, got {diagnostics:?}"
        );
    }

    #[test]
    fn record_type_hover_resolves_underlying_table() {
        let uri = Uri::from_str("file:///workspace/schema.surql").expect("valid uri");
        let mut workspace = WorkspaceIndex::default();
        workspace.documents.insert(
            uri.clone(),
            Arc::new(DocumentAnalysis {
                uri: uri.clone(),
                text: String::new(),
                tree: empty_tree(),
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
                analyzers: Vec::new(),
                query_facts: Vec::new(),
                references: Vec::new(),
                syntax_diagnostics: Vec::new(),
                document_symbols: Vec::new(),
            }),
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
        let uri = Uri::from_str("file:///workspace/schema.surql").expect("valid uri");
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
            Arc::new(DocumentAnalysis {
                uri: Uri::from_str("file:///workspace/schema.surql").expect("valid uri"),
                text: String::new(),
                tree: empty_tree(),
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
                analyzers: Vec::new(),
                query_facts: Vec::new(),
                references: Vec::new(),
                syntax_diagnostics: Vec::new(),
                document_symbols: Vec::new(),
            }),
        );
        let model = MergedSemanticModel::build(&workspace, &Default::default());

        assert_eq!(model.definition_for_token("record<person>"), Some(location));
    }

    #[test]
    fn table_hover_lists_indexes_events_and_permission_posture() {
        let uri = Uri::from_str("file:///workspace/schema.surql").expect("valid uri");
        let analysis = DocumentAnalysis {
            uri: uri.clone(),
            text: String::new(),
            tree: empty_tree(),
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
            analyzers: Vec::new(),
            query_facts: Vec::new(),
            references: Vec::new(),
            syntax_diagnostics: Vec::new(),
            document_symbols: Vec::new(),
        };
        let mut workspace = WorkspaceIndex::default();
        workspace
            .documents
            .insert(analysis.uri.clone(), Arc::new(analysis));
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
    fn hover_answers_for_the_namespaces_the_curated_table_never_covered() {
        // 18 of the 20 advertised namespaces answered nothing at all before the
        // generated catalogue existed.
        let model = MergedSemanticModel::default();
        for (token, expected) in [
            (
                "math::clamp",
                "math::clamp(arg: number, min: number, max: number)",
            ),
            ("array::at", "array::at(array: array, i: int)"),
            ("crypto::sha256", "crypto::sha256(arg: string)"),
            (
                "time::floor",
                "time::floor(val: datetime, duration: duration)",
            ),
        ] {
            let hover = model
                .hover_markdown_for_token(token, None)
                .unwrap_or_else(|| panic!("no hover for {token}"));
            assert!(
                hover.contains(expected),
                "hover for {token} lacked `{expected}`:\n{hover}"
            );
        }
    }

    #[test]
    fn hover_for_a_generated_builtin_links_its_namespace_docs() {
        let model = MergedSemanticModel::default();
        let hover = model
            .hover_markdown_for_token("math::clamp", None)
            .expect("hover");
        assert!(hover.contains("functions/database/math"), "{hover}");
    }

    #[test]
    fn hover_still_prefers_the_curated_prose_where_it_exists() {
        // The curated table carries a summary and a return type that no
        // generator can produce; it must keep winning.
        let model = MergedSemanticModel::default();
        let hover = model
            .hover_markdown_for_token("string::len", None)
            .expect("hover");
        assert!(hover.contains("Returns the length of a string"), "{hover}");
        assert!(hover.contains("-> number"), "{hover}");
    }

    #[test]
    fn hover_marks_a_name_that_parses_but_cannot_be_called() {
        let model = MergedSemanticModel::default();
        let hover = model
            .hover_markdown_for_token("duration::set_day", None)
            .expect("hover");
        assert!(
            hover.contains("no implementation is reachable"),
            "the nine parse-but-not-callable names should say so:\n{hover}"
        );
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
        let uri = Uri::from_str("file:///workspace/schema.surql").expect("valid uri");
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
            target_refs: Vec::new(),
            field_refs: Vec::new(),
            target_resolution: TargetResolution::Static,
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
            target_refs: Vec::new(),
            field_refs: Vec::new(),
            target_resolution: TargetResolution::Static,
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
    fn column_completion_items_returns_only_fields() {
        use ls_types::CompletionItemKind;

        let uri = Uri::from_str("file:///workspace/schema.surql").expect("valid uri");
        let mut model = MergedSemanticModel::default();
        // Tables and functions should NOT leak into the column-only output.
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
                location: Location::new(uri.clone(), Range::default()),
            },
        );
        model.functions.insert(
            "fn::greet".to_string(),
            FunctionDef {
                name: "fn::greet".to_string(),
                params: Vec::new(),
                return_type: None,
                language: crate::semantic::types::FunctionLanguage::SurrealQL,
                comment: None,
                permissions: Vec::new(),
                origin: SymbolOrigin::Local,
                explicit: true,
                inference: None,
                location: Location::new(uri.clone(), Range::default()),
                selection_range: Range::default(),
                body_range: None,
                called_functions: Vec::new(),
            },
        );
        for field_name in ["email", "name"] {
            model.fields.insert(
                ("person".to_string(), field_name.to_string()),
                crate::semantic::types::FieldDef {
                    table: "person".to_string(),
                    name: field_name.to_string(),
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
        }

        let items = model.column_completion_items("", &["person".to_string()], false, None);
        assert_eq!(items.len(), 2, "expected exactly the two fields");
        assert!(
            items
                .iter()
                .all(|item| item.kind == Some(CompletionItemKind::FIELD)),
            "all items must be FIELD; got {:?}",
            items
                .iter()
                .map(|i| (i.label.clone(), i.kind))
                .collect::<Vec<_>>()
        );
        let labels: Vec<_> = items.iter().map(|item| item.label.clone()).collect();
        assert!(labels.contains(&"email".to_string()));
        assert!(labels.contains(&"name".to_string()));
        // No table / function leakage.
        assert!(!labels.iter().any(|l| l == "person" || l == "fn::greet"));
    }

    #[test]
    fn field_sort_text_puts_fields_above_functions_in_loose_mode() {
        let uri = Uri::from_str("file:///workspace/schema.surql").expect("valid uri");
        let mut model = MergedSemanticModel::default();
        model.fields.insert(
            ("person".to_string(), "email".to_string()),
            crate::semantic::types::FieldDef {
                table: "person".to_string(),
                name: "email".to_string(),
                type_expr: None,
                comment: None,
                permissions: Vec::new(),
                origin: SymbolOrigin::Local,
                explicit: true,
                inference: None,
                location: Location::new(uri.clone(), Range::default()),
            },
        );
        model.functions.insert(
            "fn::greet".to_string(),
            FunctionDef {
                name: "fn::greet".to_string(),
                params: Vec::new(),
                return_type: None,
                language: crate::semantic::types::FunctionLanguage::SurrealQL,
                comment: None,
                permissions: Vec::new(),
                origin: SymbolOrigin::Local,
                explicit: true,
                inference: None,
                location: Location::new(uri, Range::default()),
                selection_range: Range::default(),
                body_range: None,
                called_functions: Vec::new(),
            },
        );

        let statement_fact = crate::semantic::types::QueryFact {
            action: QueryAction::Select,
            target_tables: vec!["person".to_string()],
            touched_fields: Vec::new(),
            dynamic: false,
            location: Location::new(
                Uri::from_str("file:///workspace/schema.surql").expect("valid uri"),
                Range::default(),
            ),
            source_preview: "SELECT email FROM person".to_string(),
            target_refs: Vec::new(),
            field_refs: Vec::new(),
            target_resolution: TargetResolution::Static,
        };
        let items = model.completion_items("", false, None, Some(&statement_fact), None);

        let field_sort = items
            .iter()
            .find(|item| item.label == "email")
            .and_then(|item| item.sort_text.clone())
            .expect("field item must have sort_text");
        let function_sort = items
            .iter()
            .find(|item| item.label == "fn::greet")
            .and_then(|item| item.sort_text.clone())
            .expect("function item must have sort_text");
        assert!(
            field_sort < function_sort,
            "field sort_text `{field_sort}` must sort before function sort_text `{function_sort}`"
        );
        assert!(field_sort.starts_with("0-fld-"));
        assert!(function_sort.starts_with("1-"));
    }

    #[test]
    fn remote_functions_cannot_be_renamed() {
        let uri = Uri::from_str("file:///workspace/schema.surql").expect("valid uri");
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

    fn model_with_person_table() -> (MergedSemanticModel, DocumentAnalysis) {
        let uri = Uri::from_str("file:///workspace/query.surql").expect("valid uri");
        let mut model = MergedSemanticModel::default();
        model.tables.insert(
            "person".to_string(),
            TableDef {
                name: "person".to_string(),
                schema_mode: None,
                comment: None,
                permissions: vec![PermissionRule {
                    actions: vec![QueryAction::Select],
                    mode: PermissionMode::Full,
                    raw: "PERMISSIONS FULL".to_string(),
                    origin: SymbolOrigin::Local,
                    location: None,
                }],
                origin: SymbolOrigin::Local,
                explicit: true,
                inference: None,
                location: Location::new(
                    Uri::from_str("file:///workspace/schema.surql").expect("valid uri"),
                    Range::default(),
                ),
            },
        );
        let analysis = DocumentAnalysis {
            uri,
            text: String::new(),
            tree: empty_tree(),
            tables: Vec::new(),
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
        (model, analysis)
    }

    #[test]
    fn code_action_matches_on_stable_code_and_data_not_message_text() {
        let (model, analysis) = model_with_person_table();
        let diagnostic = ls_types::Diagnostic {
            range: Range::default(),
            code: crate::semantic::codes::as_code(crate::semantic::codes::UNKNOWN_TABLE),
            // Sentinel wording proves the matcher never consults the
            // message when the code + data are present.
            message: "totally reworded message".to_string(),
            data: Some(serde_json::json!({ "table": "prson" })),
            ..Default::default()
        };

        let actions = model.code_actions(&analysis.uri.clone(), &analysis, &[diagnostic]);
        let quick_fix = actions
            .iter()
            .find_map(|action| match action {
                ls_types::CodeActionOrCommand::CodeAction(action)
                    if action.title.starts_with("Replace") =>
                {
                    Some(action)
                }
                _ => None,
            })
            .expect("code+data diagnostic must yield the quick fix");
        assert_eq!(quick_fix.title, "Replace `prson` with `person`");
    }

    #[test]
    fn code_action_legacy_message_fallback_still_works() {
        // Pre-0.3 diagnostics carried no code; the string fallback
        // keeps their quick fixes alive for one release. Remove in 0.4.
        let (model, analysis) = model_with_person_table();
        let diagnostic = ls_types::Diagnostic {
            range: Range::default(),
            message: "Unknown table `prson`.".to_string(),
            ..Default::default()
        };

        let actions = model.code_actions(&analysis.uri.clone(), &analysis, &[diagnostic]);
        assert!(
            actions.iter().any(|action| matches!(
                action,
                ls_types::CodeActionOrCommand::CodeAction(action)
                    if action.title == "Replace `prson` with `person`"
            )),
            "legacy message-only diagnostic must still yield the quick fix"
        );
    }

    #[test]
    fn code_action_survives_clients_that_strip_diagnostic_data() {
        // Several clients round-trip `code` but drop the non-standard
        // `data` field — the message fallback must still work.
        let (model, analysis) = model_with_person_table();
        let diagnostic = ls_types::Diagnostic {
            range: Range::default(),
            code: crate::semantic::codes::as_code(crate::semantic::codes::UNKNOWN_TABLE),
            message: "Unknown table `prson`.".to_string(),
            data: None,
            ..Default::default()
        };

        let actions = model.code_actions(&analysis.uri.clone(), &analysis, &[diagnostic]);
        assert!(actions.iter().any(|action| matches!(
            action,
            ls_types::CodeActionOrCommand::CodeAction(action)
                if action.title == "Replace `prson` with `person`"
        )));
    }

    #[test]
    fn code_action_message_fallback_parses_did_you_mean_shape() {
        // The 0.3 message carries a suggestion suffix; the fallback
        // parser must extract both table and suggestion from it.
        let (model, analysis) = model_with_person_table();
        let diagnostic = ls_types::Diagnostic {
            range: Range::default(),
            message: "Unknown table `zzz`. Did you mean `person`?".to_string(),
            ..Default::default()
        };

        // `zzz` has no near-miss, so only the parsed suggestion can
        // produce this action.
        let actions = model.code_actions(&analysis.uri.clone(), &analysis, &[diagnostic]);
        assert!(actions.iter().any(|action| matches!(
            action,
            ls_types::CodeActionOrCommand::CodeAction(action)
                if action.title == "Replace `zzz` with `person`"
        )));
    }

    #[test]
    fn code_action_honours_precomputed_suggestion_in_data() {
        let (model, analysis) = model_with_person_table();
        let diagnostic = ls_types::Diagnostic {
            range: Range::default(),
            code: crate::semantic::codes::as_code(crate::semantic::codes::UNKNOWN_TABLE),
            message: "Unknown table `zzz`. Did you mean `person`?".to_string(),
            data: Some(serde_json::json!({ "table": "zzz", "suggestion": "person" })),
            ..Default::default()
        };

        // `zzz` is nowhere near `person` by string distance, so only
        // the precomputed suggestion can produce this action.
        let actions = model.code_actions(&analysis.uri.clone(), &analysis, &[diagnostic]);
        assert!(actions.iter().any(|action| matches!(
            action,
            ls_types::CodeActionOrCommand::CodeAction(action)
                if action.title == "Replace `zzz` with `person`"
        )));
    }
}
