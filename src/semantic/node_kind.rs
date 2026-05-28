//! Tree-sitter node-kind constants and small helpers.
//!
//! The grammar lives at `../surrealql-tree-sitter` and mirrors
//! `@surrealdb/lezer` 1:1, so every visible rule uses PascalCase. Some
//! rules are intentionally "hidden" in the new grammar (leading
//! underscore in `grammar.js`); their children appear directly under the
//! parent in the tree. The constants below cover every kind the
//! analyzer needs to dispatch on. Keep them in sync with
//! `surrealql-tree-sitter/grammar.js`.

use tree_sitter::Node;

// ---- Top-level / transparent ---------------------------------------------

/// Root node emitted by `grammar.js::SurrealQL`.
pub const SURREALQL: &str = "SurrealQL";

// Hidden grammar rules (`_expressions`, `_expression`, `_subqueryStatement`,
// `_statement`, `_value`, `_baseValue`, `_computedValue`, `_modifierClause`,
// `_dataClause`, `_pathElement`, `_dotPart`, `_idName`, `_recordIdValue`,
// `_inclusivePredicate`, `_type`, etc.) do not appear in the tree. The
// analyzer must descend through `named_children()` to find what it wants.

// ---- Define statement family ---------------------------------------------

/// All `DEFINE ...` statements share a single visible kind. The variant
/// (TABLE / FIELD / INDEX / FUNCTION / PARAM / EVENT / ACCESS / TOKEN /
/// USER / ANALYZER / SCOPE / CONFIG / API / BUCKET / NAMESPACE / DATABASE)
/// is encoded as the text of the second child, which is either a
/// `Keyword` or a dedicated wrapper (`AccessDefinition`,
/// `ScopeDefinition`).
pub const DEFINE_STATEMENT: &str = "DefineStatement";
pub const ACCESS_DEFINITION: &str = "AccessDefinition";
pub const SCOPE_DEFINITION: &str = "ScopeDefinition";

pub const ALTER_STATEMENT: &str = "AlterStatement";
pub const REMOVE_STATEMENT: &str = "RemoveStatement";
pub const REBUILD_STATEMENT: &str = "RebuildStatement";

// ---- CRUD statements -----------------------------------------------------

pub const SELECT_STATEMENT: &str = "SelectStatement";
pub const CREATE_STATEMENT: &str = "CreateStatement";
pub const UPDATE_STATEMENT: &str = "UpdateStatement";
pub const UPSERT_STATEMENT: &str = "UpsertStatement";
pub const DELETE_STATEMENT: &str = "DeleteStatement";
pub const RELATE_STATEMENT: &str = "RelateStatement";
pub const INSERT_STATEMENT: &str = "InsertStatement";

pub const RETURN_STATEMENT: &str = "ReturnStatement";
pub const LET_STATEMENT: &str = "LetStatement";
pub const FOR_STATEMENT: &str = "ForStatement";
pub const IF_ELSE_STATEMENT: &str = "IfElseStatement";

// ---- Clauses -------------------------------------------------------------

pub const ON_TABLE_CLAUSE: &str = "OnTableClause";
pub const ON_ROOT_NS_DB_CLAUSE: &str = "OnRootNsDbClause";
pub const TYPE_CLAUSE: &str = "TypeClause";
pub const DEFAULT_CLAUSE: &str = "DefaultClause";
pub const ASSERT_CLAUSE: &str = "AssertClause";
pub const VALUE_CLAUSE: &str = "ValueClause";
pub const READONLY_CLAUSE: &str = "ReadonlyClause";
pub const REFERENCE_CLAUSE: &str = "ReferenceClause";
pub const COMPUTED_CLAUSE: &str = "ComputedClause";
pub const COMMENT_CLAUSE: &str = "CommentClause";
pub const CONTENT_CLAUSE: &str = "ContentClause";
pub const SET_CLAUSE: &str = "SetClause";
pub const MERGE_CLAUSE: &str = "MergeClause";
pub const PATCH_CLAUSE: &str = "PatchClause";
pub const REPLACE_CLAUSE: &str = "ReplaceClause";
pub const UNSET_CLAUSE: &str = "UnsetClause";
pub const RETURN_CLAUSE: &str = "ReturnClause";
pub const WHERE_CLAUSE: &str = "WhereClause";
pub const WITH_CLAUSE: &str = "WithClause";
pub const SPLIT_CLAUSE: &str = "SplitClause";
pub const GROUP_CLAUSE: &str = "GroupClause";
pub const ORDER_CLAUSE: &str = "OrderClause";
pub const LIMIT_START_COMBO_CLAUSE: &str = "LimitStartComboClause";
pub const FETCH_CLAUSE: &str = "FetchClause";
pub const TIMEOUT_CLAUSE: &str = "TimeoutClause";
pub const PARALLEL_CLAUSE: &str = "ParallelClause";
pub const TEMPFILES_CLAUSE: &str = "TempfilesClause";
pub const EXPLAIN_CLAUSE: &str = "ExplainClause";
pub const VERSION_CLAUSE: &str = "VersionClause";
pub const OMIT_CLAUSE: &str = "OmitClause";

pub const WHEN_CLAUSE: &str = "WhenClause";
pub const THEN_CLAUSE: &str = "ThenClause";

