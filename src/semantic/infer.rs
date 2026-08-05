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

use crate::grammar::{SPECIAL_VARIABLES, builtin_return_type, builtin_signature, renamed_builtin};
use crate::semantic::assign::{
    ElementFault, ObjectFault, Verdict, assignable, element_faults, object_faults,
};
use crate::semantic::codes;
use crate::semantic::node_kind as k;
use crate::semantic::text::byte_range_to_lsp;
use crate::semantic::type_expr::TypeExpr;
use crate::semantic::types::{DocumentAnalysis, FunctionLanguage, MergedSemanticModel};

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
            literal_elements(node)
                .into_iter()
                .map(|child| infer_expr_type(child, ctx))
                .collect(),
        ))),
        k::SET => TypeExpr::Set(Box::new(join_types(
            literal_elements(node)
                .into_iter()
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

        // `'abc'.len()`, `123.to_float().is_float()`. Folded link by link against
        // the engine's receiver tables — see [`path_type`].
        k::PATH => path_type(node, ctx),

        // `"" + "222"`, `1s * 2`. Typed against the engine's own operand tables,
        // after re-grouping the chain to the engine's precedence — see
        // [`chain_type`].
        k::BINARY_EXPRESSION => chain_type(node, ctx),

        // Deliberately unhandled, needing field resolution the server does not
        // have: Idiom, Subscript, IdiomFunction on its own, Closure, Range,
        // Block, IfElseStatement, and every statement kind.
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

/// Trivia, plus the brace nodes that carry no value.
///
/// `BraceOpen`/`BraceClose` are *named* children of a `Set` (and of a `Block`),
/// while the brackets of an `Array` are anonymous and never appear at all.
/// Filtering only trivia therefore leaked two braces into every set literal's
/// element list, so [`join_types`] saw more than one type and answered
/// `Scalar("any")` — making **every** set literal `set<any>`, which
/// [`crate::semantic::assign`]'s top rule then accepted against every `set<T>`.
fn is_structural(node: Node<'_>) -> bool {
    is_trivia(node) || matches!(node.kind(), k::BRACE_OPEN | k::BRACE_CLOSE)
}

/// The value-bearing elements of a collection literal, in source order.
///
/// Shared by the `Array` and `Set` arms of [`infer_expr_type`] and by the
/// element walk, so the nodes a fault points at are exactly the nodes whose
/// types were inferred. Two enumerations would let an element be checked but not
/// inferred, or the reverse.
pub(crate) fn literal_elements<'tree>(node: Node<'tree>) -> Vec<Node<'tree>> {
    k::named_children(node)
        .into_iter()
        .filter(|child| !is_structural(*child))
        .collect()
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
        // A declared type wins: it is what the author promised, and what the
        // engine coerces the result to. Fall back to what the body was read to
        // produce, which `MergedSemanticModel::build` worked out ahead of time —
        // a map lookup here, never a walk into the callee, which is what keeps
        // recursive functions from recursing. See
        // [`infer_function_return_types`].
        return function
            .return_type
            .clone()
            .or_else(|| ctx.model.inferred_function_returns.get(name).cloned())
            .unwrap_or(TypeExpr::Unknown);
    }
    // A call site can be more specific than the signature — prefer it.
    if let Some(refined) = refine_builtin_return(name, node, ctx) {
        return refined;
    }
    builtin_return_type(name).unwrap_or(TypeExpr::Unknown)
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
/// True when any part of this subtree failed to parse.
///
/// The same reason [`collect_variable_references`] refuses to walk a broken
/// subtree: a diagnostic derived from a guess about unparsed text is worse than
/// no diagnostic.
fn contains_parse_error(node: Node<'_>) -> bool {
    if node.is_error() || node.is_missing() {
        return true;
    }
    // `has_error` covers the whole subtree in one call, including unnamed
    // children a manual walk over `named_children` would skip.
    node.has_error()
}

fn argument_nodes<'tree>(arg_list: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = arg_list.walk();
    arg_list
        .named_children(&mut cursor)
        .filter(|child| !is_trivia(*child))
        .collect()
}

/// How many arguments a call must supply.
///
/// Only the leading run of parameters that cannot be omitted is required.
///
/// A parameter may be omitted when its declared type admits `NONE`, because
/// SurrealDB substitutes `NONE` for a missing argument rather than failing.
/// `option<T>` is the obvious case; `any` is the one that bites. SurrealDB's own
/// `custom_optional_args.surql` proves it: `fn::any_arg($a: any)` called as
/// `fn::any_arg()` returns a value, while `fn::one_arg($a: bool)` called the
/// same way is an error.
fn required_arity(params: &[crate::semantic::types::FunctionParam]) -> usize {
    params
        .iter()
        .rposition(|param| !admits_none(param.type_expr.as_ref()))
        .map(|index| index + 1)
        .unwrap_or(0)
}

/// True when a missing argument for this parameter is legal.
fn admits_none(declared: Option<&TypeExpr>) -> bool {
    match declared {
        Some(TypeExpr::Option(_)) => true,
        Some(TypeExpr::Scalar(name)) => {
            name.eq_ignore_ascii_case("any") || name.eq_ignore_ascii_case("value")
        }
        Some(TypeExpr::Union(members)) => members.iter().any(|member| match member {
            TypeExpr::Scalar(name) => {
                name.eq_ignore_ascii_case("none") || name.eq_ignore_ascii_case("null")
            }
            _ => false,
        }),
        _ => false,
    }
}

/// How many rounds [`infer_function_return_types`] runs.
///
/// Termination does not depend on this: a round that resolves nothing stops the
/// loop, and an entry is never revised, so at most one round per candidate can
/// be productive. The cap only bounds the pathological case, and reaching round
/// *n* requires a genuine *n*-deep chain of unannotated functions.
const MAX_INFERENCE_ROUNDS: usize = 8;

/// A `DEFINE FUNCTION` whose return type has to be read from its body.
///
/// Every field borrows the [`DocumentAnalysis`] and **nothing** borrows the
/// model. That is what lets the candidate list outlive the `&mut model` writes
/// in [`infer_function_return_types`].
struct ReturnCandidate<'tree> {
    name: &'tree str,
    /// The `DefineStatement`, not the `Block`: [`collect_bindings`] needs the
    /// whole statement to bind the parameters.
    define: Node<'tree>,
    body: Node<'tree>,
    source: &'tree str,
}

