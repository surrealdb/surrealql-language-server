use std::collections::HashMap;
use std::sync::Arc;

use ls_types::{Diagnostic, DocumentSymbol, Location, Range, SymbolKind, Uri};
use serde::{Deserialize, Serialize};
use tree_sitter::Tree;

use crate::semantic::type_expr::TypeExpr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SymbolOrigin {
    Builtin,
    Inferred,
    Remote,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessResult {
    Allowed,
    Denied,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QueryAction {
    Select,
    Create,
    Update,
    Delete,
    Relate,
    Execute,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferenceFact {
    pub confidence: f32,
    pub origin: SymbolOrigin,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PermissionMode {
    Full,
    None,
    Expression(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub actions: Vec<QueryAction>,
    pub mode: PermissionMode,
    pub raw: String,
    pub origin: SymbolOrigin,
    pub location: Option<Location>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldDef {
    pub table: String,
    pub name: String,
    pub type_expr: Option<TypeExpr>,
    pub comment: Option<String>,
    pub permissions: Vec<PermissionRule>,
    pub origin: SymbolOrigin,
    pub explicit: bool,
    pub inference: Option<InferenceFact>,
    pub location: Location,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableDef {
    pub name: String,
    pub schema_mode: Option<String>,
    pub comment: Option<String>,
    pub permissions: Vec<PermissionRule>,
    pub origin: SymbolOrigin,
    pub explicit: bool,
    pub inference: Option<InferenceFact>,
    pub location: Location,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventDef {
    pub table: String,
    pub name: String,
    pub comment: Option<String>,
    pub when_clause: Option<String>,
    pub then_clause: Option<String>,
    pub origin: SymbolOrigin,
    pub location: Location,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexDef {
    pub table: String,
    pub name: String,
    pub fields: Vec<String>,
    pub unique: bool,
    pub options: Vec<String>,
    pub origin: SymbolOrigin,
    pub location: Location,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FunctionLanguage {
    #[default]
    SurrealQL,
    JavaScript,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionParam {
    pub name: String,
    pub type_expr: Option<TypeExpr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub params: Vec<FunctionParam>,
    pub return_type: Option<TypeExpr>,
    pub language: FunctionLanguage,
    pub comment: Option<String>,
    pub permissions: Vec<PermissionRule>,
    pub origin: SymbolOrigin,
    pub explicit: bool,
    pub inference: Option<InferenceFact>,
    pub location: Location,
    pub selection_range: Range,
    pub body_range: Option<Range>,
    pub called_functions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamDef {
    pub name: String,
    pub value_preview: Option<String>,
    pub comment: Option<String>,
    pub origin: SymbolOrigin,
    pub location: Location,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccessDef {
    pub name: String,
    pub comment: Option<String>,
    pub origin: SymbolOrigin,
    pub location: Location,
}

/// A `DEFINE ANALYZER`.
///
/// Indexed because an analyzer name is referenced by name elsewhere —
/// `DEFINE INDEX … FULLTEXT ANALYZER <name>` — and completion cannot offer a
/// name that nothing extracts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyzerDef {
    pub name: String,
    pub comment: Option<String>,
    pub origin: SymbolOrigin,
    pub location: Location,
}

/// A name paired with the tight range of the token that produced it,
/// so diagnostics can underline `prson` instead of the whole statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedRange {
    pub name: String,
    pub range: Range,
}

/// Why a statement's target table list is (or isn't) statically known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TargetResolution {
    /// Targets are literal table names / record ids.
    Static,
    /// The target is a `$parameter` — resolvable only at runtime, and
    /// not worth a "could not be resolved" warning.
    Parameter,
    /// The target is an expression (function call, subquery, block).
    Expression,
    /// Nothing recognisable — the legacy "dynamic" case.
    #[default]
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryFact {
    pub action: QueryAction,
    pub target_tables: Vec<String>,
    pub touched_fields: Vec<String>,
    pub dynamic: bool,
    pub location: Location,
    pub source_preview: String,
    /// Tight token ranges for [`Self::target_tables`] entries.
    /// `#[serde(default)]` keeps previously-serialized facts loading.
    #[serde(default)]
    pub target_refs: Vec<NamedRange>,
    /// Tight token ranges for [`Self::touched_fields`] entries.
    #[serde(default)]
    pub field_refs: Vec<NamedRange>,
    /// How the target list was resolved (drives warning suppression
    /// for `$param` / expression targets).
    #[serde(default)]
    pub target_resolution: TargetResolution,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolReference {
    pub name: String,
    pub kind: SymbolKind,
    pub location: Location,
    pub selection_range: Range,
}

#[derive(Debug, Clone)]
pub struct DocumentAnalysis {
    pub uri: Uri,
    pub text: String,
    /// The tree-sitter parse of [`Self::text`], cached so request
    /// handlers (semantic tokens, inlay hints, …) reuse it instead of
    /// re-parsing the document on every call. `Tree::clone` is a shallow,
    /// ref-counted copy, so storing it is cheap. This is also the
    /// foundation for incremental re-parsing once the server moves to
    /// incremental document sync.
    pub tree: Tree,
    pub tables: Vec<TableDef>,
    pub events: Vec<EventDef>,
    pub indexes: Vec<IndexDef>,
    pub fields: Vec<FieldDef>,
    pub functions: Vec<FunctionDef>,
    pub params: Vec<ParamDef>,
    pub accesses: Vec<AccessDef>,
    pub analyzers: Vec<AnalyzerDef>,
    pub query_facts: Vec<QueryFact>,
    pub references: Vec<SymbolReference>,
    pub syntax_diagnostics: Vec<Diagnostic>,
    pub document_symbols: Vec<DocumentSymbol>,
}

/// What a workspace scan had to skip. Non-zero counters are reported
/// to the client so silent truncation doesn't look like coverage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkspaceScanStats {
    /// Directory entries the walker could not read (permissions, IO).
    pub walk_errors: usize,
    /// Files skipped because they exceed the size ceiling.
    pub skipped_oversize: usize,
    /// Files that matched but could not be read as UTF-8 text.
    pub skipped_unreadable: usize,
    /// True when the workspace file cap stopped the scan early.
    pub file_cap_hit: bool,
}

/// Documents are shared via [`Arc`] so that cloning a [`WorkspaceIndex`]
/// across the background task / read-snapshot boundary is O(documents) pointer
/// copies instead of O(total source bytes) string clones.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceIndex {
    pub documents: HashMap<Uri, Arc<DocumentAnalysis>>,
    pub scan_stats: WorkspaceScanStats,
}

#[derive(Debug, Clone, Default)]
pub struct LiveMetadataSnapshot {
    pub documents: HashMap<Uri, Arc<DocumentAnalysis>>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MergedSemanticModel {
    pub tables: HashMap<String, TableDef>,
    pub events: HashMap<(String, String), EventDef>,
    pub indexes: HashMap<(String, String), IndexDef>,
    pub fields: HashMap<(String, String), FieldDef>,
    pub functions: HashMap<String, FunctionDef>,
    pub params: HashMap<String, ParamDef>,
    pub accesses: HashMap<String, AccessDef>,
    pub analyzers: HashMap<String, AnalyzerDef>,
    pub function_references: HashMap<String, Vec<Location>>,
    pub function_callers: HashMap<String, Vec<String>>,
    pub workspace_symbols: Vec<DocumentSymbol>,
    pub query_facts: HashMap<Uri, Vec<QueryFact>>,
    /// True when the live-metadata fetch reported errors while this
    /// model was built — remote tables may be missing, so
    /// unknown-name judgments are unreliable until recovery.
    pub metadata_degraded: bool,
}
