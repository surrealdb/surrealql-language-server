//! Expression typing and the call-site type check.
//!
//! # Why this lives model-side
//!
//! Type checking needs two things at once: the parse tree, and every
//! symbol in the workspace. The analyzer
//! ([`crate::semantic::analyzer::analyze_document`]) has the tree but runs
//! per-document with no cross-file knowledge — and a `fn::` definition
//! routinely lives in another file. [`MergedSemanticModel`] has the
//! symbols. So the check runs from
//! [`MergedSemanticModel::semantic_diagnostics`], which receives both the
//! model and a `DocumentAnalysis` carrying its cached tree.
//!
//! # The silence rule
//!
//! Everything here is subordinate to one invariant: **an argument is only
//! reported when both its own type and the declared parameter type are
//! confidently known and provably incompatible.** [`infer_expr_type`]
//! returns [`TypeExpr::Unknown`] for anything it cannot pin down, and
//! [`assignable`] turns any doubt into [`Verdict::Unknown`], which is
//! silent. Adding a new expression arm can only ever *reduce* silence, so
//! new arms must be correct rather than merely plausible.

use ls_types::{Diagnostic, DiagnosticSeverity, Range};
use tree_sitter::Node;

use crate::config::ServerSettings;
use strsim::jaro_winkler;

use crate::grammar::{SPECIAL_VARIABLES, builtin_return_type};
use crate::semantic::assign::{ObjectFault, Verdict, assignable, object_faults};
use crate::semantic::codes;
use crate::semantic::node_kind as k;
use crate::semantic::text::byte_range_to_lsp;
use crate::semantic::type_expr::TypeExpr;
use crate::semantic::types::{DocumentAnalysis, MergedSemanticModel};

const SOURCE: &str = "surreal-language-server";

/// What [`infer_expr_type`] needs to resolve names it meets.
pub struct TypeCtx<'a> {
    pub model: &'a MergedSemanticModel,
    pub source: &'a str,
    pub bindings: &'a BindingTable,
}

/// Where a variable came from. Only used for hover wording today, but it
/// is the natural place to hang go-to-definition later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    Let,
    FunctionParam,
    ForLoop,
    ClosureParam,
}

impl BindingKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Let => "LET",
            Self::FunctionParam => "parameter",
            Self::ForLoop => "FOR",
            Self::ClosureParam => "closure parameter",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Binding {
    /// Includes the `$` sigil, matching how variables appear in source.
    pub name: String,
    pub ty: TypeExpr,
    /// From `LET $x: int = …`, when written.
    pub declared: Option<TypeExpr>,
    /// Byte span of the name at its declaration site.
    pub decl_span: std::ops::Range<usize>,
    /// Byte span over which the binding is visible.
    ///
    /// Byte offsets rather than an LSP `Range`: resolution keys off
    /// `node.start_byte()`, and comparing integers avoids a
    /// position round-trip at every variable reference.
    ///
    /// For a `LET` this starts at the *end* of the statement, so that
    /// `LET $x = $x` cannot resolve to itself. Hover therefore has to
    /// consult [`Self::decl_span`] as well — see [`BindingTable::at`].
    pub scope: std::ops::Range<usize>,
    pub kind: BindingKind,
}

/// Every variable binding in a document, in source order.
///
/// A flat `Vec` with "last preceding match wins" gives shadowing
/// (`LET $x = 1; LET $x = 'a';`) without building a scope tree — the
/// scope span on each entry is enough to reject bindings that are out of
/// view.
#[derive(Debug, Clone, Default)]
pub struct BindingTable {
    entries: Vec<Binding>,
}

/// Is `at` inside `scope`, treating the end as **inclusive**?
///
/// A cursor sitting on the final byte of a scope is still in it — most
/// visibly when completing at the very end of a document, where the
/// offset equals the root node's end and a half-open range would say no.
fn covers(scope: &std::ops::Range<usize>, at: usize) -> bool {
    at >= scope.start && at <= scope.end
}

impl BindingTable {
    /// The binding a `$name` reference at byte offset `at` resolves to:
    /// the last one declared before that point whose scope still covers it.
    pub fn resolve(&self, name: &str, at: usize) -> Option<&Binding> {
        self.entries
            .iter()
            .rev()
            .find(|binding| binding.name == name && covers(&binding.scope, at))
    }

