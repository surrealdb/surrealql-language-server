use std::collections::HashMap;
use std::sync::Arc;

use ls_types::{Diagnostic, DocumentSymbol, Location, Range, SymbolKind, Uri};
use serde::{Deserialize, Serialize};
use tree_sitter::Tree;

use crate::semantic::text::LineIndex;
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

/// The `TYPE RELATION IN a|b OUT c|d` half of a `DEFINE TABLE`.
///
/// A faithful record of what the source declares, which is why it lives on
/// [`TableDef`] rather than on the merged model. The *derived* graph — which
/// includes edges only a `RELATE` statement witnesses — is
/// [`MergedSemanticModel::graph_edges`] instead.
///
/// Either list can be empty: `TYPE RELATION` alone is legal and constrains
/// neither endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RelationDef {
    /// The tables an edge row may point *from*, written `IN` or `FROM`.
    pub in_tables: Vec<String>,
    /// The tables an edge row may point *to*, written `OUT` or `TO`.
    pub out_tables: Vec<String>,
    pub enforced: bool,
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
    /// `Some` only for a table declared `TYPE RELATION`. `#[serde(default)]`
    /// keeps a previously-serialized definition loading.
    #[serde(default)]
    pub relation: Option<RelationDef>,
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

impl IndexDef {
    /// The analyzer this index names, if it names one.
    ///
    /// The grammar gives the analyzer no field of its own, and the option
    /// clause arrives as one unsplit string (`"SEARCH ANALYZER simple BM25"`),
    /// so the name is the word after `ANALYZER`.
    pub fn analyzer(&self) -> Option<&str> {
        self.options.iter().find_map(|option| {
            let mut words = option.split_whitespace();
            while let Some(word) = words.next() {
                if word.eq_ignore_ascii_case("analyzer") {
                    return words.next();
                }
            }
            None
        })
    }
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

/// One `RELATE a->edge->b` sighting: proof that `edge` joins two tables.
///
/// Most SurrealQL schemas never declare their edge tables — SurrealDB's own
/// graph corpus defines `person` as `SCHEMALESS` and creates `knows` purely
/// through `RELATE`. Without this, the graph would be empty for exactly the
/// projects that use graphs most.
///
/// An observation is weaker evidence than a `TYPE RELATION` declaration, so
/// [`MergedSemanticModel::reindex_graph_edges`] reads the declarations first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeObservation {
    /// The edge table — the middle subject of the `RELATE`.
    pub edge: String,
    /// The table the edge points from. `None` when the subject is a
    /// `$parameter`, a call, or an array, which name no table statically.
    pub from: Option<String>,
    /// The table the edge points to, under the same rule as [`Self::from`].
    pub to: Option<String>,
}

/// A `REMOVE` statement, so the merged model can drop what it removes.
///
/// Without this, `REMOVE TABLE person` left `person` in the model and every
/// later query against it looked fine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removal {
    /// The `DEFINE` form being removed, lowercased: `table`, `field`,
    /// `function`, and so on.
    pub form: String,
    /// The name given after the form keyword.
    pub name: String,
    /// The table from an `ON <table>` clause, for a field, event or index.
    pub table: Option<String>,
    pub location: Location,
}

/// A name paired with the tight range of the token that produced it,
/// so diagnostics can underline `prson` instead of the whole statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedRange {
    pub name: String,
    /// Byte offsets, not a [`Range`].
    ///
    /// A query-heavy document records four of these per statement, and
    /// converting each to a line-and-column pair here cost 25,600 conversions
    /// on a 3200-statement file — the same shape as the position-conversion
    /// hotspot the `LineIndex` work removed. A diagnostic converts one, when it
    /// is emitted; the edit path converts none.
    pub start: usize,
    pub end: usize,
}

