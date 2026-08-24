//! The rule registry: what every diagnostic code *is*, as data.
//!
//! [`crate::semantic::codes`] owns the wire strings. This module owns
//! everything else about a rule — its category, the severity it reports at,
//! what analysis it needs before it may run, whether it offers a fix, and the
//! prose that explains it.
//!
//! Why a static table rather than a registration macro or an `inventory`-style
//! link-time collector: the crate is compiled for `wasm32` as well as native,
//! link-section tricks do not survive that reliably, and a 17-entry array is
//! already the house pattern (`codes::SCHEMALESS_SCOPED_CODES`,
//! `grammar_generated.rs`). A rule "registers" by appearing in [`RULES`], and
//! the tests at the bottom make it impossible to add a code and forget one.
//!
//! **This module changes no behaviour.** Nothing reads [`RULES`] yet. It is the
//! metadata half of the change, landed on its own so the severity column can be
//! reviewed against the emitters before anything starts depending on it.

use ls_types::{CodeDescription, Diagnostic, DiagnosticSeverity, NumberOrString, Uri};

use crate::config::ServerSettings;
use crate::semantic::codes;

/// A rule's stable identifier — the same string as the wire `Diagnostic.code`.
///
/// The two must never diverge. `every_code_has_a_rule` and
/// `every_rule_has_a_code` below enforce that in both directions.
pub type RuleId = &'static str;

/// What kind of fault a rule reports. Used to group the generated
/// documentation and, later, to let a user silence a whole class at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// The engine refuses to parse it. The query never runs, so these are
    /// never advisory.
    Syntax,
    /// A claim about a table or a field, judged against the merged model.
    Schema,
    /// A claim about the type of a value.
    Types,
    /// Static evaluation of a `PERMISSIONS` clause.
    Permissions,
}

/// What an analysis run must already have computed before a rule may fire.
///
/// Bitflags rather than a list, so the gate is one mask test instead of a
/// search. The constants are cumulative: [`Self::MODEL`] contains
/// [`Self::TREE`], because a rule that needs the whole workspace also needs
/// this document's tree. That folding is done here, once, so a rule cannot
/// declare an incoherent set.
///
/// A *rule* names what it needs. A *caller* names what it has, by combining
/// constants with [`Self::union`]. The server has live metadata and an auth
/// context; an offline command-line run has neither, which is precisely how
/// `unknown-table` stands itself down there instead of reporting every table
/// in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requires(u8);

impl Requires {
    /// This document's parse tree and text. Always available — the syntax pass
    /// runs per-document with no cross-file view at all.
    pub const TREE: Self = Self(0b0001);
    /// [`crate::semantic::types::MergedSemanticModel`] — every symbol in the
    /// workspace, merged by provenance.
    pub const MODEL: Self = Self(0b0011);
    /// A merged model whose live-metadata half is *not* degraded. A rule that
    /// accuses the author of naming something that does not exist needs this;
    /// a half-loaded schema makes every table look wrong.
    pub const LIVE_METADATA: Self = Self(0b0111);
    /// The active auth context out of the settings.
    pub const AUTH_CONTEXT: Self = Self(0b1011);

    /// Everything in `self` and everything in `other`.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Does an environment offering `self` meet everything `needed` asks for?
    pub const fn satisfies(self, needed: Self) -> bool {
        self.0 & needed.0 == needed.0
    }
}

/// How safely a fix can be applied without a human reading it first.
///
/// The distinction matters for a future `--fix` and for "fix all in file": one
/// of these is a lookup in a table the engine itself owns, the other is a
/// spelling guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Applicability {
    /// The replacement is correct by construction, not inferred. Safe to apply
    /// in bulk.
    MachineApplicable,
    /// An edit-distance guess. Offer it; never apply it unattended.
    Suggestion,
}

/// One registered rule.
pub struct Rule {
    pub id: RuleId,
    pub category: Category,
    /// The severity the emitter produces today. Transcribed from the emission
    /// site, never chosen — see the test `severities_are_transcribed_not_chosen`
    /// in `tests/compat.rs`.
    pub default_severity: DiagnosticSeverity,
    pub requires: Requires,
    /// `Some` when this rule offers a quick fix, carrying how safe that fix is.
    /// The producers still live in `MergedSemanticModel::code_actions`; only
    /// the classification lives here.
    pub fix: Option<Applicability>,
    /// One line, for `explain` and the generated rule table.
    pub summary: &'static str,
    /// Long form, for the rule catalogue page. Lifted from the doc comments in
    /// [`crate::semantic::codes`], which is where the reasoning was written
    /// down first.
    pub docs: &'static str,
}

