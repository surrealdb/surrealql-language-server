//! Tree-sitter node-kind constants and small helpers.
//!
//! The grammar lives at `../surrealql-tree-sitter` and mirrors
//! `@surrealdb/lezer`, which emits PascalCase rule names such as
//! `SelectStatement`, `Ident`, and `FunctionCall`. The constants below
//! cover every kind the analyzer and highlighter dispatch on.
//!
//! **Every constant here is verified present in
//! `surrealql-tree-sitter/src/node-types.json` at the revision CI pins.**
//! That invariant matters: a constant naming a kind the grammar never
//! emits compiles fine and silently disables whatever code matches on it.
//! Several extraction bugs (function parameter types, function return
//! types, SELECT target resolution) survived three releases exactly that
//! way. When adding one, check `node-types.json` first.

use tree_sitter::Node;

// ---- Top-level / transparent ---------------------------------------------

/// Root node emitted by the grammar.
pub const SOURCE_FILE: &str = "SurrealQL";

// `_value`, `_baseValue`, `_computedValue` and `_subqueryStatement` are
// *hidden* rules: their children appear inline under the parent, so there
// is no wrapper node kind to match on. The analyzer descends through
// whatever the parent exposes via `named_children()`.
pub const PREDICATE: &str = "Predicate";
pub const SUB_QUERY: &str = "SubQuery";
pub const BLOCK: &str = "Block";
/// The braces of a [`BLOCK`]. Named nodes, so a walk over a block's children
/// meets them alongside the statements.
pub const BRACE_OPEN: &str = "BraceOpen";
pub const BRACE_CLOSE: &str = "BraceClose";

// ---- Statements ----------------------------------------------------------

/// The grammar emits a single `DefineStatement` for every DEFINE form;
/// discriminate with [`crate::semantic::analyzer::define_form`].
pub const DEFINE_STATEMENT: &str = "DefineStatement";

pub const SELECT_STATEMENT: &str = "SelectStatement";
pub const CREATE_STATEMENT: &str = "CreateStatement";
pub const UPDATE_STATEMENT: &str = "UpdateStatement";
pub const UPSERT_STATEMENT: &str = "UpsertStatement";
pub const DELETE_STATEMENT: &str = "DeleteStatement";
pub const RELATE_STATEMENT: &str = "RelateStatement";
pub const INSERT_STATEMENT: &str = "InsertStatement";
pub const LET_STATEMENT: &str = "LetStatement";
pub const FOR_STATEMENT: &str = "ForStatement";
pub const IF_ELSE_STATEMENT: &str = "IfElseStatement";
/// The brace form of `IF`: wraps the condition and every branch block of an
/// [`IF_ELSE_STATEMENT`], so a walk looking for the branches passes through it.
pub const MODERN: &str = "Modern";
pub const RETURN_STATEMENT: &str = "ReturnStatement";
pub const THROW_STATEMENT: &str = "ThrowStatement";

// ---- Clauses -------------------------------------------------------------

pub const ON_TABLE_CLAUSE: &str = "OnTableClause";
pub const TYPE_CLAUSE: &str = "TypeClause";
pub const COMMENT_CLAUSE: &str = "CommentClause";
pub const CONTENT_CLAUSE: &str = "ContentClause";
pub const SET_CLAUSE: &str = "SetClause";
pub const RETURN_CLAUSE: &str = "ReturnClause";
pub const WHERE_CLAUSE: &str = "WhereClause";
pub const GROUP_CLAUSE: &str = "GroupClause";
pub const OMIT_CLAUSE: &str = "OmitClause";
pub const ORDER_CLAUSE: &str = "OrderClause";
pub const SPLIT_CLAUSE: &str = "SplitClause";
pub const FETCH_CLAUSE: &str = "FetchClause";
pub const TIMEOUT_CLAUSE: &str = "TimeoutClause";
pub const PARALLEL_CLAUSE: &str = "ParallelClause";
pub const LIMIT_START_COMBO_CLAUSE: &str = "LimitStartComboClause";
pub const WITH_CLAUSE: &str = "WithClause";
pub const EXPLAIN_CLAUSE: &str = "ExplainClause";
pub const VERSION_CLAUSE: &str = "VersionClause";
pub const TEMPFILES_CLAUSE: &str = "TempfilesClause";

pub const WHEN_CLAUSE: &str = "WhenClause";
pub const THEN_CLAUSE: &str = "ThenClause";