/// Read a return type out of the body of every function that declares none.
///
/// # Why a round loop and not a recursion
///
/// A round types each candidate body against the map *as it stood when the
/// round began*, then writes the results. So a body only ever asks a `HashMap`
/// what a callee returns — it never descends into that callee. Three properties
/// follow, and all three matter:
///
/// * **No cycles are possible.** `fn::fib` without an annotation, and any
///   mutually recursive pair, simply never resolve. They stay absent from the
///   map, which reads as [`TypeExpr::Unknown`], which is silent. No visited set,
///   no depth guard, no stack risk.
/// * **The result does not depend on iteration order.** Round *n* resolves
///   exactly the candidates whose call chains are *n* deep, whatever order the
///   `HashMap` hands them over in. Computing a whole round before writing it is
///   what buys this; writing inside the loop would make the outcome depend on
///   which candidate came first, and `HashMap` order is not stable across
///   rebuilds.
/// * **It terminates.** Only confident types are written and none is ever
///   revised, so the map strictly grows and the no-progress break fires.
pub fn infer_function_return_types(
    documents: &[&DocumentAnalysis],
    model: &mut MergedSemanticModel,
) {
    let candidates = return_candidates(documents, model);
    if candidates.is_empty() {
        return;
    }

    for _ in 0..MAX_INFERENCE_ROUNDS {
        let resolved: Vec<(String, TypeExpr)> = {
            // A shared reborrow, so the round can read the model while it
            // computes. It ends with this block, before the write below.
            let snapshot: &MergedSemanticModel = model;
            candidates
                .iter()
                .filter(|candidate| {
                    !snapshot
                        .inferred_function_returns
                        .contains_key(candidate.name)
                })
                .filter_map(|candidate| {
                    let mut bindings = BindingTable::default();
                    // Scoped to the `DEFINE`, not to the document, and that is a
                    // correctness requirement rather than an optimisation. A
                    // SurrealQL function body sees only its own parameters, so
                    // in
                    //
                    //     LET $greeting = 'hello';
                    //     DEFINE FUNCTION fn::f() { RETURN $greeting; };
                    //
                    // `$greeting` is unset inside the body and the engine yields
                    // NONE. Whole-document bindings would resolve it to `string`
                    // and infer a type the function cannot produce.
                    collect_bindings(
                        candidate.define,
                        candidate.define.end_byte(),
                        candidate.source,
                        snapshot,
                        &mut bindings,
                    );
                    let ctx = TypeCtx {
                        model: snapshot,
                        source: candidate.source,
                        bindings: &bindings,
                    };
                    body_return_type(candidate.body, &ctx)
                        .map(|ty| (candidate.name.to_string(), ty))
                })
                .collect()
        };

        if resolved.is_empty() {
            return;
        }
        model.inferred_function_returns.extend(resolved);
    }
}

/// Every function whose body should be read, across every document.
fn return_candidates<'tree>(
    documents: &[&'tree DocumentAnalysis],
    model: &MergedSemanticModel,
) -> Vec<ReturnCandidate<'tree>> {
    let mut candidates: Vec<ReturnCandidate<'tree>> = Vec::new();

    for analysis in documents {
        // Consult the already-extracted vector before touching the tree. This
        // pass runs inside `MergedSemanticModel::build`, which re-runs on every
        // keystroke over *every* document — so a document whose functions are
        // all annotated, or which defines none at all, must not cost a tree
        // walk.
        let worth_walking = analysis.functions.iter().any(|function| {
            function.return_type.is_none() && function.language == FunctionLanguage::SurrealQL
        });
        if !worth_walking {
            continue;
        }

        let mut defines = Vec::new();
        collect_function_defines(analysis.tree.root_node(), &analysis.text, &mut defines);
        for define in defines {
            if let Some(candidate) = return_candidate(define, analysis, model) {
                // A name defined twice in one document: keep the last, because
                // `merge_function` compares with `>=` and `absorb_analysis`
                // feeds it in source order, so the last definition is the one
                // that won the merge and the one hover shows.
                candidates.retain(|existing| existing.name != candidate.name);
                candidates.push(candidate);
            }
        }
    }

    candidates
}

/// Every `DEFINE FUNCTION` statement in the tree.
///
/// Does not descend into a body it has already captured: a `DEFINE FUNCTION`
/// nested inside another function's body is not a shape anyone writes, and the
/// bodies are the bulk of a document's nodes.
fn collect_function_defines<'tree>(node: Node<'tree>, source: &str, out: &mut Vec<Node<'tree>>) {
    if node.kind() == k::DEFINE_STATEMENT
        && crate::semantic::analyzer::define_form(node, source).as_deref() == Some("function")
    {
        out.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_function_defines(child, source, out);
    }
}

/// This definition as a candidate, when its body is the one to read.
fn return_candidate<'tree>(
    define: Node<'tree>,
    analysis: &'tree DocumentAnalysis,
    model: &MergedSemanticModel,
) -> Option<ReturnCandidate<'tree>> {
    let source = &analysis.text;
    let name = k::text_of(source, k::find_child(define, k::FUNCTION_NAME)?)?;

    // Judge against the definition that won the merge, not against this one. A
    // name can be defined in several documents, and `merge_function` keeps
    // exactly one `FunctionDef` — whose `return_type` and `location` are what
    // hover and the checker report. Reading a *losing* definition's body would
    // make the two disagree.
    let winner = model.functions.get(name)?;
    if winner.return_type.is_some() || winner.language != FunctionLanguage::SurrealQL {
        return None;
    }
    if winner.location.uri != analysis.uri {
        return None;
    }

    // A parse error anywhere in the statement, not just in the body.
    // `analyzer::function_return_type` answers `None` for a *broken* annotation
    // (`-> ` with nothing readable after it) exactly as it does for an absent
    // one, so without this the server would quietly substitute its own guess
    // for an annotation the author is halfway through typing.
    if contains_parse_error(define) {
        return None;
    }

    let body = k::find_child(define, k::BLOCK)?;
    Some(ReturnCandidate {
        name,
        define,
        body,
        source,
    })
}

/// The type of an idiom path, when every link is a method we can resolve.
///
/// Folds along the path: type the base, then let each method carry the type
/// forward. That is what makes a chain work —
/// `"019535d9-…".to_uuid().is_uuid()` types as `bool`, because `to_uuid` hands
/// a `uuid` to the next link.
///
/// Any link that is not a resolvable method stops the fold at `Unknown`. A field
/// access needs schema resolution the server does not do, and reading past it
/// would invent a type. An `Optional` link is the exception: `$v.?.trim()` reads
/// the same value as `$v.trim()`.
fn path_type(node: Node<'_>, ctx: &TypeCtx<'_>) -> TypeExpr {
    let children = k::named_children(node);
    let Some((base, links)) = children.split_first() else {
        return TypeExpr::Unknown;
    };

    let mut current = infer_expr_type(*base, ctx);
    for link in links {
        if matches!(current, TypeExpr::Unknown) {
            return TypeExpr::Unknown;
        }
        // Optional chaining declines to fail on NONE; it does not change the
        // value's type.
        if k::find_child(*link, k::OPTIONAL).is_some() {
            continue;
        }
        let Some(resolved) = k::find_child(*link, k::IDIOM_FUNCTION)
            .and_then(|idiom| k::find_child(idiom, k::FUNCTION_NAME))
            .and_then(|name| k::text_of(ctx.source, name))
            .and_then(|method| crate::semantic::method::resolve(&current, method))
        else {
            return TypeExpr::Unknown;
        };
        match crate::semantic::method::return_type(resolved.function) {
            Some(returns) => current = returns,
            // The catalogue carries no return types, so this is common. See
            // `method::return_type`.
            None => return TypeExpr::Unknown,
        }
    }

    current
}