impl NamedRange {
    /// The line-and-column range, converted on demand.
    pub fn range(&self, source: &str, lines: &crate::semantic::text::LineIndex) -> Range {
        lines.range(source, self.start, self.end)
    }
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
    /// Line start offsets for [`Self::text`], built once per analysis so every
    /// byte-offset-to-[`Position`] conversion is a binary search rather than a
    /// scan from byte 0. Request handlers reuse it for cursor lookups too.
    ///
    /// [`Position`]: ls_types::Position
    pub line_index: LineIndex,
    pub tables: Vec<TableDef>,
    pub events: Vec<EventDef>,
    pub indexes: Vec<IndexDef>,
    pub fields: Vec<FieldDef>,
    pub functions: Vec<FunctionDef>,
    pub params: Vec<ParamDef>,
    pub accesses: Vec<AccessDef>,
    pub analyzers: Vec<AnalyzerDef>,
    pub query_facts: Vec<QueryFact>,
    /// Every `RELATE a->edge->b` this document writes, in source order.
    pub edge_observations: Vec<EdgeObservation>,
    pub references: Vec<SymbolReference>,
    pub syntax_diagnostics: Vec<Diagnostic>,
    pub document_symbols: Vec<DocumentSymbol>,
    /// Every name this document refers to, deduplicated. See
    /// `collect_referenced_names`.
    pub referenced_names: Vec<String>,
    /// Every `REMOVE` this document performs, in source order.
    pub removals: Vec<Removal>,
    /// `-- surql-ignore` directives found in this document, collected during
    /// the same walk that produces everything else here. Applied at the very
    /// end of [`crate::semantic::pipeline::diagnostics_for_document`].
    pub suppressions: crate::semantic::suppress::Suppressions,
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

/// Which way a graph hop points.
///
/// The grammar has three arrow kinds and this mirrors them: `->` is
/// [`Self::Right`], `<-` and `<~` are [`Self::Left`], and `<->` is
/// [`Self::Both`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupDirection {
    Right,
    Left,
    Both,
}

impl LookupDirection {
    /// Pick which of a rightward and a leftward index this direction reads.
    ///
    /// [`Self::Both`] reads each in turn, so every caller stays a single loop
    /// instead of branching three ways. Yields references, and allocates
    /// nothing — this runs on the completion path.
    pub fn maps<'a, T>(self, rightward: &'a T, leftward: &'a T) -> impl Iterator<Item = &'a T> {
        let (first, second) = match self {
            Self::Right => (rightward, None),
            Self::Left => (leftward, None),
            Self::Both => (rightward, Some(leftward)),
        };
        std::iter::once(first).chain(second)
    }
}