pub const FIELDS_COLUMNS_CLAUSE: &str = "FieldsColumnsClause";
pub const INDEX_CLAUSE: &str = "IndexClause";
pub const UNIQUE_CLAUSE: &str = "UniqueClause";
pub const SEARCH_ANALYZER_CLAUSE: &str = "SearchAnalyzerClause";
pub const MTREE_CLAUSE: &str = "MtreeClause";
pub const HNSW_CLAUSE: &str = "HnswClause";
pub const INDEX_DIMENSION_CLAUSE: &str = "IndexDimensionClause";

pub const PERMISSIONS_FOR_CLAUSE: &str = "PermissionsForClause";
pub const PERMISSIONS_BASIC_CLAUSE: &str = "PermissionsBasicClause";
pub const PERMISSION_GROUP: &str = "PermissionGroup";

pub const TABLE_TYPE_CLAUSE: &str = "TableTypeClause";
pub const TABLE_VIEW_CLAUSE: &str = "TableViewClause";
pub const CHANGEFEED_CLAUSE: &str = "ChangefeedClause";
pub const DURATION_CLAUSE: &str = "DurationClause";

// ---- Values and expressions ----------------------------------------------

pub const BINARY_EXPRESSION: &str = "BinaryExpression";
pub const PATH: &str = "Path";
pub const RANGE: &str = "Range";
pub const ARGUMENT_LIST: &str = "ArgumentList";
pub const FUNCTION_CALL: &str = "FunctionCall";
pub const FUNCTION_JS: &str = "FunctionJs";
pub const JAVASCRIPT_BLOCK: &str = "JavaScriptBlock";

pub const BLOCK: &str = "Block";
pub const SUB_QUERY: &str = "SubQuery";
pub const FIELDS: &str = "Fields";

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
pub const RANGE_RECORD_ID: &str = "RangeRecordId";
pub const RECORD_TB_IDENT: &str = "RecordTbIdent";
pub const RECORD_ID_IDENT: &str = "RecordIdIdent";

pub const IDENT: &str = "Ident";
pub const IDIOM: &str = "Idiom";
pub const TYPE_NAME: &str = "TypeName";
pub const FUNCTION_NAME: &str = "FunctionName";
pub const VARIABLE_NAME: &str = "VariableName";

pub const NUMBER: &str = "Number";
pub const INT: &str = "Int";
pub const FLOAT: &str = "Float";
pub const DECIMAL: &str = "Decimal";
pub const STRING: &str = "String";
pub const FORMAT_STRING: &str = "FormatString";
pub const REGEX: &str = "Regex";
pub const DURATION: &str = "Duration";
pub const DURATION_PART: &str = "DurationPart";

pub const BOOL: &str = "Bool";
pub const NONE: &str = "None";
pub const LITERAL: &str = "Literal";

pub const TYPE: &str = "Type";

// ---- Visible keyword and operator nodes ----------------------------------

pub const KEYWORD: &str = "Keyword";
pub const OPERATOR: &str = "Operator";
pub const COLON: &str = "Colon";
pub const PIPE: &str = "Pipe";
pub const BRACE_OPEN: &str = "BraceOpen";
pub const BRACE_CLOSE: &str = "BraceClose";

// ---- Helpers --------------------------------------------------------------

/// Returns the source text covered by `node`, trimmed.
pub fn text_of<'a>(source: &'a str, node: Node<'_>) -> Option<&'a str> {
    node.utf8_text(source.as_bytes()).ok().map(str::trim)
}

/// Returns true when `node` is a `Keyword` whose source text matches
/// `expected` (case-insensitive).
pub fn is_kw(node: Node<'_>, source: &str, expected: &str) -> bool {
    if node.kind() != KEYWORD {
        return false;
    }
    text_of(source, node).is_some_and(|text| text.eq_ignore_ascii_case(expected))
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

/// Identify the "kind" of a `DefineStatement` by inspecting its second
/// named child. Returns an uppercase canonical name such as `TABLE`,
/// `FIELD`, `INDEX`, `FUNCTION`, `PARAM`, `EVENT`, `ACCESS`,
/// `NAMESPACE`, etc. Returns `None` if the statement is malformed.
pub fn define_statement_variant(node: Node<'_>, source: &str) -> Option<String> {
    debug_assert_eq!(node.kind(), DEFINE_STATEMENT);
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    let second = children.get(1)?;
    match second.kind() {
        ACCESS_DEFINITION => Some("ACCESS".to_string()),
        SCOPE_DEFINITION => Some("SCOPE".to_string()),
        KEYWORD => text_of(source, *second).map(|s| s.to_ascii_uppercase()),
        _ => None,
    }
}

/// Returns true when any descendant of `node` (inclusive) has the given
/// kind. Useful for "does this block contain a `FunctionJs`?" checks.
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

/// Concatenate the source text of every `Ident` descendant inside an
/// `Idiom`, joined with `.`. Used to canonicalise field names such as
/// `address.city`. When `node` itself is an `Ident`, returns its text.
pub fn idiom_text(source: &str, node: Node<'_>) -> Option<String> {
    if node.kind() == IDENT {
        return text_of(source, node).map(str::to_string);
    }
    if node.kind() != IDIOM {
        return text_of(source, node).map(str::to_string);
    }
    let mut parts = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == IDENT
            && let Some(text) = text_of(source, child)
        {
            parts.push(text.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}