// ---------------------------------------------------------------------------
// Arithmetic operands
// ---------------------------------------------------------------------------
//
// The operand rules themselves live in [`crate::semantic::operate`], read from
// the engine. What lives here is the tree work, and it exists because the
// grammar and the engine disagree about shape.
//
// The pinned grammar puts *every* binary operator on one left-associative
// precedence level (`grammar.js`, `precedences` and the `BinaryExpression`
// rule). So it parses `1 + 1 * 3` as `(1 + 1) * 3`, while SurrealDB evaluates
// `1 + (1 * 3)` and answers `4` — its own
// `language/expression/operators/precedence.surql` asserts exactly that. Reading
// the tree as parsed would therefore describe operand pairs the engine never
// formed, which is the wrong-but-plausible diagnostic this module exists to
// avoid.
//
// The fix is three steps: flatten the left spine back into the written operand
// and operator sequence, re-group it with the engine's binding powers, then fold
// the result. Same-operator chains are unaffected; mixed ones become correct.

/// One operator occurrence in a flattened chain.
struct ChainOp {
    /// Whitespace-collapsed source text, so `NOT  IN` cannot masquerade.
    spelling: String,
    power: u8,
}

/// A left-nested run of binary operators, flattened back to source order.
///
/// `operands[i]` is followed by `operators[i]`, so there is always exactly one
/// more operand than operator.
struct Chain<'tree> {
    operands: Vec<Node<'tree>>,
    operators: Vec<ChainOp>,
}

/// The re-grouped shape of a chain. Indices point into [`Chain`].
enum Grouped {
    Leaf(usize),
    Apply {
        operator: usize,
        left: Box<Grouped>,
        right: Box<Grouped>,
    },
}

/// One folded sub-expression: what it is, and where it is.
struct Folded {
    ty: TypeExpr,
    kind: crate::semantic::operate::ValueKind,
    /// Byte span from the leftmost operand to the rightmost. A re-grouped pair
    /// covers a contiguous range even when it matches no single node.
    start: usize,
    end: usize,
}

/// The type of a binary-expression chain, reporting nothing.
fn chain_type(node: Node<'_>, ctx: &TypeCtx<'_>) -> TypeExpr {
    // Diagnostics belong to `check_binary_expressions`, which visits each chain
    // exactly once. Typing is reached from many places — hover, completion,
    // every other check — so reporting from here would duplicate.
    let mut discarded = Vec::new();
    fold_from(node, ctx, &mut discarded)
        .map(|folded| folded.ty)
        .unwrap_or(TypeExpr::Unknown)
}

/// Flatten, re-group, and fold the chain rooted at `node`.
fn fold_from(node: Node<'_>, ctx: &TypeCtx<'_>, out: &mut Vec<Diagnostic>) -> Option<Folded> {
    let chain = flatten_chain(node, ctx.source)?;
    let grouped = regroup(&chain);
    fold_chain(&grouped, &chain, ctx, out)
}

/// Collect the operands and operators of a left-nested chain, in source order.
fn flatten_chain<'tree>(node: Node<'tree>, source: &str) -> Option<Chain<'tree>> {
    let mut chain = Chain {
        operands: Vec::new(),
        operators: Vec::new(),
    };
    push_chain(node, source, &mut chain)?;
    Some(chain)
}

fn push_chain<'tree>(node: Node<'tree>, source: &str, chain: &mut Chain<'tree>) -> Option<()> {
    // `BinaryExpression: seq(_value, Operator, _value)` gives exactly three
    // named children — but `Comment` and `BlockComment` are named in this
    // grammar and sit between them, so `1 /* note */ + 2` has four. Filter
    // first, then insist on the shape; anything else is one we do not model.
    let children: Vec<Node<'tree>> = k::named_children(node)
        .into_iter()
        .filter(|child| !is_trivia(*child))
        .collect();
    let [left, operator, right] = children.as_slice() else {
        return None;
    };
    if operator.kind() != k::OPERATOR {
        return None;
    }

    // Descend the left spine only. `prec.left` means the right operand is never
    // itself a bare chain — a parenthesised group is a `SubQuery`, which ends
    // the spine on its own and is typed by recursion instead.
    if left.kind() == k::BINARY_EXPRESSION {
        push_chain(*left, source, chain)?;
    } else {
        chain.operands.push(*left);
    }

    let spelling = crate::semantic::operate::normalize_operator(k::text_of(source, *operator)?);
    chain.operators.push(ChainOp {
        power: crate::semantic::operate::binding_power(&spelling),
        spelling,
    });
    chain.operands.push(*right);
    Some(())
}

/// Rebuild the chain with the engine's precedence.
fn regroup(chain: &Chain<'_>) -> Grouped {
    let mut index = 0;
    climb(chain, &mut index, 0)
}

/// Precedence climbing over the flat list.
///
/// Every operator the engine lists is left-associative
/// (`syn/parser/expression.rs`), so the right-hand side may only absorb
/// operators that bind *strictly* tighter — hence `power + 1`.
fn climb(chain: &Chain<'_>, index: &mut usize, min_power: u8) -> Grouped {
    let mut left = Grouped::Leaf(*index);
    while let Some(operator) = chain.operators.get(*index) {
        if operator.power < min_power {
            break;
        }
        let at = *index;
        let power = operator.power;
        *index += 1;
        let right = climb(chain, index, power + 1);
        left = Grouped::Apply {
            operator: at,
            left: Box::new(left),
            right: Box::new(right),
        };
    }
    left
}

/// Type a re-grouped chain, recording every operand pair the engine rejects.
///
/// `None` means "not provably anything", and every `None` is a diagnostic that
/// will not fire. Both sides are folded before either is tested, so a failure
/// nested inside an unresolvable expression is still reported.
fn fold_chain(
    grouped: &Grouped,
    chain: &Chain<'_>,
    ctx: &TypeCtx<'_>,
    out: &mut Vec<Diagnostic>,
) -> Option<Folded> {
    use crate::semantic::operate::{ArithOp, arith_result, value_kind};

    match grouped {
        Grouped::Leaf(index) => {
            let node = *chain.operands.get(*index)?;
            let ty = infer_expr_type(node, ctx);
            let kind = value_kind(&ty)?;
            Some(Folded {
                ty,
                kind,
                start: node.start_byte(),
                end: node.end_byte(),
            })
        }
        Grouped::Apply {
            operator,
            left,
            right,
        } => {
            let spelling = &chain.operators.get(*operator)?.spelling;
            let folded_left = fold_chain(left, chain, ctx, out);
            // The right side of `??`, `?:`, `&&` or `||` may never run, so a
            // failure inside it is not provable. Type it, but discard whatever it
            // wanted to report.
            let folded_right = if crate::semantic::operate::short_circuits(spelling) {
                let mut discarded = Vec::new();
                fold_chain(right, chain, ctx, &mut discarded)
            } else {
                fold_chain(right, chain, ctx, out)
            };
            let (left, right) = (folded_left?, folded_right?);

            // Not arithmetic: a comparison, a containment test, a logical
            // operator, `??`, or an assignment form. None of them can fail
            // (`val/mod.rs`, `Value::equal` and the derived `PartialOrd`), and
            // `+=` takes the looser increment path, so there is nothing to
            // report and nothing reliable to type.
            let arith = ArithOp::parse(spelling)?;

            match arith_result(arith, left.kind, right.kind) {
                Some(kind) => Some(Folded {
                    ty: kind.as_type(),
                    kind,
                    start: left.start,
                    end: right.end,
                }),
                None => {
                    if arith.can_fail() {
                        out.push(diagnostic(
                            byte_range_to_lsp(ctx.source, left.start, right.end),
                            codes::OPERATOR_TYPE,
                            arith.failure_message(&left.ty, &right.ty),
                        ));
                    }
                    None
                }
            }
        }
    }
}