/// Every rule, in wire-code alphabetical order.
///
/// Severity column, for review at a glance: 11 `ERROR`, 6 `WARNING`. The six
/// warnings are the faults the engine still executes — a renamed builtin it
/// silently forwards, a name it parses but cannot dispatch, and the four
/// schema judgements that rest on a model which may be incomplete.
pub static RULES: &[Rule] = &[
    Rule {
        id: codes::ARGUMENT_COUNT,
        category: Category::Types,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::MODEL,
        fix: None,
        summary: "A call passes too many or too few arguments.",
        docs: "The call site's argument count falls outside the arity the \
               function declares. Arity comes from the generated catalogue for \
               a builtin, and from the `DEFINE FUNCTION` signature for a user \
               function. A call with no arguments at all is never reported, so \
               that typing `f(` does not squiggle.",
    },
    Rule {
        id: codes::ARGUMENT_TYPE,
        category: Category::Types,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::MODEL,
        fix: None,
        summary: "An argument's type cannot satisfy the declared parameter type.",
        docs: "Reported only when the coercion relation answers `Incompatible`. \
               An argument whose type cannot be determined answers `Unknown` \
               and is passed over in silence.",
    },
    Rule {
        id: codes::DUPLICATE_DEFINITION,
        category: Category::Schema,
        default_severity: DiagnosticSeverity::WARNING,
        requires: Requires::MODEL,
        fix: None,
        summary: "The same name is defined more than once in the workspace.",
        docs: "Two `DEFINE` statements for the same table, field or function. \
               The merge keeps one and drops the other silently, so the file \
               that loses can look as though it were never read. A warning \
               rather than an error: redefining is legal SurrealQL, and a \
               migration script may do it deliberately.",
    },
    Rule {
        id: codes::DYNAMIC_TARGET,
        category: Category::Schema,
        default_severity: DiagnosticSeverity::WARNING,
        requires: Requires::MODEL,
        fix: None,
        summary: "The statement target could not be resolved to a static table name.",
        docs: "The analyzer could not name the table a statement acts on, and \
               the target is neither a parameter nor an expression — those two \
               are legitimately dynamic and are classified out before this \
               fires.",
    },
    Rule {
        id: codes::FIELD_TYPE,
        category: Category::Types,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::MODEL,
        fix: None,
        summary: "A DEFINE FIELD DEFAULT, VALUE or COMPUTED expression cannot satisfy the declared TYPE.",
        docs: "The engine coerces all three clauses to the declared type and \
               fails with `Couldn't coerce value for field …`. `ASSERT` is \
               deliberately excluded: it is a predicate over `$value`, not a \
               value coerced to the declared type, so nothing may compare it \
               against that type.",
    },
    Rule {
        id: codes::LET_TYPE,
        category: Category::Types,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::MODEL,
        fix: None,
        summary: "A LET value cannot satisfy its declared type.",
        docs: "`LET $x: T = …` where the value's type cannot be coerced to `T`. \
               When the whole-value verdict is silent, each element of an array \
               or object literal is judged on its own, so a single bad member \
               is still reported.",
    },
    Rule {
        id: codes::NOT_CALLABLE,
        category: Category::Types,
        default_severity: DiagnosticSeverity::WARNING,
        requires: Requires::TREE,
        fix: None,
        summary: "A builtin the parser accepts that no implementation backs in call form.",
        docs: "The query parses and then fails at run time. A warning rather \
               than an error, because the claim rests on reading the engine's \
               dispatch tables rather than on the engine refusing the text. \
               Deliberately not applied to the method form.",
    },
    Rule {
        id: codes::OPERATOR_TYPE,
        category: Category::Types,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::MODEL,
        fix: None,
        summary: "An arithmetic operator whose operand types SurrealDB rejects.",
        docs: "Such as `\"a\" + 1`. The operand tables are transcribed \
               arm-by-arm from the engine, and both operand types must be \
               certain before this fires. Division never fails, so it is never \
               reported.",
    },
    Rule {
        id: codes::PERMISSION_DENIED,
        category: Category::Permissions,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::AUTH_CONTEXT,
        fix: None,
        summary: "Static permission evaluation proved the active auth context is denied.",
        docs: "The table or field declares `PERMISSIONS NONE`, or a role check \
               that the active auth context cannot satisfy. `SELECT` and \
               `RELATE` are exempt: their rules are row-level and cannot be \
               decided without data.",
    },
    Rule {
        id: codes::PERMISSION_UNKNOWN,
        category: Category::Permissions,
        default_severity: DiagnosticSeverity::WARNING,
        requires: Requires::AUTH_CONTEXT,
        fix: None,
        summary: "Static permission evaluation could not decide.",
        docs: "The permission expression reads something only the running \
               query knows — a record identity, a session value — or no \
               explicit rule was found at all.",
    },
    Rule {
        id: codes::RENAMED_FUNCTION,
        category: Category::Types,
        default_severity: DiagnosticSeverity::WARNING,
        requires: Requires::TREE,
        fix: Some(Applicability::MachineApplicable),
        summary: "A builtin called by a name SurrealDB has renamed.",
        docs: "The engine still accepts the old name and records the \
               replacement itself, so this is a warning rather than an error. \
               The fix is machine-applicable: the new name comes out of the \
               engine's own rename table, not out of an edit-distance guess.",
    },
    Rule {
        id: codes::RETURN_TYPE,
        category: Category::Types,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::MODEL,
        fix: None,
        summary: "A RETURN yields a value the function's declared return type cannot accept.",
        docs: "The engine coerces a function's result to its declared type and \
               fails with `Couldn't coerce return value from function …`. Both \
               an explicit `RETURN` and a block's trailing expression are \
               checked, including inside an `IF` branch or a `FOR` body, which \
               really do return from the enclosing function.",
    },
    Rule {
        id: codes::UNDEFINED_VARIABLE,
        category: Category::Types,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::MODEL,
        fix: None,
        summary: "A variable reference that nothing in scope binds.",
        docs: "Bindings come from `LET`, function parameters, `FOR` and closure \
               parameters, plus `DEFINE PARAM` and the special variables the \
               engine supplies. Names listed in `analysis.externalParams` are \
               treated as bound.",
    },
    Rule {
        id: codes::UNKNOWN_FIELD,
        category: Category::Schema,
        default_severity: DiagnosticSeverity::WARNING,
        requires: Requires::MODEL,
        fix: None,
        summary: "A query touches a field that is not defined on an explicit table.",
        docs: "Only reported where the schema is closed — `SCHEMAFULL`, or \
               `SCHEMALESS` under `analysis.schemalessDiagnostics: \"strict\"`. \
               `id`, `in` and `out` are always allowed, and `RELATE` is skipped \
               entirely.",
    },
    Rule {
        id: codes::UNKNOWN_TABLE,
        category: Category::Schema,
        default_severity: DiagnosticSeverity::WARNING,
        requires: Requires::LIVE_METADATA,
        fix: Some(Applicability::Suggestion),
        summary: "A query targets a table with no known definition.",
        docs: "Needs undegraded metadata: a half-loaded schema makes every \
               table look undefined. A table the analyzer inferred from the \
               very statement that names it is reported only when it is used \
               once and a near-miss explicit table exists, which is what turns \
               this from dead code into typo detection.",
    },
    Rule {
        id: codes::UNKNOWN_INDEX_FIELD,
        category: Category::Schema,
        default_severity: DiagnosticSeverity::WARNING,
        requires: Requires::MODEL,
        fix: None,
        summary: "A DEFINE INDEX names a field the table does not declare.",
        docs: "The index is built over a field that has no `DEFINE FIELD` on \
               the table. On a `SCHEMAFULL` table that field can never hold a \
               value, so the index can never match anything.",
    },
    Rule {
        id: codes::UNKNOWN_ANALYZER,
        category: Category::Schema,
        default_severity: DiagnosticSeverity::WARNING,
        requires: Requires::MODEL,
        fix: None,
        summary: "A DEFINE INDEX names an analyzer nothing defines.",
        docs: "A full-text index refers to an analyzer by name. Nothing \
               validated that the name existed, so a typo produced an index \
               that could not be built.",
    },
    Rule {
        id: codes::UNKNOWN_FUNCTION,
        category: Category::Types,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::MODEL,
        fix: Some(Applicability::Suggestion),
        summary: "A call to a function that does not exist.",
        docs: "The name is neither in the builtin catalogue nor defined \
               anywhere in the workspace. Needs the merged model: a real \
               function defined in a file the server has not read is not an \
               unknown function, so this rule stands down when the workspace \
               is unavailable.",
    },
    Rule {
        id: codes::RELATION_ENDPOINT,
        category: Category::Schema,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::MODEL,
        fix: None,
        summary: "A RELATE points at a table the edge does not declare.",
        docs: "The edge table declares `TYPE RELATION IN … OUT …` and marks it \
               `ENFORCED`, which means the engine itself refuses a row outside \
               those lists. Only reported for an enforced relation: without \
               `ENFORCED` the declaration is documentation, not a constraint.",
    },
    Rule {
        id: codes::UNUSED_BINDING,
        category: Category::Types,
        default_severity: DiagnosticSeverity::HINT,
        requires: Requires::MODEL,
        fix: None,
        summary: "A binding nothing reads.",
        docs: "A `LET` whose value is never used, or a `DEFINE PARAM` or \
               `DEFINE FUNCTION` nothing in the workspace calls. Reported as a \
               hint and tagged `Unnecessary`, so an editor greys it out rather \
               than adding a warning to the problems panel. Needs the merged \
               model: a function called from a file the server has not read is \
               not unused.",
    },
    Rule {
        id: codes::UNUSED_SUPPRESSION,
        category: Category::Syntax,
        default_severity: DiagnosticSeverity::HINT,
        requires: Requires::TREE,
        fix: None,
        summary: "A suppression directive that silenced nothing.",
        docs: "The rule the directive names did not report where the directive \
               sits. Either the fault was fixed and the comment outlived it, or \
               the directive is on the wrong line. Reported as a hint and tagged \
               `Unnecessary`, so an editor greys it out — a stale suppression is \
               worth removing, not worth interrupting for. Silence it for a \
               file that keeps directives deliberately with \
               `-- surql-ignore-file: unused-suppression`; a line directive \
               cannot cover it, because a line directive's scope is the next \
               line of code rather than the line it sits on.",
    },
    Rule {
        id: codes::UNKNOWN_METHOD,
        category: Category::Types,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::MODEL,
        fix: None,
        summary: "A method the receiver's type does not have.",
        docs: "Such as `\"abc\".nonsense()`. Reported only when the receiver's \
               type is certain. An object receiver is exempt, because the \
               engine falls back to a closure-valued field there.",
    },
    Rule {
        id: codes::UNKNOWN_TYPE,
        category: Category::Syntax,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::TREE,
        fix: Some(Applicability::Suggestion),
        summary: "A type position holds a word SurrealDB's kind grammar does not have.",
        docs: "Such as `LET $x: xxx = 2`. Unlike the other judgements this is a \
               *syntax* fault: the engine refuses to parse it at all, so the \
               query never runs. It is therefore reported from the syntax pass \
               and is not gated by `analysis.enableTypeChecking`.",
    },
    Rule {
        id: codes::PARSE,
        category: Category::Syntax,
        default_severity: DiagnosticSeverity::ERROR,
        requires: Requires::TREE,
        fix: None,
        summary: "The grammar could not parse this text.",
        docs: "Covers both a missing token and an unparseable region. A \
               multi-line error region is clamped to its first line, with the \
               full extent attached as related information, so one typo does \
               not smear a squiggle over the rest of the file.",
    },
];