/// Which edge tables leave, and which arrive at, each table.
///
/// Both maps are keyed by an *endpoint* table name and hold edge table names,
/// which is the direction completion and type inference ask in: "I am on
/// `person` and I typed `->` — what can I traverse?"
///
/// `outgoing["person"]` answers for `->`; `incoming["person"]` answers for
/// `<-`. An edge that declares neither endpoint appears in neither map, and an
/// edge whose two endpoints are the same table appears in both.
#[derive(Debug, Clone, Default)]
pub struct GraphIndex {
    /// Endpoint table → edge tables reachable with `->`.
    pub outgoing: HashMap<String, Vec<String>>,
    /// Endpoint table → edge tables reachable with `<-`.
    pub incoming: HashMap<String, Vec<String>>,
    /// Edge table → the tables it points *to*, for the second hop of
    /// `->edge->target`.
    pub edge_targets: HashMap<String, Vec<String>>,
    /// Edge table → the tables it points *from*, for `<-edge<-source`.
    pub edge_sources: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct MergedSemanticModel {
    pub tables: HashMap<String, TableDef>,
    pub events: HashMap<(String, String), EventDef>,
    pub indexes: HashMap<(String, String), IndexDef>,
    /// Every field, grouped by the table it belongs to: table name → field
    /// name → definition.
    ///
    /// Nested rather than keyed by a `(table, field)` tuple so a lookup
    /// borrows both halves instead of allocating them. A tuple key cannot be
    /// borrowed from a `(&str, &str)` pair, so every read of a flat map had to
    /// build — and then drop — two `String`s. `fields_for_table` paid that per
    /// field, on a path completion runs per table.
    ///
    /// The outer map also replaces the separate `fields_by_table` index: the
    /// inner map's keys *are* the field names of a table.
    ///
    /// Insert through [`MergedSemanticModel::insert_field`][insert_field],
    /// which applies the origin-priority merge.
    ///
    /// [insert_field]: MergedSemanticModel::insert_field
    pub fields: HashMap<String, HashMap<String, FieldDef>>,
    pub functions: HashMap<String, FunctionDef>,
    pub params: HashMap<String, ParamDef>,
    pub accesses: HashMap<String, AccessDef>,
    pub analyzers: HashMap<String, AnalyzerDef>,
    pub function_references: HashMap<String, Vec<Location>>,
    /// Every name any document refers to.
    ///
    /// A set rather than a position index: this is on the edit path, and only
    /// `unused-binding` reads it, which needs a yes-or-no answer. Positions are
    /// computed on demand by [`Self::references_for_symbol`].
    pub referenced_names: std::collections::HashSet<String>,
    pub function_callers: HashMap<String, Vec<String>>,
    /// The return type read out of a function *body*, for the functions that
    /// declare none. Keyed by full name, `fn::` prefix included.
    ///
    /// A derived cross-document fact, so it belongs here rather than on
    /// [`FunctionDef`] — the same reason [`Self::function_callers`] does.
    /// `FunctionDef` stays a faithful record of what the source says, and
    /// `FunctionDef::return_type` keeps meaning "the author wrote this".
    ///
    /// Filled by
    /// [`crate::semantic::infer::infer_function_return_types`].
    pub inferred_function_returns: HashMap<String, TypeExpr>,
    pub workspace_symbols: Vec<DocumentSymbol>,
    pub query_facts: HashMap<Uri, Vec<QueryFact>>,
    /// How many query facts across the workspace target each table name.
    ///
    /// Derived from [`Self::query_facts`] by
    /// [`MergedSemanticModel::reindex_target_usage`][reindex]. The
    /// unknown-table check needs this per inferred target in the document being
    /// diagnosed, and counting it on demand meant flattening every fact in the
    /// workspace each time.
    ///
    /// [reindex]: MergedSemanticModel::reindex_target_usage
    pub target_usage: HashMap<String, usize>,
    /// The names of the *explicitly defined* tables — the only candidates a
    /// "did you mean" sweep may offer.
    ///
    /// Derived, and maintained by
    /// [`MergedSemanticModel::insert_table`][insert_table] alongside
    /// [`Self::tables`]. Insert through that method rather than writing to
    /// `tables` directly, or the sweep will not see the table.
    ///
    /// Exists because the sweep read `tables.values()` and filtered on
    /// `explicit` afterwards. In a workspace where most tables are inferred from
    /// usage that walks the whole map — thousands of ~230-byte entries streamed
    /// to read one `bool` — to reach a candidate set a fraction of the size.
    ///
    /// [insert_table]: MergedSemanticModel::insert_table
    pub explicit_tables: Vec<String>,
    /// True when the live-metadata fetch reported errors while this
    /// model was built — remote tables may be missing, so
    /// unknown-name judgments are unreliable until recovery.
    pub metadata_degraded: bool,
    /// The graph topology, derived from every `TableDef::relation` and every
    /// [`EdgeObservation`] in the workspace.
    ///
    /// Derived and cross-document, so it belongs here rather than on
    /// [`TableDef`] — the same reason [`Self::function_callers`] does, and a
    /// load-bearing one: [`MergedSemanticModel::insert_table`] *replaces* a
    /// `TableDef` wholesale when a higher-origin definition wins, which would
    /// discard any observed edges stored there.
    ///
    /// Rebuilt by
    /// [`MergedSemanticModel::reindex_graph_edges`][reindex_graph_edges].
    ///
    /// [reindex_graph_edges]: MergedSemanticModel::reindex_graph_edges
    pub graph_edges: GraphIndex,
}