    /// The binding a cursor at `at` refers to, whether that is a *use*
    /// inside the scope or the *declaration* itself.
    ///
    /// Hover wants both: `LET $r = …` is the most natural place to ask
    /// what `$r` is, but the declaration sits just outside the scope span
    /// by construction.
    pub fn at(&self, name: &str, at: usize) -> Option<&Binding> {
        self.resolve(name, at).or_else(|| {
            self.entries
                .iter()
                .rev()
                .find(|binding| binding.name == name && covers(&binding.decl_span, at))
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = &Binding> {
        self.entries.iter()
    }

    /// Bindings visible at `at`, most recent shadow first.
    pub fn visible_at(&self, at: usize) -> Vec<&Binding> {
        let mut seen = Vec::new();
        let mut out: Vec<&Binding> = Vec::new();
        for binding in self.entries.iter().rev() {
            if !covers(&binding.scope, at) || seen.contains(&binding.name) {
                continue;
            }
            seen.push(binding.name.clone());
            out.push(binding);
        }
        out
    }
}

/// Best-effort type of an expression node.
///
/// Returns [`TypeExpr::Unknown`] rather than guessing. Every `Unknown` is
/// a diagnostic that will not fire, which is the safe direction.
pub fn infer_expr_type(node: Node<'_>, ctx: &TypeCtx<'_>) -> TypeExpr {
    let text = || k::text_of(ctx.source, node).unwrap_or_default();

    match node.kind() {
        k::STRING => TypeExpr::Scalar(string_kind(text()).to_string()),
        k::FORMAT_STRING => TypeExpr::Scalar("string".to_string()),
        k::REGEX => TypeExpr::Scalar("regex".to_string()),
        k::DURATION => TypeExpr::Scalar("duration".to_string()),
        k::BOOL => TypeExpr::Scalar("bool".to_string()),
        k::NONE => TypeExpr::Scalar("none".to_string()),
        k::POINT => TypeExpr::Scalar("point".to_string()),

        k::INT => TypeExpr::Scalar("int".to_string()),
        k::FLOAT => TypeExpr::Scalar("float".to_string()),
        k::DECIMAL => TypeExpr::Scalar("decimal".to_string()),
        // `Number` wraps an optional sign plus the concrete numeric kind.
        k::NUMBER => k::named_children(node)
            .first()
            .map(|inner| infer_expr_type(*inner, ctx))
            .unwrap_or(TypeExpr::Unknown),

        k::ARRAY => TypeExpr::Array(Box::new(join_types(
            k::named_children(node)
                .into_iter()
                .filter(|child| !is_trivia(*child))
                .map(|child| infer_expr_type(child, ctx))
                .collect(),
        ))),
        k::SET => TypeExpr::Set(Box::new(join_types(
            k::named_children(node)
                .into_iter()
                .filter(|child| !is_trivia(*child))
                .map(|child| infer_expr_type(child, ctx))
                .collect(),
        ))),
        k::OBJECT => object_literal_type(node, ctx),

        k::RECORD_ID | k::RANGE_RECORD_ID => k::find_child(node, k::RECORD_TB_IDENT)
            .and_then(|table| k::text_of(ctx.source, table))
            .map(|table| TypeExpr::Record(vec![table.to_string()]))
            .unwrap_or_else(|| TypeExpr::Record(Vec::new())),

        // `<int> "5"` — the cast target wins.
        k::TYPE_CAST => k::named_children(node)
            .into_iter()
            .find(|child| k::TYPE_KINDS.contains(&child.kind()))
            .map(|ty| TypeExpr::from_node(ty, ctx.source))
            .unwrap_or(TypeExpr::Unknown),

        k::FUNCTION_CALL => call_return_type(node, ctx),

        // A parenthesised expression is its content.
        k::SUB_QUERY => k::named_children(node)
            .into_iter()
            .find(|child| !is_trivia(*child))
            .map(|inner| infer_expr_type(inner, ctx))
            .unwrap_or(TypeExpr::Unknown),

        k::PREFIX_EXPRESSION => TypeExpr::Scalar("bool".to_string()),

        k::VARIABLE_NAME => ctx
            .bindings
            .resolve(text(), node.start_byte())
            .map(|binding| binding.ty.clone())
            .unwrap_or(TypeExpr::Unknown),

        // Deliberately unhandled, needing either idiom/field resolution or
        // a method-signature table (Phase 5): Path, Idiom, Subscript,
        // IdiomFunction, BinaryExpression, Closure, Range, Block,
        // IfElseStatement, and every statement kind.
        _ => TypeExpr::Unknown,
    }
}

/// Collect every variable binding in the document, typing each
/// initializer as we go.
///
/// One source-ordered pass is sufficient: SurrealQL `LET` is sequential,
/// so a binding can only refer to ones already declared. No fixpoint
/// needed.
pub fn resolve_bindings(analysis: &DocumentAnalysis, model: &MergedSemanticModel) -> BindingTable {
    let mut table = BindingTable::default();
    let root = analysis.tree.root_node();
    collect_bindings(root, root.end_byte(), &analysis.text, model, &mut table);
    table
}

fn collect_bindings(
    node: Node<'_>,
    scope_end: usize,
    source: &str,
    model: &MergedSemanticModel,
    table: &mut BindingTable,
) {
    match node.kind() {
        k::LET_STATEMENT => {
            bind_let(node, scope_end, source, model, table);
            return;
        }
        k::DEFINE_STATEMENT => {
            // A function's parameters are visible throughout its body.
            //
            // Deliberately no `return`: a direct `Block` child is the
            // *exception*. `DEFINE EVENT … THEN { … }` nests its block in a
            // `ThenClause`, and `DEFINE FIELD` nests one in DEFAULT / VALUE
            // / ASSERT. Returning here left every `LET` inside those
            // invisible — which is why hovering them showed nothing and the
            // undefined-variable check flagged their uses.
            if let Some(body) = k::find_child(node, k::BLOCK) {
                bind_param_definitions(node, &body, BindingKind::FunctionParam, source, table);
            }
        }
        k::FOR_STATEMENT => {
            bind_for(node, source, model, table);
        }
        k::CLOSURE => {
            // Scope closure parameters over the *whole* closure, not over a
            // `Block` child: a closure body may be a bare expression
            // (`|$x| $x != 'Complete'`) with no block at all. Since the
            // parameters mean nothing outside the closure anyway, the node's
            // own span is both simpler and correct for every body form.
            bind_param_definitions(node, &node, BindingKind::ClosureParam, source, table);
        }
        _ => {}
    }

    // A `Block` opens a new scope; everything else inherits the current one.
    let inner_end = if node.kind() == k::BLOCK {
        node.end_byte()
    } else {
        scope_end
    };
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_bindings(child, inner_end, source, model, table);
    }
}

/// `LET $x[: T] = value` — visible from the end of the statement to the
/// end of the enclosing scope.
fn bind_let(
    node: Node<'_>,
    scope_end: usize,
    source: &str,
    model: &MergedSemanticModel,
    table: &mut BindingTable,
) {
    let children = k::named_children(node);
    let Some(definition) = children
        .iter()
        .find(|child| child.kind() == k::PARAM_DEFINITION)
    else {
        return;
    };
    let Some((name, declared, name_node)) = param_definition_parts(*definition, source) else {
        return;
    };

    let value = children
        .iter()
        .find(|child| {
            child.kind() != k::PARAM_DEFINITION && !k::is_keyword(**child) && !is_trivia(**child)
        })
        .copied();

    // Type the initializer against the bindings that already exist. The
    // borrow of `table` must end before we push, hence the block.
    let inferred = {
        let ctx = TypeCtx {
            model,
            source,
            bindings: table,
        };
        value
            .map(|value| infer_expr_type(value, &ctx))
            .unwrap_or(TypeExpr::Unknown)
    };

    // Descend first: a nested statement in the initializer may itself
    // bind things, and they belong before this entry in source order.
    if let Some(value) = value {
        collect_bindings(value, scope_end, source, model, table);
    }

    table.entries.push(Binding {
        name,
        // An explicit annotation wins: it is what the author promised, and
        // downstream code should be checked against the promise.
        ty: declared.clone().unwrap_or(inferred),
        declared,
        decl_span: name_node.0..name_node.1,
        scope: node.end_byte()..scope_end.max(node.end_byte()),
        kind: BindingKind::Let,
    });
}

/// `FOR $item IN iterable { … }` — the loop variable is the element type
/// of the iterable, visible only inside the body.
fn bind_for(node: Node<'_>, source: &str, model: &MergedSemanticModel, table: &mut BindingTable) {
    let children = k::named_children(node);
    // The grammar gives a bare `VariableName` here, not a `ParamDefinition`.
    let Some(name_node) = children
        .iter()
        .find(|child| child.kind() == k::VARIABLE_NAME)
    else {
        return;
    };
    let Some(name) = k::text_of(source, *name_node) else {
        return;
    };
    let body = k::find_child(node, k::BLOCK);
    let iterable = children
        .iter()
        .find(|child| {
            child.kind() != k::VARIABLE_NAME
                && child.kind() != k::BLOCK
                && !k::is_keyword(**child)
                && !is_trivia(**child)
        })
        .copied();

    let element = {
        let ctx = TypeCtx {
            model,
            source,
            bindings: table,
        };
        match iterable.map(|node| infer_expr_type(node, &ctx)) {
            Some(TypeExpr::Array(inner)) | Some(TypeExpr::Set(inner)) => *inner,
            _ => TypeExpr::Unknown,
        }
    };

    if let Some(body) = body {
        table.entries.push(Binding {
            name: name.to_string(),
            ty: element,
            declared: None,
            decl_span: name_node.start_byte()..name_node.end_byte(),
            scope: body.start_byte()..body.end_byte(),
            kind: BindingKind::ForLoop,
        });
        // No descent here — the caller's generic walk visits the body (and
        // the iterable), which keeps scope handling in exactly one place.
    }
}

/// Bind every `ParamDefinition` child of `owner` over `body`'s span.
fn bind_param_definitions(
    owner: Node<'_>,
    body: &Node<'_>,
    kind: BindingKind,
    source: &str,
    table: &mut BindingTable,
) {
    for definition in k::named_children(owner)
        .into_iter()
        .filter(|child| child.kind() == k::PARAM_DEFINITION)
    {
        let Some((name, declared, name_span)) = param_definition_parts(definition, source) else {
            continue;
        };
        table.entries.push(Binding {
            name,
            ty: declared.clone().unwrap_or(TypeExpr::Unknown),
            declared,
            decl_span: name_span.0..name_span.1,
            scope: body.start_byte()..body.end_byte(),
            kind,
        });
    }
}

/// `ParamDefinition(VariableName, [Colon, Type])` → name, declared type,
/// and the name node's byte span.
fn param_definition_parts(
    definition: Node<'_>,
    source: &str,
) -> Option<(String, Option<TypeExpr>, (usize, usize))> {
    let children = k::named_children(definition);
    let name_node = children
        .iter()
        .find(|child| child.kind() == k::VARIABLE_NAME)?;
    let name = k::text_of(source, *name_node)?.to_string();
    let declared = children
        .iter()
        .find(|child| k::TYPE_KINDS.contains(&child.kind()))
        .map(|child| TypeExpr::from_node(*child, source));
    Some((
        name,
        declared,
        (name_node.start_byte(), name_node.end_byte()),
    ))
}

/// `String` covers plain and prefixed forms; the prefix picks the type.
fn string_kind(text: &str) -> &'static str {
    match text.as_bytes().first() {
        Some(b'd') => "datetime",
        Some(b'u') => "uuid",
        Some(b'r') => "record",
        Some(b'b') => "bytes",
        Some(b'f') => "file",
        _ => "string",
    }
}

fn is_trivia(node: Node<'_>) -> bool {
    matches!(node.kind(), k::COMMENT | k::BLOCK_COMMENT)
}

/// The element type of a collection literal.
///
/// Anything other than "every element agreed" collapses to `any`, not to
/// `Unknown`: an array literal genuinely *is* an array, and `array<any>`
/// is assignable to any `array<T>`, so this stays silent without
/// discarding the fact that it is an array at all.
fn join_types(types: Vec<TypeExpr>) -> TypeExpr {
    let mut iter = types.into_iter();
    let Some(first) = iter.next() else {
        // `[]` — an empty literal fits any array type.
        return TypeExpr::Scalar("any".to_string());
    };
    if iter.all(|other| other == first) {
        first
    } else {
        TypeExpr::Scalar("any".to_string())
    }
}

fn object_literal_type(node: Node<'_>, ctx: &TypeCtx<'_>) -> TypeExpr {
    let content = k::find_child(node, k::OBJECT_CONTENT).unwrap_or(node);
    let fields: Vec<(String, TypeExpr)> = k::named_children(content)
        .into_iter()
        .filter(|child| child.kind() == k::OBJECT_PROPERTY)
        .filter_map(|property| {
            let children = k::named_children(property);
            let key = children
                .iter()
                .find(|child| child.kind() == k::OBJECT_KEY)
                .and_then(|child| {
                    // ObjectKey wraps a `KeyName` or a `String`.
                    let inner = k::named_children(*child);
                    let target = inner.first().copied().unwrap_or(*child);
                    k::text_of(ctx.source, target)
                })
                .map(|text| text.trim_matches(['"', '\'', '`']).to_string())?;
            let value = children
                .iter()
                .find(|child| child.kind() != k::OBJECT_KEY && child.kind() != k::COLON)
                .map(|child| infer_expr_type(*child, ctx))
                .unwrap_or(TypeExpr::Unknown);
            Some((key, value))
        })
        .collect();
    TypeExpr::Object(fields)
}

/// The declared return type of a call, if we know the callee.
fn call_return_type(node: Node<'_>, ctx: &TypeCtx<'_>) -> TypeExpr {
    let Some(name) = callee_name(node, ctx.source) else {
        return TypeExpr::Unknown;
    };
    if let Some(function) = ctx.model.functions.get(name) {
        return function.return_type.clone().unwrap_or(TypeExpr::Unknown);
    }
    // A call site can be more specific than the signature — prefer it.
    if let Some(refined) = refine_builtin_return(name, node, ctx) {
        return refined;
    }
    builtin_return_type(name)
        .cloned()
        .unwrap_or(TypeExpr::Unknown)
}

/// Narrow a builtin's return type using its actual arguments.
///
/// `type::record`'s signature has to say `-> record`, because which table
/// it produces depends on an argument. But a bare `record` is assignable
/// to *every* `record<T>`, so leaving it coarse silently switches off
/// checking on anything derived from it — `type::record('company', $id)`
/// would pass happily into a `record<user>` parameter.
///
/// When the table argument is a plain string literal we know the answer
/// exactly, so read it. When it is a variable or an expression we do not,
/// and returning `None` here keeps the coarse-but-safe signature type.
///
/// This is a deliberate special case for the record constructors, not a
/// general mechanism — argument-dependent return types are otherwise rare
/// enough that a table of them would be more machinery than it earns.
fn refine_builtin_return(name: &str, call: Node<'_>, ctx: &TypeCtx<'_>) -> Option<TypeExpr> {
    let normalized = name.trim().to_ascii_lowercase();
    if !matches!(normalized.as_str(), "type::record" | "type::thing") {
        return None;
    }

    let args = argument_nodes(k::find_child(call, k::ARGUMENT_LIST)?);
    let literal = string_literal_text(*args.first()?, ctx.source)?;

    // Two accepted shapes: `type::record('user', 'beau')` names the table
    // directly, while the one-argument form takes a whole record id.
    let table = if args.len() == 1 {
        literal.split_once(':')?.0
    } else {
        literal
    };
    let table = table.trim();
    if table.is_empty() {
        return None;
    }
    Some(TypeExpr::Record(vec![table.to_string()]))
}

/// The contents of a plain quoted string literal.
///
/// Returns `None` for prefixed strings (`r'…'`, `d'…'`, …): those carry
/// their own semantics and are not table names.
fn string_literal_text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    if node.kind() != k::STRING {
        return None;
    }
    let text = k::text_of(source, node)?;
    let mut chars = text.chars();
    let quote = chars.next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    text.strip_prefix(quote)?.strip_suffix(quote)
}