/// What the language server has available to every rule.
///
/// Also the gate an analysis pass checks before doing work. A pass must gate on
/// the *most permissive* environment, never on the caller's: skipping work
/// because this caller cannot use it would drop a diagnostic another caller
/// could. Trimming the output to the caller's real environment is
/// [`RuleSet::apply`]'s job, and it runs last.
pub const SERVER_ENVIRONMENT: Requires = Requires::LIVE_METADATA.union(Requires::AUTH_CONTEXT);

/// Every rule `infer::type_diagnostics` can emit. The whole pass returns early
/// when none of them is on, which keeps a disabled type check as cheap as it
/// was before the pass was broken into per-check gates.
pub const TYPE_PASS: &[RuleId] = &[
    codes::ARGUMENT_COUNT,
    codes::UNKNOWN_FUNCTION,
    codes::ARGUMENT_TYPE,
    codes::FIELD_TYPE,
    codes::LET_TYPE,
    codes::NOT_CALLABLE,
    codes::OPERATOR_TYPE,
    codes::RENAMED_FUNCTION,
    codes::RETURN_TYPE,
    codes::UNDEFINED_VARIABLE,
    codes::UNKNOWN_METHOD,
    codes::UNUSED_BINDING,
];

/// `check_calls`, which reaches builtin, method and user-function calls.
pub const CHECK_CALLS: &[RuleId] = &[
    codes::ARGUMENT_COUNT,
    codes::UNKNOWN_FUNCTION,
    codes::ARGUMENT_TYPE,
    codes::NOT_CALLABLE,
    codes::RENAMED_FUNCTION,
    codes::UNKNOWN_METHOD,
];
/// `check_let_annotations`.
pub const CHECK_LET_ANNOTATIONS: &[RuleId] = &[codes::LET_TYPE];
/// `check_field_clauses`.
pub const CHECK_FIELD_CLAUSES: &[RuleId] = &[codes::FIELD_TYPE];
/// `check_function_returns`.
pub const CHECK_FUNCTION_RETURNS: &[RuleId] = &[codes::RETURN_TYPE];
/// `check_variables`.
pub const CHECK_VARIABLES: &[RuleId] = &[codes::UNDEFINED_VARIABLE, codes::UNUSED_BINDING];
/// `check_binary_expressions`.
pub const CHECK_BINARY_EXPRESSIONS: &[RuleId] = &[codes::OPERATOR_TYPE];