pub const FIELDS_COLUMNS_CLAUSE: &str = "FieldsColumnsClause";
pub const UNIQUE_CLAUSE: &str = "UniqueClause";

pub const PERMISSIONS_FOR_CLAUSE: &str = "PermissionsForClause";
pub const PERMISSIONS_BASIC_CLAUSE: &str = "PermissionsBasicClause";

// ---- Values and expressions ----------------------------------------------

pub const BINARY_EXPRESSION: &str = "BinaryExpression";
pub const PREFIX_EXPRESSION: &str = "PrefixExpression";
pub const PATH: &str = "Path";
pub const SUBSCRIPT: &str = "Subscript";
pub const ARGUMENT_LIST: &str = "ArgumentList";
/// `DEFINE CONFIG API MIDDLEWARE fn::x()` — the parent of a function *reference*
/// rather than a call. The API runtime supplies the arguments.
pub const MIDDLEWARE_CLAUSE: &str = "MiddlewareClause";
/// `DEFINE ACCESS …` wraps its own keyword and name in this node instead of
/// leaving them as direct children of the `DefineStatement`, which is why
/// [`crate::semantic::analyzer`] has to look inside it to name the form.
pub const ACCESS_DEFINITION: &str = "AccessDefinition";
/// `DEFINE SCOPE …`, the pre-3.x spelling of `DEFINE ACCESS`. Wraps identically.
pub const SCOPE_DEFINITION: &str = "ScopeDefinition";
pub const FUNCTION_CALL: &str = "FunctionCall";
/// Method-call form of a function: `$text.trim()`, `$list.len()`.
pub const IDIOM_FUNCTION: &str = "IdiomFunction";
pub const SCRIPTING_FUNCTION: &str = "FunctionJs";
pub const JS_FUNCTION_BODY: &str = "JavaScriptBlock";
pub const CLOSURE: &str = "Closure";
pub const RANGE: &str = "Range";

pub const PARAM_DEFINITION: &str = "ParamDefinition";

pub const FIELD_ASSIGNMENT: &str = "FieldAssignment";
pub const OBJECT: &str = "Object";
pub const OBJECT_CONTENT: &str = "ObjectContent";
pub const OBJECT_PROPERTY: &str = "ObjectProperty";
pub const OBJECT_KEY: &str = "ObjectKey";
pub const KEY_NAME: &str = "KeyName";
pub const ARRAY: &str = "Array";
pub const SET: &str = "Set";
pub const POINT: &str = "Point";

pub const RECORD_ID: &str = "RecordId";
/// `person:1..5` — a range *of* record ids. Note the grammar also has a
/// distinct `RecordIdRange` (below) for the range part inside one.
pub const RANGE_RECORD_ID: &str = "RangeRecordId";
pub const RECORD_ID_RANGE: &str = "RecordIdRange";
pub const RECORD_TB_IDENT: &str = "RecordTbIdent";
pub const RECORD_ID_IDENT: &str = "RecordIdIdent";
pub const RECORD_ID_STRING: &str = "RecordIdString";

pub const IDENT: &str = "Ident";
pub const IDIOM: &str = "Idiom";
/// The grammar emits one `FunctionName` kind for builtin (`string::trim`)
/// and custom (`fn::foo`) calls alike; the only discriminator is the
/// `fn::` text prefix.
pub const CUSTOM_FUNCTION_NAME: &str = "FunctionName";
pub const BUILTIN_FUNCTION_NAME: &str = "FunctionName";
pub const FUNCTION_NAME: &str = "FunctionName";
pub const VARIABLE_NAME: &str = "VariableName";
pub const FUNCTION_JS: &str = "FunctionJs";
pub const FIELDS: &str = "Fields";
/// `*` (and `?` in a filter position).
pub const ANY: &str = "Any";
/// `?` in an optional-chaining path element (`$value.?.trim()`).
pub const OPTIONAL: &str = "Optional";
/// `AFTER` / `BEFORE` / `DIFF` / `FULL` in a RETURN clause.
pub const LITERAL: &str = "Literal";

pub const NUMBER: &str = "Number";
pub const INT: &str = "Int";
pub const FLOAT: &str = "Float";
pub const DECIMAL: &str = "Decimal";
/// Covers plain and prefixed strings alike — the grammar folds
/// `r'…' u'…' d'…' b'…' f'…'` into `String`.
pub const STRING: &str = "String";
pub const FORMAT_STRING: &str = "FormatString";
pub const REGEX: &str = "Regex";
pub const DURATION: &str = "Duration";
pub const BOOL: &str = "Bool";
/// `NONE` and `NULL` share one node kind.
pub const NONE: &str = "None";