/// True when the parser lost track immediately beside this node.
///
/// `contains_parse_error` only looks inside a subtree. A construct the grammar
/// cannot parse at all can leave a well-formed-looking fragment flanked by
/// `ERROR` nodes, and that fragment's operands are an artefact of the failure.
fn has_broken_sibling(node: Node<'_>) -> bool {
    [node.prev_named_sibling(), node.next_named_sibling()]
        .into_iter()
        .flatten()
        .any(|sibling| sibling.is_error() || sibling.is_missing())
}

/// Report every arithmetic operand pair SurrealDB rejects.
fn check_binary_expressions(node: Node<'_>, ctx: &TypeCtx<'_>, out: &mut Vec<Diagnostic>) {
    // Act at the root of a chain only. Every node on the left spine is itself a
    // `BinaryExpression` whose parent is one, and folding from each of them
    // would report the same pair once per level.
    if node.kind() == k::BINARY_EXPRESSION
        && node
            .parent()
            .is_none_or(|parent| parent.kind() != k::BINARY_EXPRESSION)
        // A chain the parser could not read says nothing reliable about its
        // operands, and a syntax diagnostic already covers the position.
        && !contains_parse_error(node)
        // The same, one step out. The pinned grammar cannot parse mock syntax
        // (`|test:1..4|`), and it fails by leaving `ERROR` nodes *beside* a
        // `BinaryExpression` rather than inside it — so `test:..=-9` becomes a
        // record id minus an int, which is neither operand the author wrote.
        && !has_broken_sibling(node)
    {
        fold_from(node, ctx, out);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        check_binary_expressions(child, ctx, out);
    }
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
    check_field_clauses(root, &ctx, &mut diagnostics);
    check_function_returns(root, &ctx, &mut diagnostics);
    check_variables(root, &ctx, settings, &mut diagnostics);
    check_binary_expressions(root, &ctx, &mut diagnostics);
    diagnostics
}

/// `DEFINE FUNCTION … -> T { RETURN … }` — the returned value must satisfy `T`.
///
/// The engine coerces a function's result to its declared return type and fails
/// with `Couldn't coerce return value from function …`
/// (`expr/function.rs:330`), using the same coercion relation
/// [`assignable`] models for arguments. So a declared return type is checkable
/// with what is already here.
///
/// Two things are checked:
///
/// * every `RETURN <expr>` that returns from *this* function, including the ones
///   inside `IF` branches and `FOR` bodies, and
/// * the block's trailing expression, which is a function's result when it ends
///   without a `RETURN`.
///
/// A `RETURN` inside an `IF` really does return from the enclosing function —
/// SurrealDB's own `fn::fib($n: int) -> int` is written that way, and recursion
/// would not terminate otherwise:
///
/// ```surql
/// DEFINE FUNCTION fn::fib($n: int) -> int {
///     IF $n < 2 { RETURN $n; };
///     RETURN fn::fib($n - 1) + fn::fib($n - 2);
/// };
/// ```
///
/// So the walk descends, but only through constructs that propagate a return
/// ([`propagates_return`]). It is an allowlist rather than a blocklist, because
/// the cost of the two directions is not symmetric: descending somewhere it
/// should not reports against a value the function never returns, while failing
/// to descend merely misses one.
fn check_function_returns(node: Node<'_>, ctx: &TypeCtx<'_>, out: &mut Vec<Diagnostic>) {
    if node.kind() == k::DEFINE_STATEMENT
        && crate::semantic::analyzer::define_form(node, ctx.source).as_deref() == Some("function")
    {
        check_one_function_body(node, ctx, out);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        check_function_returns(child, ctx, out);
    }
}

fn check_one_function_body(node: Node<'_>, ctx: &TypeCtx<'_>, out: &mut Vec<Diagnostic>) {
    let children = k::named_children(node);
    // No declared return type means nothing to check against. Note this reads
    // the annotation from the *tree*, not from `FunctionDef.return_type`, so an
    // inferred return type can never be checked against itself.
    let Some(declared) = crate::semantic::analyzer::function_return_type(&children, ctx.source)
    else {
        return;
    };
    let Some(body) = k::find_child(node, k::BLOCK) else {
        return;
    };
    // A body the parser could not read says nothing reliable about what it
    // returns, and a syntax diagnostic already covers it.
    if contains_parse_error(body) {
        return;
    }
    let name = k::find_child(node, k::FUNCTION_NAME)
        .and_then(|node| k::text_of(ctx.source, node))
        .unwrap_or("this function");

    for result in body_results(body) {
        // `RETURN` with no value yields NONE, which every optional type
        // accepts and `assignable` already judges.
        if let BodyResult::Value(value) = result {
            report_return_mismatch(value, &declared, name, ctx, out);
        }
    }
}

/// One thing a function body can hand back.
#[derive(Debug, Clone, Copy)]
enum BodyResult<'tree> {
    /// A node carrying a value: a `RETURN <expr>`, or a trailing expression.
    Value(Node<'tree>),
    /// A `RETURN` whose value we could not identify.
    ///
    /// Unreachable on a clean tree: the grammar is
    /// `ReturnStatement: seq(Keyword, $._expression)` with the expression
    /// **not** optional, so a valueless `RETURN` needs a `MISSING` node — and
    /// both callers reject a subtree with a parse error before they get here.
    /// It therefore means "we failed to read this", not "this yields NONE", and
    /// [`body_return_type`] refuses to infer rather than assume one.
    NoValue,
}

/// Everything a function body can hand back, in source order.
///
/// Two sources: every `RETURN` that returns from *this* function, and the
/// block's trailing expression when the body ends without one.
///
/// [`check_one_function_body`] checks each against a declared type, and
/// [`body_return_type`] joins their types into an inferred one. They share this
/// enumeration on purpose — a node one of them counts as a result and the other
/// does not would mean a value gets checked but not inferred, or worse.
fn body_results<'tree>(body: Node<'tree>) -> Vec<BodyResult<'tree>> {
    let mut returns = Vec::new();
    collect_function_returns(body, &mut returns);
    let mut results: Vec<BodyResult<'tree>> = returns
        .into_iter()
        .map(|statement| match returned_expression(statement) {
            Some(value) => BodyResult::Value(value),
            None => BodyResult::NoValue,
        })
        .collect();

    // A body that ends in a bare expression returns it. `{ 1 }` is
    // `-> int`; `{ '' }` is not.
    //
    // This contribution is deliberately unconditional for every other node
    // kind, including statements whose type we cannot work out, and that is
    // load-bearing rather than lazy. `{ IF $n > 0 { RETURN 'yes'; }; }` returns
    // NONE when the branch does not fire, so unioning only the collected
    // `RETURN`s would infer `string` — narrower than the truth, and the narrow
    // answer is the one that fires a diagnostic. The `Unknown` this yields is
    // what keeps that case honest.
    //
    // `THROW` is the one exception, and a sound one: it always raises, so no
    // value is ever coerced through that path. Without it the common
    // validate-or-throw shape — `{ IF ok { RETURN $x; }; THROW '…'; }` — could
    // never be inferred.
    if let Some(tail) = body_tail(body)
        && !matches!(tail.kind(), k::RETURN_STATEMENT | k::THROW_STATEMENT)
    {
        results.push(BodyResult::Value(tail));
    }

    results
}