/// The statement-target block in `MergedSemanticModel::semantic_diagnostics`.
pub const CHECK_TARGETS: &[RuleId] = &[codes::UNKNOWN_TABLE, codes::DYNAMIC_TARGET];
/// The touched-field block.
pub const CHECK_FIELDS: &[RuleId] = &[codes::UNKNOWN_FIELD];
/// The `DEFINE INDEX` consistency block.
pub const CHECK_INDEXES: &[RuleId] = &[codes::UNKNOWN_INDEX_FIELD, codes::UNKNOWN_ANALYZER];
/// The duplicate-definition sweep.
pub const CHECK_DUPLICATES: &[RuleId] = &[codes::DUPLICATE_DEFINITION];
/// The `RELATE` endpoint check.
pub const CHECK_RELATIONS: &[RuleId] = &[codes::RELATION_ENDPOINT];
/// The unused-binding sweep.
pub const CHECK_UNUSED: &[RuleId] = &[codes::UNUSED_BINDING];
/// The permission block.
pub const CHECK_PERMISSIONS: &[RuleId] = &[codes::PERMISSION_DENIED, codes::PERMISSION_UNKNOWN];

/// Where the generated rule pages live.
///
/// A client that supports `codeDescription` turns the code in the problems
/// panel into a link, which is the difference between a user seeing
/// `unknown-table` and a user learning why it fired.
const DOCS_BASE: &str =
    "https://github.com/surrealdb/surrealql-language-server/blob/main/docs/rules.md";