// ---- Types ----------------------------------------------------------------

pub const TYPE: &str = "Type";
pub const TYPE_NAME: &str = "TypeName";
pub const PARAMETERIZED_TYPE: &str = "ParameterizedType";
pub const UNION_TYPE: &str = "UnionType";
pub const LITERAL_TYPE: &str = "LiteralType";
pub const ARRAY_TYPE: &str = "ArrayType";
pub const OBJECT_TYPE: &str = "ObjectType";
pub const OBJECT_TYPE_CONTENT: &str = "ObjectTypeContent";
pub const OBJECT_TYPE_PROPERTY: &str = "ObjectTypeProperty";
pub const TYPE_CAST: &str = "TypeCast";

/// Every node kind that can carry a type expression payload. Used
/// wherever a clause is followed by "some type".
pub const TYPE_KINDS: &[&str] = &[
    TYPE,
    TYPE_NAME,
    PARAMETERIZED_TYPE,
    UNION_TYPE,
    LITERAL_TYPE,
    ARRAY_TYPE,
    OBJECT_TYPE,
];

// ---- Punctuation / operators / comments ----------------------------------

pub const COLON: &str = "Colon";
pub const PIPE: &str = "Pipe";
/// `->`, used both for graph traversal and for a function's return arrow.
pub const LOOKUP_RIGHT: &str = "LookupRight";
pub const OPERATOR: &str = "Operator";
pub const COMMENT: &str = "Comment";
pub const BLOCK_COMMENT: &str = "BlockComment";

// ---- Helpers --------------------------------------------------------------

/// Returns the source text covered by `node`, trimmed.
pub fn text_of<'a>(source: &'a str, node: Node<'_>) -> Option<&'a str> {
    node.utf8_text(source.as_bytes()).ok().map(str::trim)
}

/// True when `node` is a keyword node. The local grammar emits a generic
/// `Keyword` node; older/newer grammar variants may emit `keyword_*`.
pub fn is_keyword(node: Node<'_>) -> bool {
    node.kind() == "Keyword" || node.kind().starts_with("keyword_")
}

/// True when `node` is a keyword whose source text matches `expected`
/// (case-insensitive). Works across every dedicated `keyword_*` kind.
pub fn is_kw(node: Node<'_>, source: &str, expected: &str) -> bool {
    is_keyword(node)
        && text_of(source, node).is_some_and(|text| text.eq_ignore_ascii_case(expected))
}

/// True when `kind` is the grammar's DEFINE statement node.
pub fn is_define_statement(kind: &str) -> bool {
    kind == DEFINE_STATEMENT
}

/// First child node with a matching kind (named children only).
pub fn find_child<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| c.kind() == kind)
}

/// First child node whose kind matches any of `kinds` (named children only).
pub fn find_child_any<'tree>(node: Node<'tree>, kinds: &[&str]) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| kinds.contains(&c.kind()))
}

/// Iterator-friendly collection of all named children.
pub fn named_children<'tree>(node: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// Returns true when any descendant of `node` (inclusive) has the given
/// kind. Useful for "does this block contain a `scripting_function`?"
/// checks.
pub fn has_descendant(node: Node<'_>, kind: &str) -> bool {
    if node.kind() == kind {
        return true;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if has_descendant(child, kind) {
            return true;
        }
    }
    false
}

/// Extract a dotted name (e.g. `address.city`) from a value/predicate/path
/// subtree by collecting every `identifier` leaf in source order, joined
/// with `.`. Simple field names return the single identifier; compound
/// idioms parsed as `path(base_value(identifier), path_element(subscript(
/// identifier)))` become `a.b`. Returns `None` when no identifier is found
/// (e.g. a `*` projection).
pub fn dotted_name(source: &str, node: Node<'_>) -> Option<String> {
    let mut parts = Vec::new();
    collect_identifier_parts(source, node, &mut parts);
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}

fn collect_identifier_parts(source: &str, node: Node<'_>, parts: &mut Vec<String>) {
    if matches!(node.kind(), IDENT | IDIOM)
        && let Some(text) = text_of(source, node)
    {
        parts.push(text.to_string());
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_identifier_parts(source, child, parts);
    }
}