/// The node a block's own value comes from: its last named child that is
/// neither trivia nor a brace.
///
/// `BraceOpen`/`BraceClose` are *named* nodes in this grammar and have to be
/// filtered explicitly — see [`is_structural`], which owns that rule. `;` is
/// anonymous, so it never appears here.
fn body_tail<'tree>(body: Node<'tree>) -> Option<Node<'tree>> {
    k::named_children(body)
        .into_iter()
        .rev()
        .find(|child| !is_structural(*child))
}

/// The type a function body produces, or `None` when we are not certain.
///
/// # The poison rule
///
/// One unresolvable path makes the whole answer `None`. That is the only safe
/// direction: an inferred type flows into the argument check exactly as a
/// declared one does, so a type *narrower* than the truth would report against
/// a value the function really can return — a diagnostic on working code, which
/// this module exists to avoid.
///
/// `None` rather than [`TypeExpr::Unknown`] on purpose. The caller stores what
/// this returns, and storing `Unknown` would freeze it: a body that could not be
/// typed in one round because a callee was still unresolved has to stay eligible
/// for the next round. "Absent" must mean "not yet", never "no".
///
/// Paths that disagree are joined with [`TypeExpr::union`] rather than
/// discarded. A union on the value side of [`assignable`] can never come back
/// `Incompatible` — it needs *every* member to fit or it answers `Unknown`
/// (`assign.rs`, the `Union` arm) — so a union stays silent in the checker while
/// still telling hover something true. `union` also folds a `none` member into
/// an `option<…>`, which is inert on the value side for the same reason.
///
/// WARNING: this must never become reachable from [`infer_expr_type`]. The
/// no-cycle argument for the whole feature is that
/// [`infer_function_return_types`] reads already-computed types out of a map, so
/// nothing here can re-enter a function body. Wire this into
/// [`call_return_type`] as a fallback and `fn::fib` without an annotation
/// overflows the stack.
fn body_return_type(body: Node<'_>, ctx: &TypeCtx<'_>) -> Option<TypeExpr> {
    let results = body_results(body);
    // A body with no result at all — `{}` — says nothing.
    if results.is_empty() {
        return None;
    }

    let mut parts: Vec<TypeExpr> = Vec::new();
    for result in results {
        let BodyResult::Value(node) = result else {
            // A `RETURN` we could not read. See `BodyResult::NoValue`.
            return None;
        };
        let ty = infer_expr_type(node, ctx);
        if matches!(ty, TypeExpr::Unknown | TypeExpr::Other(_)) {
            return None;
        }
        // `TypeExpr::union` does not deduplicate, and `string | string` in a
        // hover is a bug report waiting to happen.
        if !parts.contains(&ty) {
            parts.push(ty);
        }
    }

    // `union` of one member is that member, so the common single-`RETURN` body
    // comes back as a plain scalar.
    match TypeExpr::union(parts) {
        TypeExpr::Unknown | TypeExpr::Other(_) => None,
        ty => Some(ty),
    }
}

/// Every `RETURN` that returns from the function whose body is `node`.
///
/// Descends only through [`propagates_return`], so a `RETURN` belonging to some
/// other construct is never collected.
fn collect_function_returns<'tree>(node: Node<'tree>, out: &mut Vec<Node<'tree>>) {
    for child in k::named_children(node) {
        if child.kind() == k::RETURN_STATEMENT {
            out.push(child);
            continue;
        }
        if propagates_return(child.kind()) {
            collect_function_returns(child, out);
        }
    }
}

/// True when a `RETURN` inside this construct returns from the enclosing
/// function rather than from the construct itself.
///
/// An allowlist, and short on purpose. Everything absent from it stops the
/// walk, which is the safe direction:
///
/// * `LET $y = { RETURN 5 }` — the block is a *value*; its `RETURN` sets `$y`.
///   `LetStatement` is absent, so the walk never reaches that block.
/// * `Closure` and a nested `DefineStatement` — a `RETURN` inside either returns
///   from *it*, not from the function around it.
/// * `SubQuery` — an `IF` condition lives in one, and it is not a return path.
fn propagates_return(kind: &str) -> bool {
    matches!(
        kind,
        // A branch body, or a plain nested statement block.
        k::BLOCK
            // `IF … { … } ELSE IF … { … } ELSE { … }`, whose condition and
            // branches are wrapped in a `Modern` node.
            | k::IF_ELSE_STATEMENT
            | k::MODERN
            // `FOR $i IN … { RETURN … }` returns from the function, not the loop.
            | k::FOR_STATEMENT
    )
}

/// The value a `RETURN` carries, if it carries one.
fn returned_expression<'tree>(statement: Node<'tree>) -> Option<Node<'tree>> {
    k::named_children(statement)
        .into_iter()
        .find(|child| !k::is_keyword(*child) && !is_trivia(*child))
}

fn report_return_mismatch(
    value: Node<'_>,
    declared: &TypeExpr,
    name: &str,
    ctx: &TypeCtx<'_>,
    out: &mut Vec<Diagnostic>,
) {
    let actual = infer_expr_type(value, ctx);
    if assignable(&actual, declared) != Verdict::Incompatible {
        // Silent as a whole, but an individual element may still be wrong. See
        // the note in `check_let_annotations` on why this is not nested.
        report_element_faults(
            value,
            declared,
            codes::RETURN_TYPE,
            ctx,
            out,
            &|fault, label| match fault {
                ElementFault::Element { actual, .. } => {
                    format!("`{name}` returns `{declared}`, but {label} is `{actual}`.")
                }
                ElementFault::Arity { expected, actual } => format!(
                    "`{name}` returns `{declared}`, which has {expected} elements, but this \
                     value has {actual}."
                ),
            },
        );
        return;
    }
    out.push(diagnostic(
        node_range(ctx.source, value),
        codes::RETURN_TYPE,
        format!("`{name}` returns `{declared}`, but this value is `{actual}`."),
    ));
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
            // The whole-value verdict wins when it speaks: its message already
            // says the whole truth, so element messages would only repeat it.
            //
            // NOTE this branch is INVERTED against `report_object_faults`, which
            // nests inside an `is_incompatible()` early return. It has to be:
            // `join_types` collapses a mixed literal to `any`, so
            // `array<int> = ["20", 30]` reads as `Compatible` and a refinement
            // nested inside the fault could never fire. Do not "tidy" this back
            // into the object shape — that silently kills every mixed literal
            // and every declared tuple.
            if assignable(&actual, &declared).is_incompatible() {
                out.push(diagnostic(
                    node_range(ctx.source, *value),
                    codes::LET_TYPE,
                    format!("`{name}` is declared `{declared}` but the value is `{actual}`."),
                ));
            } else {
                report_element_faults(
                    *value,
                    &declared,
                    codes::LET_TYPE,
                    ctx,
                    out,
                    &|fault, label| match fault {
                        ElementFault::Element { actual, .. } => {
                            format!("`{name}` is declared `{declared}` but {label} is `{actual}`.")
                        }
                        ElementFault::Arity { expected, actual } => format!(
                            "`{name}` is declared `{declared}`, which has {expected} elements, \
                             but the value has {actual}."
                        ),
                    },
                );
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        check_let_annotations(child, ctx, out);
    }
}