/// The callee of a `FunctionCall`, when it is a plain name.
///
/// `FunctionCall`'s declared children include `RecordId` and
/// `VariableName` — `person:tobie()` and `$fn()` parse as calls too. Those
/// are dynamic dispatch and get skipped.
fn callee_name<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    let mut cursor = node.walk();
    let name = node
        .children(&mut cursor)
        .find(|child| child.kind() == k::FUNCTION_NAME)?;
    k::text_of(source, name)
}

/// The positional arguments of a call.
///
/// `Comment`/`BlockComment` are declared `extras` in this grammar, so they
/// can sit between arguments and a naive `is_named()` filter would count
/// them as arguments — turning `f(a /* note */, b)` into a 3-argument
/// call.
fn argument_nodes<'tree>(arg_list: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = arg_list.walk();
    arg_list
        .named_children(&mut cursor)
        .filter(|child| !is_trivia(*child))
        .collect()
}

/// How many arguments a call must supply.
///
/// A trailing `option<T>` parameter may be omitted, so only the leading
/// run of non-optional parameters is required.
fn required_arity(params: &[crate::semantic::types::FunctionParam]) -> usize {
    params
        .iter()
        .rposition(|param| !matches!(param.type_expr, Some(TypeExpr::Option(_))))
        .map(|index| index + 1)
        .unwrap_or(0)
}