/// The documentation link for one rule.
pub fn docs_url(id: &str) -> String {
    format!("{DOCS_BASE}#{id}")
}

/// The severity names `analysis.ruleSeverity` accepts, plus `off`.
///
/// `off` is not an LSP severity — it is the absence of one. Keeping it in the
/// same vocabulary means a user turns a rule down and turns it off through one
/// setting rather than two.
pub const ACCEPTED_RULE_SEVERITIES: &[&str] = &["off", "hint", "info", "warning", "error"];

/// Parse one `analysis.ruleSeverity` value.
///
/// `Some(None)` is `off`. `None` is a value outside the vocabulary, which
/// `ServerSettings::validate_and_repair` drops with a warning rather than
/// guessing at.
pub fn parse_severity(value: &str) -> Option<Option<DiagnosticSeverity>> {
    match value.to_ascii_lowercase().as_str() {
        "off" => Some(None),
        "hint" => Some(Some(DiagnosticSeverity::HINT)),
        "info" | "information" => Some(Some(DiagnosticSeverity::INFORMATION)),
        "warning" | "warn" => Some(Some(DiagnosticSeverity::WARNING)),
        "error" => Some(Some(DiagnosticSeverity::ERROR)),
        _ => None,
    }
}

/// The effective severity of every rule for one analysis run.
///
/// Resolved once at the top of the pipeline and applied once at the end.
/// Fixed-size and cheap to build, so no caller has to thread it through a
/// changed signature to avoid the cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSet {
    /// Indexed by position in [`RULES`]. `None` means the rule is off.
    severity: Vec<Option<DiagnosticSeverity>>,
}

impl RuleSet {
    /// Resolve from the settings and from what this run actually has.
    ///
    /// Precedence, lowest first:
    ///
    /// 1. [`Rule::default_severity`].
    /// 2. The coarse booleans `analysis.enableTypeChecking` and
    ///    `analysis.enablePermissionAnalysis`, which turn a whole category off.
    /// 3. `analysis.ruleSeverity`, which wins over both — including turning a
    ///    single rule back *on* that a boolean turned off. Any order here is
    ///    defensible; an order nobody wrote down is not, so it is written down
    ///    in `README.md` as well.
    /// 4. [`Rule::requires`]. A rule whose inputs this run does not have is off
    ///    whatever the settings say, because it cannot reach a sound answer.
    pub fn resolve(settings: &ServerSettings, available: Requires) -> Self {
        let severity = RULES
            .iter()
            .map(|rule| {
                let mut effective = Some(rule.default_severity);

                let category_disabled = match rule.category {
                    Category::Types => !settings.analysis.enable_type_checking,
                    Category::Permissions => !settings.analysis.enable_permission_analysis,
                    Category::Syntax | Category::Schema => false,
                };
                if category_disabled {
                    effective = None;
                }

                if let Some(configured) = settings.analysis.rule_severity.get(rule.id)
                    && let Some(parsed) = parse_severity(configured)
                {
                    effective = parsed;
                }

                if !available.satisfies(rule.requires) {
                    effective = None;
                }

                effective
            })
            .collect();
        Self { severity }
    }

    /// Every rule at its default severity, with everything available. The
    /// resolution a caller gets from `ServerSettings::default()` on the server.
    pub fn permissive() -> Self {
        Self {
            severity: RULES
                .iter()
                .map(|rule| Some(rule.default_severity))
                .collect(),
        }
    }