/// `DEFINE FIELD f ON t TYPE T DEFAULT <value>` — the value must satisfy `T`.
///
/// The engine coerces a field's `DEFAULT`, `VALUE` and `COMPUTED` expressions to
/// the declared type and fails with `Couldn't coerce value for field …`. All
/// three were verified against a live engine.
///
/// `ASSERT` is deliberately **not** checked. An `ASSERT` is a predicate over
/// `$value` that must be truthy, not a value coerced to the type, so comparing
/// `ASSERT $value >= 0` against `TYPE option<decimal>` would report a `bool`
/// where a `decimal` is declared. That is a false positive on seven lines of
/// `tests/fixtures/adversarial.surql` alone. `PERMISSIONS` is a predicate too,
/// for the same reason.
fn check_field_clauses(node: Node<'_>, ctx: &TypeCtx<'_>, out: &mut Vec<Diagnostic>) {
    if node.kind() == k::DEFINE_STATEMENT
        && crate::semantic::analyzer::define_form(node, ctx.source).as_deref() == Some("field")
    {
        check_one_field(node, ctx, out);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        check_field_clauses(child, ctx, out);
    }
}

fn check_one_field(node: Node<'_>, ctx: &TypeCtx<'_>, out: &mut Vec<Diagnostic>) {
    // Guarded on the WHOLE statement rather than on the payload: the pinned
    // grammar cannot read `array<number, 2>`, and SurrealDB's own corpus pairs
    // that type with a perfectly clean `DEFAULT`. A payload-scoped guard would
    // miss it and then compare against a half-read type.
    if contains_parse_error(node) {
        return;
    }
    let children = k::named_children(node);
    // Read the declared type from the *tree*, not from `model.fields`: a field
    // can be defined in more than one document, so the merged record may be a
    // different definition — the same reason `check_one_function_body` reads its
    // annotation from the tree.
    let Some(declared) = children
        .iter()
        .find(|child| child.kind() == k::TYPE_CLAUSE)
        .and_then(|clause| k::find_child_any(*clause, k::TYPE_KINDS))
        .map(|payload| TypeExpr::from_node(payload, ctx.source))
    else {
        return;
    };
    let name = children
        .iter()
        .find(|child| matches!(child.kind(), k::IDIOM | k::PATH | k::IDENT))
        .and_then(|child| k::dotted_name(ctx.source, *child))
        .unwrap_or_else(|| "this field".to_string());

    // A block, a subquery, an `IF … ELSE` and a `$value` reference all reach
    // `infer_expr_type`'s fallback and answer `Unknown`, which is silent. That
    // silence is load bearing: `$value` is a special variable outside the binding
    // table, so if inference ever learns to type it as the field type, every
    // `VALUE` clause becomes self-referential and this pass needs revisiting.
    for clause in children.iter().filter(|child| {
        matches!(
            child.kind(),
            k::DEFAULT_CLAUSE | k::VALUE_CLAUSE | k::COMPUTED_CLAUSE
        )
    }) {
        let Some(payload) = clause_payload(*clause) else {
            continue;
        };
        let actual = infer_expr_type(payload, ctx);
        // Same shape as `check_let_annotations`: the whole-value verdict wins
        // when it speaks, and the element walk runs only when it stays silent.
        if assignable(&actual, &declared).is_incompatible() {
            out.push(diagnostic(
                node_range(ctx.source, payload),
                codes::FIELD_TYPE,
                format!("`{name}` is declared `{declared}` but this value is `{actual}`."),
            ));
        } else {
            report_element_faults(
                payload,
                &declared,
                codes::FIELD_TYPE,
                ctx,
                out,
                &|fault, label| match fault {
                    ElementFault::Element { actual, .. } => {
                        format!("`{name}` is declared `{declared}` but {label} is `{actual}`.")
                    }
                    ElementFault::Arity { expected, actual } => format!(
                        "`{name}` is declared `{declared}`, which has {expected} elements, but \
                         this value has {actual}."
                    ),
                },
            );
        }
    }
}

/// The value a `DEFAULT` / `VALUE` / `COMPUTED` clause carries.
///
/// `DefaultClause: seq(Keyword[DEFAULT], optional(DefaultAlways), _value)`, and
/// `_value` is hidden, so the payload is a direct child. `DefaultAlways` has to
/// be filtered **by kind**: it is not a `Keyword` node, so `is_keyword` lets it
/// through and a naive "first non-keyword child" returns the `ALWAYS` marker
/// instead of the value.
fn clause_payload<'tree>(clause: Node<'tree>) -> Option<Node<'tree>> {
    k::named_children(clause).into_iter().find(|child| {
        !k::is_keyword(*child) && child.kind() != k::DEFAULT_ALWAYS && !is_structural(*child)
    })
}