/// Type-check every resolvable call in the document.
pub fn type_diagnostics(
    analysis: &DocumentAnalysis,
    model: &MergedSemanticModel,
    settings: &ServerSettings,
) -> Vec<Diagnostic> {
    if !settings.analysis.enable_type_checking {
        return Vec::new();
    }

    let bindings = resolve_bindings(analysis, model);
    let ctx = TypeCtx {
        model,
        source: &analysis.text,
        bindings: &bindings,
    };
    let mut diagnostics = Vec::new();
    let root = analysis.tree.root_node();
    check_calls(root, &ctx, &mut diagnostics);
    check_let_annotations(root, &ctx, &mut diagnostics);
    check_variables(root, &ctx, settings, &mut diagnostics);
    diagnostics
}

/// True when this `VariableName` is a binding *site* rather than a use.
///
/// The same three parents that [`crate::semantic::highlight`] marks as
/// declarations: a `ParamDefinition` (`LET $x`, function and closure
/// parameters), a `ForStatement`'s loop variable, and the name a
/// `DEFINE PARAM` introduces.
fn is_binding_site(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            k::PARAM_DEFINITION | k::FOR_STATEMENT | k::DEFINE_STATEMENT
        )
    })
}

/// Report `$variable` references that nothing in scope binds.
///
/// SurrealDB substitutes `NONE` for an unset parameter rather than
/// failing, so a typo like `$fx` for `$f` silently changes what a query
/// means instead of announcing itself. That is precisely the class of bug
/// worth catching statically.
///
/// A name is considered bound when it comes from any of:
/// the [`BindingTable`] (`LET`, function/closure parameters, `FOR` loop
/// variables), a `DEFINE PARAM`, one of the language's
/// [`SPECIAL_VARIABLES`], or `analysis.externalParams` for names the
/// caller binds at runtime.
fn check_variables(
    root: Node<'_>,
    ctx: &TypeCtx<'_>,
    settings: &ServerSettings,
    out: &mut Vec<Diagnostic>,
) {
    for node in variable_references(root) {
        let Some(name) = k::text_of(ctx.source, node) else {
            continue;
        };
        if is_bound(name, node.start_byte(), ctx, settings) {
            continue;
        }
        let hint = match nearest_in_scope(name, node.start_byte(), ctx) {
            Some(near) => format!(" Did you mean `{near}`?"),
            None => String::new(),
        };
        out.push(diagnostic(
            node_range(ctx.source, node),
            codes::UNDEFINED_VARIABLE,
            format!("`{name}` is not defined.{hint}"),
        ));
    }
}

