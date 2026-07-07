//! Tree-sitter node-kind constants and small helpers.
//!
//! The grammar lives at `../surrealql-tree-sitter` and currently mirrors
//! `@surrealdb/lezer`, which emits PascalCase rule names such as
//! `SelectStatement`, `Ident`, and `FunctionCall`. The constants below
//! cover every kind the analyzer and highlighter dispatch on. Keep them in
//! sync with `surrealql-tree-sitter/src/node-types.json`.

use tree_sitter::Node;

// ---- Top-level / transparent ---------------------------------------------

/// Root node emitted by the grammar.
pub const SOURCE_FILE: &str = "SurrealQL";

// Transparent wrapper rules that merely nest statements/values:
// `expressions`, `expression`, `subquery_statement`, `value`,
// `base_value`, `predicate`, `inclusive_predicate`. The analyzer descends
// through them via `named_children()`.
pub const VALUE: &str = "Value";
pub const BASE_VALUE: &str = "BaseValue";
pub const PREDICATE: &str = "Predicate";
pub const INCLUSIVE_PREDICATE: &str = "InclusivePredicate";
pub const SUB_QUERY: &str = "SubQuery";
pub const BLOCK: &str = "Block";

// ---- Define statement family (v3: one kind per DEFINE form) ---------------

pub const DEFINE_STATEMENT: &str = "DefineStatement";
pub const DEFINE_TABLE_STATEMENT: &str = "DefineTableStatement";
pub const DEFINE_FIELD_STATEMENT: &str = "DefineFieldStatement";
pub const DEFINE_EVENT_STATEMENT: &str = "DefineEventStatement";
pub const DEFINE_FUNCTION_STATEMENT: &str = "DefineFunctionStatement";
pub const DEFINE_INDEX_STATEMENT: &str = "DefineIndexStatement";
pub const DEFINE_PARAM_STATEMENT: &str = "DefineParamStatement";
pub const DEFINE_ACCESS_STATEMENT: &str = "DefineAccessStatement";
pub const DEFINE_SCOPE_STATEMENT: &str = "DefineScopeStatement";

// ---- CRUD statements -----------------------------------------------------

pub const SELECT_STATEMENT: &str = "SelectStatement";
pub const CREATE_STATEMENT: &str = "CreateStatement";
pub const UPDATE_STATEMENT: &str = "UpdateStatement";
pub const UPSERT_STATEMENT: &str = "UpsertStatement";
pub const DELETE_STATEMENT: &str = "DeleteStatement";
pub const RELATE_STATEMENT: &str = "RelateStatement";
pub const INSERT_STATEMENT: &str = "InsertStatement";
pub const LET_STATEMENT: &str = "LetStatement";

// ---- Clauses -------------------------------------------------------------

pub const ON_TABLE_CLAUSE: &str = "OnTableClause";
pub const TYPE_CLAUSE: &str = "TypeClause";
pub const COMMENT_CLAUSE: &str = "CommentClause";
pub const CONTENT_CLAUSE: &str = "ContentClause";
pub const SET_CLAUSE: &str = "SetClause";
pub const RETURN_CLAUSE: &str = "ReturnClause";
pub const RETURNS_CLAUSE: &str = "ReturnsClause";
pub const WHERE_CLAUSE: &str = "WhereClause";
pub const FROM_CLAUSE: &str = "FromClause";
pub const SELECT_CLAUSE: &str = "SelectClause";
pub const GROUP_CLAUSE: &str = "GroupClause";

pub const WHEN_CLAUSE: &str = "WhenClause";
pub const THEN_CLAUSE: &str = "ThenClause";
pub const WHEN_THEN_CLAUSE: &str = "WhenThenClause";

pub const FIELDS_COLUMNS_CLAUSE: &str = "FieldsColumnsClause";
pub const UNIQUE_CLAUSE: &str = "UniqueClause";

pub const PERMISSIONS_FOR_CLAUSE: &str = "PermissionsForClause";
pub const PERMISSIONS_BASIC_CLAUSE: &str = "PermissionsBasicClause";
pub const PERMISSIONS_EXPRESSION_CLAUSE: &str = "PermissionsExpressionClause";

// ---- Targets -------------------------------------------------------------

pub const CREATE_TARGET: &str = "CreateTarget";
pub const RELATE_SUBJECT: &str = "RelateSubject";

// ---- Values and expressions ----------------------------------------------

pub const BINARY_EXPRESSION: &str = "BinaryExpression";
pub const PATH: &str = "Path";
pub const PATH_ELEMENT: &str = "PathElement";
pub const SUBSCRIPT: &str = "Subscript";
pub const ARGUMENT_LIST: &str = "ArgumentList";
pub const FUNCTION_CALL: &str = "FunctionCall";
pub const SCRIPTING_FUNCTION: &str = "FunctionJs";
pub const JS_FUNCTION_BODY: &str = "JavaScriptBlock";

pub const PARAM_LIST: &str = "ParamList";
pub const PARAM_DEFINITION: &str = "ParamDefinition";

pub const FIELD_ASSIGNMENT: &str = "FieldAssignment";
pub const OBJECT: &str = "Object";
pub const OBJECT_CONTENT: &str = "ObjectContent";
pub const OBJECT_PROPERTY: &str = "ObjectProperty";
pub const OBJECT_KEY: &str = "ObjectKey";
pub const ARRAY: &str = "Array";

pub const RECORD_ID: &str = "RecordId";
pub const RECORD_ID_VALUE: &str = "RecordIdValue";
pub const RECORD_ID_RANGE: &str = "RangeRecordId";
pub const MULTI_RECORD: &str = "MultiRecord";

pub const IDENT: &str = "Ident";
pub const IDIOM: &str = "Idiom";
pub const CUSTOM_FUNCTION_NAME: &str = "FunctionName";
pub const BUILTIN_FUNCTION_NAME: &str = "FunctionName";
pub const FUNCTION_NAME: &str = "FunctionName";
pub const VARIABLE_NAME: &str = "VariableName";
pub const TYPE_NAME: &str = "TypeName";
pub const FUNCTION_JS: &str = "FunctionJs";

pub const NUMBER: &str = "Number";
pub const INT: &str = "Int";
pub const FLOAT: &str = "Float";
pub const DECIMAL: &str = "Decimal";
pub const STRING: &str = "String";
pub const PREFIXED_STRING: &str = "PrefixedString";
pub const REGEX: &str = "Regex";
pub const DURATION: &str = "Duration";

pub const TYPE: &str = "Type";
pub const PARAMETERIZED_TYPE: &str = "ParameterizedType";

// ---- Operators / comments ------------------------------------------------

pub const OPERATOR: &str = "Operator";
pub const ASSIGNMENT_OPERATOR: &str = "AssignmentOperator";
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

/// True when `kind` is one of the dedicated `define_*_statement` kinds.
pub fn is_define_statement(kind: &str) -> bool {
    kind == DEFINE_STATEMENT || kind.starts_with("define_") && kind.ends_with("_statement")
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