    pub fn severity(&self, id: RuleId) -> Option<DiagnosticSeverity> {
        RULES
            .iter()
            .position(|rule| rule.id == id)
            .and_then(|index| self.severity[index])
    }

    pub fn is_enabled(&self, id: RuleId) -> bool {
        self.severity(id).is_some()
    }

    /// True when at least one of `ids` is enabled — the gate a whole analysis
    /// pass checks before doing any work.
    pub fn any_enabled(&self, ids: &[RuleId]) -> bool {
        ids.iter().any(|id| self.is_enabled(id))
    }

    /// Re-stamp each diagnostic's severity and drop the ones whose rule is off.
    ///
    /// The single choke point. It runs last in
    /// [`crate::semantic::pipeline::diagnostics_for_document`], so no emitter
    /// can route around it.
    ///
    /// A diagnostic carrying a code with no registered rule is left exactly as
    /// it is rather than dropped. Dropping it would make an unregistered code
    /// invisible instead of loud, and `every_code_has_a_rule` already fails the
    /// build in that case.
    pub fn apply(&self, diagnostics: &mut Vec<Diagnostic>) {
        diagnostics.retain_mut(|diagnostic| {
            let Some(NumberOrString::String(code)) = &diagnostic.code else {
                return true;
            };
            let Some(index) = RULES.iter().position(|rule| rule.id == code.as_str()) else {
                return true;
            };
            match self.severity[index] {
                Some(severity) => {
                    diagnostic.severity = Some(severity);
                    // Attached here rather than at each emission site: this is
                    // the one place every diagnostic passes through, so no new
                    // rule can forget it.
                    if diagnostic.code_description.is_none()
                        && let Ok(href) = docs_url(RULES[index].id).parse::<Uri>()
                    {
                        diagnostic.code_description = Some(CodeDescription { href });
                    }
                    true
                }
                None => false,
            }
        });
    }
}

/// The rule with this id, if it is registered.
pub fn rule(id: &str) -> Option<&'static Rule> {
    RULES.iter().find(|rule| rule.id == id)
}

/// Every registered rule id.
pub fn ids() -> impl Iterator<Item = RuleId> {
    RULES.iter().map(|rule| rule.id)
}

/// The rule catalogue as Markdown — the page `codeDescription` links into.
///
/// Generated from [`RULES`] rather than hand-written, and checked against the
/// committed `docs/rules.md` by a test, so a new rule cannot ship undocumented
/// and the page cannot drift from the code. That check runs everywhere, unlike
/// the builtin-catalogue freshness check, which needs a SurrealDB checkout.
pub fn catalogue_markdown() -> String {
    let mut out = String::new();
    out.push_str("# SurrealQL diagnostic rules\n\n");
    out.push_str(
        "<!-- Generated from `src/semantic/rules.rs`. Do not edit by hand -- \
         `rule_catalogue_is_in_sync` prints the replacement when it drifts. -->\n\n",
    );
    out.push_str(
        "Every diagnostic carries a stable `code`. Set its severity with \
         `analysis.ruleSeverity`, or silence it in place with \
         `-- surql-ignore: <code>`. Both are described in `README.md`.\n\n",
    );
    out.push_str("| Rule | Category | Default | Fix |\n| --- | --- | --- | --- |\n");
    for rule in RULES {
        out.push_str(&format!(
            "| [`{}`](#{}) | {} | {} | {} |\n",
            rule.id,
            rule.id,
            format!("{:?}", rule.category).to_lowercase(),
            severity_name(rule.default_severity),
            match rule.fix {
                Some(Applicability::MachineApplicable) => "automatic",
                Some(Applicability::Suggestion) => "suggested",
                None => "—",
            },
        ));
    }
    for rule in RULES {
        out.push_str(&format!(
            "\n## {}\n\n**{}**\n\n- Category: {}\n- Default severity: {}\n- Fix: {}\n\n{}\n",
            rule.id,
            rule.summary,
            format!("{:?}", rule.category).to_lowercase(),
            severity_name(rule.default_severity),
            match rule.fix {
                Some(Applicability::MachineApplicable) =>
                    "offered, and safe to apply automatically",
                Some(Applicability::Suggestion) => "offered; review it before applying",
                None => "none",
            },
            rule.docs,
        ));
    }
    out
}