/// Every `VariableName` in the tree that is a *use*, skipping binding
/// sites and anything inside a subtree the parser could not understand.
fn variable_references<'tree>(root: Node<'tree>) -> Vec<Node<'tree>> {
    let mut found = Vec::new();
    collect_variable_references(root, &mut found);
    found
}

fn collect_variable_references<'tree>(node: Node<'tree>, out: &mut Vec<Node<'tree>>) {
    // A broken subtree yields a broken binding table — a `LET` the parser
    // failed on binds nothing, and every later use of it would be reported
    // as undefined. Syntax errors are already surfaced on their own; do not
    // pile invented name errors on top of them.
    if node.is_error() || node.is_missing() {
        return;
    }
    if node.kind() == k::VARIABLE_NAME {
        if !is_binding_site(node) {
            out.push(node);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_variable_references(child, out);
    }
}

fn is_bound(name: &str, at: usize, ctx: &TypeCtx<'_>, settings: &ServerSettings) -> bool {
    if ctx.bindings.at(name, at).is_some() || ctx.model.params.contains_key(name) {
        return true;
    }
    if SPECIAL_VARIABLES
        .iter()
        .any(|(special, _)| special.eq_ignore_ascii_case(name))
    {
        return true;
    }
    // `externalParams` is configured without the sigil.
    let bare = name.trim_start_matches('$');
    settings
        .analysis
        .external_params
        .iter()
        .any(|declared| declared.trim_start_matches('$').eq_ignore_ascii_case(bare))
}

