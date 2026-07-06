//! Tree-sitter node-kind constants and small helpers.
//!
//! The grammar lives at `../surrealql-tree-sitter`. As of grammar **v3**
//! every rule is emitted in `snake_case` (e.g. `select_statement`,
//! `identifier`, `function_call`), each `DEFINE ...` form is its own
//! dedicated statement kind (`define_table_statement`,
//! `define_field_statement`, …) rather than one generic `DefineStatement`,
//! and every keyword is its own node (`keyword_from`, `keyword_where`, …)
//! instead of a single `Keyword` kind. The constants below cover every
//! kind the analyzer and highlighter dispatch on. Keep them in sync with
//! `surrealql-tree-sitter/src/node-types.json`.

use tree_sitter::Node;

// ---- Top-level / transparent ---------------------------------------------

/// Root node emitted by the grammar.
pub const SOURCE_FILE: &str = "source_file";

// Transparent wrapper rules that merely nest statements/values:
// `expressions`, `expression`, `subquery_statement`, `value`,
// `base_value`, `predicate`, `inclusive_predicate`. The analyzer descends
// through them via `named_children()`.
pub const VALUE: &str = "value";
pub const BASE_VALUE: &str = "base_value";
pub const PREDICATE: &str = "predicate";
pub const INCLUSIVE_PREDICATE: &str = "inclusive_predicate";
pub const SUB_QUERY: &str = "sub_query";
pub const BLOCK: &str = "block";

// ---- Define statement family (v3: one kind per DEFINE form) ---------------

pub const DEFINE_TABLE_STATEMENT: &str = "define_table_statement";
pub const DEFINE_FIELD_STATEMENT: &str = "define_field_statement";
pub const DEFINE_EVENT_STATEMENT: &str = "define_event_statement";
pub const DEFINE_FUNCTION_STATEMENT: &str = "define_function_statement";
pub const DEFINE_INDEX_STATEMENT: &str = "define_index_statement";
pub const DEFINE_PARAM_STATEMENT: &str = "define_param_statement";
pub const DEFINE_ACCESS_STATEMENT: &str = "define_access_statement";
pub const DEFINE_SCOPE_STATEMENT: &str = "define_scope_statement";

// ---- CRUD statements -----------------------------------------------------

pub const SELECT_STATEMENT: &str = "select_statement";
pub const CREATE_STATEMENT: &str = "create_statement";
pub const UPDATE_STATEMENT: &str = "update_statement";
pub const UPSERT_STATEMENT: &str = "upsert_statement";
pub const DELETE_STATEMENT: &str = "delete_statement";
pub const RELATE_STATEMENT: &str = "relate_statement";
pub const INSERT_STATEMENT: &str = "insert_statement";
pub const LET_STATEMENT: &str = "let_statement";

// ---- Clauses -------------------------------------------------------------

pub const ON_TABLE_CLAUSE: &str = "on_table_clause";
pub const TYPE_CLAUSE: &str = "type_clause";
pub const COMMENT_CLAUSE: &str = "comment_clause";
pub const CONTENT_CLAUSE: &str = "content_clause";
pub const SET_CLAUSE: &str = "set_clause";
pub const RETURN_CLAUSE: &str = "return_clause";
pub const RETURNS_CLAUSE: &str = "returns_clause";
pub const WHERE_CLAUSE: &str = "where_clause";
pub const FROM_CLAUSE: &str = "from_clause";
pub const SELECT_CLAUSE: &str = "select_clause";
pub const GROUP_CLAUSE: &str = "group_clause";

pub const WHEN_THEN_CLAUSE: &str = "when_then_clause";

pub const FIELDS_COLUMNS_CLAUSE: &str = "fields_columns_clause";
pub const UNIQUE_CLAUSE: &str = "unique_clause";

pub const PERMISSIONS_FOR_CLAUSE: &str = "permissions_for_clause";
pub const PERMISSIONS_BASIC_CLAUSE: &str = "permissions_basic_clause";
pub const PERMISSIONS_EXPRESSION_CLAUSE: &str = "permissions_expression_clause";

// ---- Targets -------------------------------------------------------------

pub const CREATE_TARGET: &str = "create_target";
pub const RELATE_SUBJECT: &str = "relate_subject";

// ---- Values and expressions ----------------------------------------------

pub const BINARY_EXPRESSION: &str = "binary_expression";
pub const PATH: &str = "path";
pub const PATH_ELEMENT: &str = "path_element";
pub const SUBSCRIPT: &str = "subscript";
pub const ARGUMENT_LIST: &str = "argument_list";
pub const FUNCTION_CALL: &str = "function_call";
pub const SCRIPTING_FUNCTION: &str = "scripting_function";
pub const JS_FUNCTION_BODY: &str = "js_function_body";

pub const PARAM_LIST: &str = "param_list";

pub const FIELD_ASSIGNMENT: &str = "field_assignment";
pub const OBJECT: &str = "object";
pub const OBJECT_CONTENT: &str = "object_content";
pub const OBJECT_PROPERTY: &str = "object_property";
pub const OBJECT_KEY: &str = "object_key";
pub const ARRAY: &str = "array";

pub const RECORD_ID: &str = "record_id";
pub const RECORD_ID_VALUE: &str = "record_id_value";
pub const RECORD_ID_RANGE: &str = "record_id_range";
pub const MULTI_RECORD: &str = "multi_record";

pub const IDENT: &str = "identifier";
pub const CUSTOM_FUNCTION_NAME: &str = "custom_function_name";
pub const BUILTIN_FUNCTION_NAME: &str = "builtin_function_name";
pub const FUNCTION_NAME: &str = "function_name";
pub const VARIABLE_NAME: &str = "variable_name";
pub const TYPE_NAME: &str = "type_name";
pub const FUNCTION_JS: &str = "scripting_function";

pub const NUMBER: &str = "number";
pub const INT: &str = "int";
pub const FLOAT: &str = "float";
pub const DECIMAL: &str = "decimal";
pub const STRING: &str = "string";
pub const PREFIXED_STRING: &str = "prefixed_string";
pub const REGEX: &str = "regex";
pub const DURATION: &str = "duration";

pub const TYPE: &str = "type";
pub const PARAMETERIZED_TYPE: &str = "parameterized_type";

// ---- Operators / comments ------------------------------------------------

pub const OPERATOR: &str = "operator";
pub const ASSIGNMENT_OPERATOR: &str = "assignment_operator";
pub const COMMENT: &str = "comment";

// ---- Helpers --------------------------------------------------------------

/// Returns the source text covered by `node`, trimmed.
pub fn text_of<'a>(source: &'a str, node: Node<'_>) -> Option<&'a str> {
    node.utf8_text(source.as_bytes()).ok().map(str::trim)
}

/// True when `node` is a keyword node. In grammar v3 every keyword is its
/// own kind (`keyword_from`, `keyword_where`, …), so a prefix test covers
/// them all.
pub fn is_keyword(node: Node<'_>) -> bool {
    node.kind().starts_with("keyword_")
}

/// True when `node` is a keyword whose source text matches `expected`
/// (case-insensitive). Works across every dedicated `keyword_*` kind.
pub fn is_kw(node: Node<'_>, source: &str, expected: &str) -> bool {
    is_keyword(node)
        && text_of(source, node).is_some_and(|text| text.eq_ignore_ascii_case(expected))
}

/// True when `kind` is one of the dedicated `define_*_statement` kinds.
pub fn is_define_statement(kind: &str) -> bool {
    kind.starts_with("define_") && kind.ends_with("_statement")
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
    if node.kind() == IDENT
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