fn check_calls(node: Node<'_>, ctx: &TypeCtx<'_>, out: &mut Vec<Diagnostic>) {
    if node.kind() == k::FUNCTION_CALL {
        check_one_call(node, ctx, out);
    }
    if node.kind() == k::IDIOM_FUNCTION {
        check_method_call(node, ctx, out);
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
    // `MIDDLEWARE fn::x()` registers a function; it does not call it. The API
    // runtime invokes it with `(request, next)` supplied, so the written
    // argument list is always shorter than the declared parameter list.
    // SurrealDB's own `language-tests/tests/api/` corpus has 33 of these, every
    // one of which was reported as a wrong argument count.
    if node
        .parent()
        .is_some_and(|parent| parent.kind() == k::MIDDLEWARE_CLAUSE)
    {
        return;
    }
    // A name SurrealDB has renamed. The engine records the replacement itself
    // and still accepts the old spelling, so this is a warning with a quick fix
    // (`code_action`), not an error.
    if let Some(current) = renamed_builtin(name)
        && let Some(name_node) = k::find_child(node, k::FUNCTION_NAME)
    {
        out.push(warning(
            node_range(ctx.source, name_node),
            codes::RENAMED_FUNCTION,
            format!("`{name}` has been renamed to `{current}`."),
        ));
    }

    let Some(arg_list) = k::find_child(node, k::ARGUMENT_LIST) else {
        return;
    };
    // An argument the parser could not read makes the count meaningless: the
    // `ERROR` node might stand for one argument or five. The pinned grammar
    // cannot parse a closure (`|| 'x'`) or a signed decimal suffix
    // (`-1.5dec`), and both are valid SurrealQL — so counting the error node
    // reported a wrong arity on code the engine accepts. A syntax diagnostic
    // already covers the position; do not pile an invented one on top.
    if contains_parse_error(arg_list) {
        return;
    }
    let args = argument_nodes(arg_list);

    // A `DEFINE FUNCTION` shadows nothing — `fn::` is a separate namespace from
    // the builtins — so the two paths are exclusive rather than ordered.
    match ctx.model.functions.get(name) {
        Some(function) => check_user_call(name, function, node, arg_list, &args, ctx, out),
        None => check_builtin_call(name, arg_list, &args, ctx, out),
    }
}

/// Method-call syntax: `'abc'.len()`, `[1, 2].at(0)`.
///
/// The receiver is the function's first argument, so `'abc'.len()` is
/// `string::len('abc')` with nothing written. That makes the check the same one
/// `check_builtin_call` runs, shifted by one position.
///
/// Deliberately narrow, in three ways, because the engine's mapping from method
/// to function is a table this does not have:
///
/// * The canonical name is assumed to be `<receiver type>::<method>`. That holds
///   for `string::`, `array::`, `set::`, `object::` and `duration::`, and fails
///   for the remapped ones — `<number>.round()` is `math::round`, not
///   `number::round`. A name that is not in the catalogue is simply not checked,
///   so those stay silent rather than wrong. Generating the engine's 11 receiver
///   tables would widen coverage; nothing here has to change when it does.
/// * Only the first link of a path is a known receiver. In `'abc'.foo.trim()`
///   the receiver of `trim` is `'abc'.foo`, whose type is unknown — reading the
///   type of `'abc'` instead would invent one.
/// * The receiver type must be concrete. Anything else is silent, as everywhere.
fn check_method_call(node: Node<'_>, ctx: &TypeCtx<'_>, out: &mut Vec<Diagnostic>) {
    let Some(method) =
        k::find_child(node, k::FUNCTION_NAME).and_then(|name| k::text_of(ctx.source, name))
    else {
        return;
    };
    let Some(arg_list) = k::find_child(node, k::ARGUMENT_LIST) else {
        return;
    };
    if contains_parse_error(arg_list) {
        return;
    }
    let Some(receiver) = method_receiver(node) else {
        return;
    };

    // The gate. `None` means we cannot tell which of the engine's twelve tables
    // applies, and picking the wrong one is worse than picking none: `String`
    // and the catch-all disagree about the arity of four method names.
    let receiver_type = infer_expr_type(receiver, ctx);
    let Some(kind) = crate::semantic::method::receiver_kind(&receiver_type) else {
        return;
    };

    let Some(resolved) = crate::semantic::method::resolve(&receiver_type, method) else {
        // The receiver is known, so the engine would normally refuse this with
        // `no such method found for the <kind> type` — and this is the one place
        // the old convention-guessing could not tell a typo from a remapped name.
        //
        // An object is the exception, and it is not a small one. When method
        // dispatch fails on an object the engine retries the name as a
        // *closure-valued field* (`val/value/get.rs`, the `fallback_function!`
        // arm), so `{ a: |$x| $x }.a(1)` is legal and the field can be called
        // anything. Three files in SurrealDB's own corpus do exactly that. Since
        // the server does not track which fields hold closures, an object method
        // it cannot find is unprovable rather than wrong.
        if kind != "Object" {
            out.push(diagnostic(
                node_range(ctx.source, node),
                codes::UNKNOWN_METHOD,
                format!(
                    "`{}` has no method `{method}`.",
                    crate::semantic::method::kind_label(kind)
                ),
            ));
        }
        return;
    };

    let Some(signature) = builtin_signature(resolved.function) else {
        return;
    };
    // Deliberately *not* checking `not_callable` here. `value::chain` has no
    // callable function form — `value::chain(x, f)` parses and then fails — but
    // `x.chain(f)` is exactly how the engine expects it to be written.
    if !signature.generated.signature_known {
        return;
    }
    // The receiver fills parameter one, so a function with no declared
    // parameters cannot be what this method resolves to.
    if signature.generated.params.is_empty() {
        return;
    }

    let args = argument_nodes(arg_list);
    if args.is_empty() {
        // The same transient-typing state `check_builtin_call` refuses to report:
        // an editor that closes brackets writes `.at()` on the `(` keystroke.
        return;
    }

    let required = signature.required_arity().saturating_sub(1);
    let maximum = signature.maximum_arity().map(|max| max.saturating_sub(1));
    if args.len() < required || maximum.is_some_and(|max| args.len() > max) {
        out.push(diagnostic(
            node_range(ctx.source, arg_list),
            codes::ARGUMENT_COUNT,
            format!(
                "`.{method}()` (`{}`) expects {}, found {}.",
                resolved.function,
                expected_arity_label(required, maximum),
                args.len()
            ),
        ));
        return;
    }

    for (index, argument) in args.iter().enumerate() {
        // Shifted by one: the receiver already supplied parameter zero.
        let Some(expected) = signature.param_type_at(index + 1) else {
            continue;
        };
        let actual = infer_expr_type(*argument, ctx);
        if assignable(&actual, expected) != Verdict::Incompatible {
            // Silent as a whole, but an element may still be wrong. See the note
            // in `check_let_annotations` on why this is not nested inside the
            // fault.
            let callee = format!("`.{method}()` (`{}`)", resolved.function);
            report_element_faults(
                *argument,
                expected,
                codes::ARGUMENT_TYPE,
                ctx,
                out,
                &argument_element_message(index + 2, &callee),
            );
            continue;
        }
        out.push(diagnostic(
            node_range(ctx.source, *argument),
            codes::ARGUMENT_TYPE,
            format!(
                // Numbered as the engine numbers it: the receiver is argument
                // one, so the first written argument is argument two. Matching
                // that means the server and a runtime error agree. The resolved
                // function is named so a reader can find its documentation.
                "Argument {} of `.{method}()` (`{}`) expects `{expected}`, found `{actual}`.",
                index + 2,
                resolved.function
            ),
        ));
    }
}

/// The value a method is called on, when the method is the first *meaningful*
/// link of the path.
///
/// Walks up `IdiomFunction` → `Subscript` → `Path`, then insists that nothing
/// between the receiver and the method changes what the receiver is.
///
/// An `Optional` link is the one thing that does not: `$v.?.trim()` reads the
/// same value as `$v.trim()`, it only declines to fail when `$v` is NONE. That
/// shape appears six times across the two test fixtures, and skipping it is what
/// makes them resolvable at all.
///
/// Every other intervening link *does* change the receiver. In `$a.b.trim()` the
/// receiver is `$a.b`, whose type needs field resolution this server does not
/// have — reading `$a` instead would invent one.
pub(crate) fn method_receiver<'tree>(idiom: Node<'tree>) -> Option<Node<'tree>> {
    let subscript = idiom.parent()?;
    let path = subscript.parent()?;
    let children = k::named_children(path);

    // Find where this method sits, then require every link before it to be an
    // `Optional`.
    let position = children
        .iter()
        .position(|child| child.id() == subscript.id())?;
    if position == 0 {
        return None;
    }
    let intervening = &children[1..position];
    if !intervening
        .iter()
        .all(|link| k::find_child(*link, k::OPTIONAL).is_some())
    {
        return None;
    }

    children.first().copied()
}