/// The closest in-scope name, for a "did you mean" hint.
///
/// Threshold matches [`MergedSemanticModel::find_nearest_table`], so
/// suggestions stay as conservative as the ones for table names.
fn nearest_in_scope(name: &str, at: usize, ctx: &TypeCtx<'_>) -> Option<String> {
    let candidates = ctx
        .bindings
        .visible_at(at)
        .into_iter()
        .map(|binding| binding.name.clone())
        .chain(ctx.model.params.keys().cloned())
        .chain(
            SPECIAL_VARIABLES
                .iter()
                .map(|(special, _)| (*special).to_string()),
        );

    candidates
        .map(|candidate| {
            let score = jaro_winkler(name, &candidate);
            (candidate, score)
        })
        .filter(|(_, score)| *score > 0.86)
        .max_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(candidate, _)| candidate)
}

/// `LET $x: int = 'a'` — the value must satisfy the written annotation.
fn check_let_annotations(node: Node<'_>, ctx: &TypeCtx<'_>, out: &mut Vec<Diagnostic>) {
    if node.kind() == k::LET_STATEMENT {
        let children = k::named_children(node);
        if let Some(definition) = children
            .iter()
            .find(|child| child.kind() == k::PARAM_DEFINITION)
            && let Some((name, Some(declared), _)) = param_definition_parts(*definition, ctx.source)
            && let Some(value) = children.iter().find(|child| {
                child.kind() != k::PARAM_DEFINITION
                    && !k::is_keyword(**child)
                    && !is_trivia(**child)
            })
        {
            let actual = infer_expr_type(*value, ctx);
            if assignable(&actual, &declared).is_incompatible() {
                out.push(diagnostic(
                    node_range(ctx.source, *value),
                    codes::LET_TYPE,
                    format!("`{name}` is declared `{declared}` but the value is `{actual}`."),
                ));
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        check_let_annotations(child, ctx, out);
    }
}

fn check_calls(node: Node<'_>, ctx: &TypeCtx<'_>, out: &mut Vec<Diagnostic>) {
    if node.kind() == k::FUNCTION_CALL {
        check_one_call(node, ctx, out);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        check_calls(child, ctx, out);
    }
}

fn check_one_call(node: Node<'_>, ctx: &TypeCtx<'_>, out: &mut Vec<Diagnostic>) {
    let Some(name) = callee_name(node, ctx.source) else {
        return;
    };
    // Only user-defined functions carry structured parameter types today.
    let Some(function) = ctx.model.functions.get(name) else {
        return;
    };
    let Some(arg_list) = k::find_child(node, k::ARGUMENT_LIST) else {
        return;
    };
    let args = argument_nodes(arg_list);

    let required = required_arity(&function.params);
    if args.len() < required || args.len() > function.params.len() {
        out.push(diagnostic(
            node_range(ctx.source, arg_list),
            codes::ARGUMENT_COUNT,
            format!(
                "`{name}` expects {} argument{}, found {}.",
                arity_label(required, function.params.len()),
                if function.params.len() == 1 { "" } else { "s" },
                args.len()
            ),
        ));
        // Positional comparison is meaningless once the count is wrong.
        return;
    }

    for (index, argument) in args.iter().enumerate() {
        let Some(param) = function.params.get(index) else {
            break;
        };
        let Some(expected) = param.type_expr.as_ref() else {
            continue; // Unannotated parameter: nothing to check against.
        };
        let actual = infer_expr_type(*argument, ctx);
        if assignable(&actual, expected) != Verdict::Incompatible {
            continue;
        }

        // An object literal against a declared object type deserves one
        // diagnostic per bad property, pointing at that property's value,
        // rather than a single "argument 2 is wrong" over the whole
        // literal.
        if let (TypeExpr::Object(actual_fields), TypeExpr::Object(expected_fields)) =
            (&actual, expected)
            && argument.kind() == k::OBJECT
        {
            report_object_faults(
                *argument,
                index + 1,
                name,
                actual_fields,
                expected_fields,
                ctx,
                out,
            );
            continue;
        }

        out.push(diagnostic(
            node_range(ctx.source, *argument),
            codes::ARGUMENT_TYPE,
            format!(
                "Argument {} of `{name}` expects `{expected}`, found `{actual}`.",
                index + 1
            ),
        ));
    }
}

fn report_object_faults(
    literal: Node<'_>,
    position: usize,
    name: &str,
    actual_fields: &[(String, TypeExpr)],
    expected_fields: &[(String, TypeExpr)],
    ctx: &TypeCtx<'_>,
    out: &mut Vec<Diagnostic>,
) {
    for fault in object_faults(actual_fields, expected_fields) {
        let (range, message) = match fault {
            ObjectFault::Property {
                key,
                expected,
                actual,
            } => {
                // Point at the property's value, falling back to the whole
                // literal if we cannot locate it.
                let range = property_value_node(literal, &key, ctx.source)
                    .map(|value| node_range(ctx.source, value))
                    .unwrap_or_else(|| node_range(ctx.source, literal));
                (
                    range,
                    format!(
                        "Argument {position} of `{name}`: property `{key}` expects \
                         `{expected}`, found `{actual}`."
                    ),
                )
            }
            ObjectFault::Missing { key } => (
                node_range(ctx.source, literal),
                format!("Argument {position} of `{name}`: missing required property `{key}`."),
            ),
        };
        out.push(diagnostic(range, codes::ARGUMENT_TYPE, message));
    }
}

/// The value node of `key` inside an object literal.
fn property_value_node<'tree>(
    literal: Node<'tree>,
    key: &str,
    source: &str,
) -> Option<Node<'tree>> {
    let content = k::find_child(literal, k::OBJECT_CONTENT).unwrap_or(literal);
    k::named_children(content)
        .into_iter()
        .filter(|child| child.kind() == k::OBJECT_PROPERTY)
        .find_map(|property| {
            let children = k::named_children(property);
            let name = children
                .iter()
                .find(|child| child.kind() == k::OBJECT_KEY)
                .and_then(|child| {
                    let inner = k::named_children(*child);
                    let target = inner.first().copied().unwrap_or(*child);
                    k::text_of(source, target)
                })
                .map(|text| text.trim_matches(['"', '\'', '`']))?;
            if name != key {
                return None;
            }
            children
                .iter()
                .find(|child| child.kind() != k::OBJECT_KEY && child.kind() != k::COLON)
                .copied()
        })
}

fn arity_label(required: usize, total: usize) -> String {
    if required == total {
        total.to_string()
    } else {
        format!("{required} to {total}")
    }
}

fn node_range(source: &str, node: Node<'_>) -> Range {
    byte_range_to_lsp(source, node.start_byte(), node.end_byte())
}

fn diagnostic(range: Range, code: &str, message: String) -> Diagnostic {
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        code: codes::as_code(code),
        source: Some(SOURCE.to_string()),
        message,
        ..Diagnostic::default()
    }
}