fn severity_name(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::ERROR => "error",
        DiagnosticSeverity::WARNING => "warning",
        DiagnosticSeverity::INFORMATION => "info",
        DiagnosticSeverity::HINT => "hint",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code in the wire registry has a rule. Without this, adding a code
    /// and forgetting the rule would silently produce a diagnostic that no
    /// severity map, suppression filter or catalogue page knows about.
    #[test]
    fn every_code_has_a_rule() {
        for code in ALL_CODES {
            assert!(
                rule(code).is_some(),
                "`{code}` is in `codes.rs` but has no entry in `RULES`"
            );
        }
    }

    /// And the other direction, so a removed code cannot leave a stale rule
    /// advertising a diagnostic nothing emits.
    #[test]
    fn every_rule_has_a_code() {
        for id in ids() {
            assert!(
                ALL_CODES.contains(&id),
                "`{id}` is in `RULES` but is not a constant in `codes.rs`"
            );
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut seen: Vec<RuleId> = ids().collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(before, seen.len(), "`RULES` holds a duplicate id");
    }

    /// The two counts the severity column has to add up to. A miscount here is
    /// the cheapest possible signal that a transcription slipped.
    #[test]
    fn thirteen_rules_report_as_errors() {
        let errors = RULES
            .iter()
            .filter(|rule| rule.default_severity == DiagnosticSeverity::ERROR)
            .count();
        assert_eq!(errors, 13);
    }

    #[test]
    fn nine_rules_report_as_warnings() {
        let warnings: Vec<RuleId> = RULES
            .iter()
            .filter(|rule| rule.default_severity == DiagnosticSeverity::WARNING)
            .map(|rule| rule.id)
            .collect();
        assert_eq!(
            warnings,
            vec![
                codes::DUPLICATE_DEFINITION,
                codes::DYNAMIC_TARGET,
                codes::NOT_CALLABLE,
                codes::PERMISSION_UNKNOWN,
                codes::RENAMED_FUNCTION,
                codes::UNKNOWN_FIELD,
                codes::UNKNOWN_TABLE,
                codes::UNKNOWN_INDEX_FIELD,
                codes::UNKNOWN_ANALYZER,
            ]
        );
    }

    /// `Requires` is only useful if the cumulative constants really are
    /// cumulative. A rule needing `TREE` must run wherever a model is present.
    #[test]
    fn richer_environments_satisfy_poorer_requirements() {
        assert!(Requires::MODEL.satisfies(Requires::TREE));
        assert!(Requires::LIVE_METADATA.satisfies(Requires::MODEL));
        assert!(Requires::LIVE_METADATA.satisfies(Requires::TREE));
        assert!(Requires::AUTH_CONTEXT.satisfies(Requires::MODEL));
    }

    #[test]
    fn poorer_environments_do_not_satisfy_richer_requirements() {
        assert!(!Requires::TREE.satisfies(Requires::MODEL));
        assert!(!Requires::MODEL.satisfies(Requires::LIVE_METADATA));
        assert!(!Requires::MODEL.satisfies(Requires::AUTH_CONTEXT));
        // The two richer flags are siblings, not a chain: a run with live
        // metadata has not thereby been given an auth context.
        assert!(!Requires::LIVE_METADATA.satisfies(Requires::AUTH_CONTEXT));
        assert!(!Requires::AUTH_CONTEXT.satisfies(Requires::LIVE_METADATA));
    }

    /// Which is why a caller states what it has by unioning.
    #[test]
    fn a_union_satisfies_both_halves() {
        let server = Requires::LIVE_METADATA.union(Requires::AUTH_CONTEXT);
        assert!(server.satisfies(Requires::LIVE_METADATA));
        assert!(server.satisfies(Requires::AUTH_CONTEXT));
        assert!(server.satisfies(Requires::MODEL));
        assert!(server.satisfies(Requires::TREE));
    }

    /// Exactly the three diagnostics `MergedSemanticModel::code_actions`
    /// produces a fix for today. If a fourth producer appears, this fails until
    /// its rule declares the fix.
    #[test]
    fn four_rules_offer_a_fix() {
        let fixable: Vec<(RuleId, Applicability)> = RULES
            .iter()
            .filter_map(|rule| rule.fix.map(|fix| (rule.id, fix)))
            .collect();
        assert_eq!(
            fixable,
            vec![
                (codes::RENAMED_FUNCTION, Applicability::MachineApplicable),
                (codes::UNKNOWN_TABLE, Applicability::Suggestion),
                (codes::UNKNOWN_FUNCTION, Applicability::Suggestion),
                (codes::UNKNOWN_TYPE, Applicability::Suggestion),
            ]
        );
    }

    /// A fix that rewrites a name out of the engine's own rename table is the
    /// only one that is safe unattended. Everything else is a spelling guess.
    #[test]
    fn only_the_engine_backed_fix_is_machine_applicable() {
        for rule in RULES {
            if rule.fix == Some(Applicability::MachineApplicable) {
                assert_eq!(rule.id, codes::RENAMED_FUNCTION);
            }
        }
    }

    #[test]
    fn every_rule_documents_itself() {
        for rule in RULES {
            assert!(!rule.summary.is_empty(), "`{}` has no summary", rule.id);
            assert!(!rule.docs.is_empty(), "`{}` has no docs", rule.id);
            assert!(
                rule.summary.ends_with('.'),
                "`{}` summary must be a sentence",
                rule.id
            );
        }
    }

    /// The wire codes, listed once so the two drift tests can run in both
    /// directions. Kept here rather than in `codes.rs` because it exists to
    /// check that module, and a list that lives beside what it checks tends to
    /// be updated in the same edit that breaks it.
    const ALL_CODES: &[&str] = &[
        codes::ARGUMENT_COUNT,
        codes::DUPLICATE_DEFINITION,
        codes::UNKNOWN_FUNCTION,
        codes::UNKNOWN_INDEX_FIELD,
        codes::UNKNOWN_ANALYZER,
        codes::UNUSED_SUPPRESSION,
        codes::RELATION_ENDPOINT,
        codes::UNUSED_BINDING,
        codes::ARGUMENT_TYPE,
        codes::DYNAMIC_TARGET,
        codes::FIELD_TYPE,
        codes::LET_TYPE,
        codes::NOT_CALLABLE,
        codes::OPERATOR_TYPE,
        codes::PARSE,
        codes::PERMISSION_DENIED,
        codes::PERMISSION_UNKNOWN,
        codes::RENAMED_FUNCTION,
        codes::RETURN_TYPE,
        codes::UNDEFINED_VARIABLE,
        codes::UNKNOWN_FIELD,
        codes::UNKNOWN_METHOD,
        codes::UNKNOWN_TABLE,
        codes::UNKNOWN_TYPE,
    ];

    /// The pass list must be exactly the `Types` category, or the whole-pass
    /// early-out in `infer::type_diagnostics` skips a rule that is still on.
    #[test]
    fn the_type_pass_list_is_the_types_category() {
        let mut from_registry: Vec<RuleId> = RULES
            .iter()
            .filter(|rule| rule.category == Category::Types)
            .map(|rule| rule.id)
            .collect();
        from_registry.sort_unstable();
        let mut listed = TYPE_PASS.to_vec();
        listed.sort_unstable();
        assert_eq!(listed, from_registry);
    }

    /// Every per-check list together must cover the pass list exactly. A code
    /// in the pass but in no check would never be gated; a code in a check but
    /// not the pass would be gated by a pass that cannot emit it.
    #[test]
    fn the_per_check_lists_partition_the_type_pass() {
        let mut union: Vec<RuleId> = [
            CHECK_CALLS,
            CHECK_LET_ANNOTATIONS,
            CHECK_FIELD_CLAUSES,
            CHECK_FUNCTION_RETURNS,
            CHECK_VARIABLES,
            CHECK_BINARY_EXPRESSIONS,
        ]
        .concat();
        let before = union.len();
        union.sort_unstable();
        union.dedup();
        assert_eq!(before, union.len(), "a code is claimed by two checks");
        let mut pass = TYPE_PASS.to_vec();
        pass.sort_unstable();
        assert_eq!(union, pass);
    }

    /// And the model-side lists must cover the two non-type categories.
    #[test]
    fn the_model_check_lists_cover_schema_and_permissions() {
        let mut union: Vec<RuleId> = [
            CHECK_TARGETS,
            CHECK_FIELDS,
            CHECK_PERMISSIONS,
            CHECK_INDEXES,
            CHECK_DUPLICATES,
            CHECK_RELATIONS,
        ]
        .concat();
        union.sort_unstable();
        let mut from_registry: Vec<RuleId> = RULES
            .iter()
            .filter(|rule| matches!(rule.category, Category::Schema | Category::Permissions))
            .map(|rule| rule.id)
            .collect();
        from_registry.sort_unstable();
        assert_eq!(union, from_registry);
    }

    /// The committed catalogue matches the registry. Regenerate with
    /// `cargo run --bin surrealql-language-server -- rules --markdown`? No —
    /// simpler: the failure message prints the file to write.
    #[test]
    fn rule_catalogue_is_in_sync() {
        let generated = catalogue_markdown();
        let committed =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/rules.md"))
                .unwrap_or_default();
        assert_eq!(
            committed.trim_end(),
            generated.trim_end(),
            "docs/rules.md is stale. Write this to it:\n\n{generated}"
        );
    }

    #[test]
    fn every_rule_has_a_documentation_anchor() {
        let catalogue = catalogue_markdown();
        for id in ids() {
            assert!(
                catalogue.contains(&format!("\n## {id}\n")),
                "`{id}` has no section in the catalogue"
            );
        }
    }

    #[test]
    fn the_code_list_is_complete() {
        assert_eq!(ALL_CODES.len(), 24);
        assert_eq!(RULES.len(), 24);
    }
}