/// Argument count for a builtin, against the generated catalogue.
///
/// SurrealDB rejects a wrong count outright (`fnc/args.rs:195-227`), so this
/// cannot fire on a query that runs.
fn check_builtin_call(
    name: &str,
    arg_list: Node<'_>,
    args: &[Node<'_>],
    ctx: &TypeCtx<'_>,
    out: &mut Vec<Diagnostic>,
) {
    let Some(signature) = builtin_signature(name) else {
        return;
    };
    // The parser accepts the name, but nothing implements it in call form, so
    // the query parses and then fails at run time. Nine names today, among them
    // `duration::set_day` and `object::matches`.
    if signature.generated.not_callable {
        out.push(warning(
            node_range(ctx.source, arg_list),
            codes::NOT_CALLABLE,
            format!(
                "`{name}` parses, but SurrealDB has no implementation to call. \
                 The query will fail at run time."
            ),
        ));
        return;
    }
    // The generator could not read this implementation, so an empty parameter
    // list means "unknown", not "takes nothing".
    if !signature.generated.signature_known {
        return;
    }
    // `ArgumentList` is `seq('(', optional(…), ')')`, so `string::len()` is a
    // syntactically complete zero-argument call. Every editor that closes
    // brackets produces exactly that on the `(` keystroke, and this server has
    // no debounce — reporting it would squiggle every call while it is typed.
    if args.is_empty() {
        return;
    }

    let required = signature.required_arity();
    let maximum = signature.maximum_arity();
    let too_few = args.len() < required;
    let too_many = maximum.is_some_and(|max| args.len() > max);
    if too_few || too_many {
        out.push(diagnostic(
            node_range(ctx.source, arg_list),
            codes::ARGUMENT_COUNT,
            format!(
                "`{name}` expects {}, found {}.",
                expected_arity_label(required, maximum),
                args.len()
            ),
        ));
        // Comparing positions is meaningless once the count is wrong — the same
        // reason the `fn::` path stops here.
        return;
    }

    for (index, argument) in args.iter().enumerate() {
        let Some(expected) = signature.param_type_at(index) else {
            continue;
        };
        let actual = infer_expr_type(*argument, ctx);
        if assignable(&actual, expected) != Verdict::Incompatible {
            // Silent as a whole, but an element may still be wrong. See the note
            // in `check_let_annotations` on why this is not nested inside the
            // fault.
            let callee = format!("`{name}`");
            report_element_faults(
                *argument,
                expected,
                codes::ARGUMENT_TYPE,
                ctx,
                out,
                &argument_element_message(index + 1, &callee),
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

/// The engine's own wording for an argument count (`fnc/args.rs:199-221`).
fn expected_arity_label(required: usize, maximum: Option<usize>) -> String {
    match maximum {
        None if required == 0 => "zero or more arguments".to_string(),
        None => format!("{required} or more arguments"),
        Some(0) => "no arguments".to_string(),
        Some(max) if max == required && max == 1 => "1 argument".to_string(),
        Some(max) if max == required => format!("{max} arguments"),
        Some(max) => format!("{required} to {max} arguments"),
    }
}

fn check_user_call(
    name: &str,
    function: &crate::semantic::types::FunctionDef,
    _node: Node<'_>,
    arg_list: Node<'_>,
    args: &[Node<'_>],
    ctx: &TypeCtx<'_>,
    out: &mut Vec<Diagnostic>,
) {
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
            // Silent as a whole, but an element may still be wrong. This is the
            // element counterpart of the object drill-down below, and it sits on
            // the other side of the verdict — see the note in
            // `check_let_annotations`. The two never collide: a collection
            // literal is never an `Object` node.
            let callee = format!("`{name}`");
            report_element_faults(
                *argument,
                expected,
                codes::ARGUMENT_TYPE,
                ctx,
                out,
                &argument_element_message(index + 1, &callee),
            );
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

/// The message for an element fault inside call argument `position`.
///
/// `callee` arrives already wrapped in backticks, because a method call names
/// both the method and the builtin it resolved to. The wording mirrors the
/// per-property form in [`report_object_faults`], so the two drill-downs read
/// alike.
fn argument_element_message<'a>(
    position: usize,
    callee: &'a str,
) -> impl Fn(&ElementFault, &str) -> String + 'a {
    move |fault, label| match fault {
        ElementFault::Element {
            expected, actual, ..
        } => format!(
            "Argument {position} of {callee}: {label} expects `{expected}`, found `{actual}`."
        ),
        ElementFault::Arity { expected, actual } => {
            format!("Argument {position} of {callee} expects {expected} elements, found {actual}.")
        }
    }
}

/// Peel `option<…>` so a shape test sees the collection underneath.
fn without_option(ty: &TypeExpr) -> &TypeExpr {
    match ty {
        TypeExpr::Option(inner) => without_option(inner),
        other => other,
    }
}
/// The elements of a collection literal to check, or `None` when the walk must
/// not run.
///
/// Three guards, and every one of them keeps a false positive out:
///
/// * **A literal only.** A variable, a call, a subquery or a cast has no element
///   nodes to point at and no per-element types the joined type did not already
///   carry. This is what keeps `VALUE array::distinct($value OR [])` silent.
/// * **The container shapes must agree.** A `Set` literal against `array<T>` is a
///   *shape* fault that [`assignable`] already reports; walking it would add a
///   second, worse message for the same mistake.
/// * **No parse error.** A subtree the parser could not read says nothing about
///   what the author wrote, and a `parse` diagnostic already covers it. Load
///   bearing: the pinned grammar cannot read a unary minus, so
///   `[$w, -$h]` is an `ERROR` subtree whose fragments must not be walked.
fn element_nodes<'tree>(literal: Node<'tree>, expected: &TypeExpr) -> Option<Vec<Node<'tree>>> {
    let shapes_agree = matches!(
        (literal.kind(), without_option(expected)),
        (k::ARRAY, TypeExpr::Array(_) | TypeExpr::Tuple(_)) | (k::SET, TypeExpr::Set(_))
    );
    if !shapes_agree || contains_parse_error(literal) {
        return None;
    }
    Some(literal_elements(literal))
}

/// How to name the faulty element in a message.
///
/// A set carries **no index**. The engine sorts and deduplicates a set before it
/// coerces, so `{ "20", "30" }` fails at index 0 while `{ "20", 30 }` fails at
/// index 1 — both verified against a live engine. A source-order index would
/// therefore contradict the runtime message.
fn element_label(literal: Node<'_>, index: usize) -> String {
    if literal.kind() == k::SET {
        "this element".to_string()
    } else {
        format!("element {index}")
    }
}

/// Walk the elements of `value` against `declared` and report each fault.
///
/// Callers reach this only when the **whole-value** verdict stayed silent, so at
/// most one of the two ever speaks for a given value.
fn report_element_faults(
    value: Node<'_>,
    declared: &TypeExpr,
    code: &str,
    ctx: &TypeCtx<'_>,
    out: &mut Vec<Diagnostic>,
    message: &dyn Fn(&ElementFault, &str) -> String,
) {
    let Some(elements) = element_nodes(value, declared) else {
        return;
    };
    // One enumeration, shared: the nodes a fault points at are exactly the nodes
    // whose types were inferred. Two would let an element be checked but not
    // inferred, or the reverse.
    let types: Vec<TypeExpr> = elements
        .iter()
        .map(|element| infer_expr_type(*element, ctx))
        .collect();

    for fault in element_faults(&types, declared) {
        let (range, label) = match &fault {
            ElementFault::Element { index, .. } => (
                elements
                    .get(*index)
                    .map(|node| node_range(ctx.source, *node))
                    .unwrap_or_else(|| node_range(ctx.source, value)),
                element_label(value, *index),
            ),
            // A wrong length is about the whole literal, not one element.
            ElementFault::Arity { .. } => (node_range(ctx.source, value), String::new()),
        };
        out.push(diagnostic(range, code, message(&fault, &label)));
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

/// A diagnostic for something the engine still accepts.
fn warning(range: Range, code: &str, message: String) -> Diagnostic {
    Diagnostic {
        severity: Some(DiagnosticSeverity::WARNING),
        ..diagnostic(range, code, message)
    }
}
