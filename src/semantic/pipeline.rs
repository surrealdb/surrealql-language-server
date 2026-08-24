//! The one place a document's complete diagnostic set is assembled.
//!
//! Every consumer goes through [`diagnostics_for_document`]: the LSP server,
//! the test suites, and — once it exists — the command-line mode. That is the
//! point of the module. The assembly used to live as a private function in
//! `src/core/server.rs`, which left the test suites re-implementing it by hand,
//! so a change to the real pipeline could pass a green test run while the
//! editor behaved differently.

use ls_types::{Diagnostic, NumberOrString};

use crate::config::ServerSettings;
use crate::semantic::rules::{Requires, RuleSet};

/// Re-exported so a caller reaching for the pipeline finds the environment
/// constant beside it. Defined in [`crate::semantic::rules`], which is also
/// where the analysis passes read it to gate their own work.
pub use crate::semantic::rules::SERVER_ENVIRONMENT;
use crate::semantic::types::{DocumentAnalysis, MergedSemanticModel};

/// The complete diagnostic set for one document: the syntax pass, then the
/// semantic and type passes, then the schema-mode filter over both.
///
/// The filter has to run last because `unknown-type` comes from the syntax
/// pass, which reads a single document and cannot see the merged model — see
/// [`MergedSemanticModel::apply_schemaless_policy`].
///
/// `model` and `settings` are parameters rather than state reads so callers
/// already holding a snapshot do not re-acquire the lock per document, and
/// cannot race a concurrent `recompute_model`.
pub fn diagnostics_for_document(
    analysis: &DocumentAnalysis,
    model: &MergedSemanticModel,
    settings: &ServerSettings,
) -> Vec<Diagnostic> {
    diagnostics_in(analysis, model, settings, SERVER_ENVIRONMENT)
}

/// [`diagnostics_for_document`] for a caller that has less than the server does.
///
/// The command-line mode runs without a database, so it passes
/// [`Requires::MODEL`]. That is what makes an offline run stand `unknown-table`
/// down instead of reporting every table in the file as undefined.
pub fn diagnostics_in(
    analysis: &DocumentAnalysis,
    model: &MergedSemanticModel,
    settings: &ServerSettings,
    available: Requires,
) -> Vec<Diagnostic> {
    let rules = RuleSet::resolve(settings, available);
    let mut diagnostics = analysis.syntax_diagnostics.clone();
    diagnostics.extend(model.semantic_diagnostics(analysis, settings));
    model.apply_schemaless_policy(&mut diagnostics, settings);
    // Last, so no emitter can route around the severity map or the per-rule
    // disable.
    rules.apply(&mut diagnostics);
    // Then the author's own directives. After the severity map, so a rule the
    // settings already silenced is never "suppressed" a second time, and a
    // suppressed diagnostic cannot be resurrected by anything downstream.
    if !analysis.suppressions.is_empty() {
        let mut used = vec![false; analysis.suppressions.entries().len()];
        diagnostics.retain(|diagnostic| {
            let Some(NumberOrString::String(code)) = &diagnostic.code else {
                return true;
            };
            match analysis
                .suppressions
                .matching(code, diagnostic.range.start.line)
            {
                Some(index) => {
                    used[index] = true;
                    false
                }
                None => true,
            }
        });
        // A directive that silenced nothing is either a comment that outlived
        // the fault it covered or one on the wrong line. Reported after the
        // filter, because until every diagnostic has been offered to every
        // directive there is no way to know which did work.
        if rules.is_enabled(crate::semantic::codes::UNUSED_SUPPRESSION) {
            for (entry, was_used) in analysis.suppressions.entries().iter().zip(used) {
                if was_used {
                    continue;
                }
                // Offered to the directives in turn, so
                // `-- surql-ignore-file: unused-suppression` silences the rule
                // for a file that keeps directives deliberately. A directive
                // cannot silence itself: its own scope is the next line of
                // code, not the line it sits on, so there is no paradox to
                // resolve here.
                if analysis.suppressions.suppresses(
                    crate::semantic::codes::UNUSED_SUPPRESSION,
                    entry.range.start.line,
                ) {
                    continue;
                }
                diagnostics.push(Diagnostic {
                    range: entry.range,
                    severity: rules
                        .severity(crate::semantic::codes::UNUSED_SUPPRESSION)
                        .or(Some(ls_types::DiagnosticSeverity::HINT)),
                    code: crate::semantic::codes::as_code(
                        crate::semantic::codes::UNUSED_SUPPRESSION,
                    ),
                    source: Some("surreal-language-server".to_string()),
                    message: match entry.rules.as_slice() {
                        [] => "This directive silenced nothing.".to_string(),
                        [one] => format!("`{one}` did not report here."),
                        many => format!("None of {} reported here.", many.join(", ")),
                    },
                    tags: Some(vec![ls_types::DiagnosticTag::UNNECESSARY]),
                    // Attached here rather than by `RuleSet::apply`: this
                    // diagnostic is emitted after that pass, because it can
                    // only be decided once every other diagnostic has been
                    // offered to every directive.
                    code_description: crate::semantic::rules::docs_url(
                        crate::semantic::codes::UNUSED_SUPPRESSION,
                    )
                    .parse()
                    .ok()
                    .map(|href| ls_types::CodeDescription { href }),
                    ..Diagnostic::default()
                });
            }
        }
    }
    diagnostics
}
