//! Rendering: the rule catalogue, and one document's findings.

use std::process::ExitCode;

use ls_types::{Diagnostic, DiagnosticSeverity, NumberOrString};

use crate::semantic::rules::{self, Applicability};

use super::{EXIT_OK, EXIT_USAGE};

/// `rules` — every registered rule, one per line.
pub fn list_rules() -> ExitCode {
    let width = rules::RULES
        .iter()
        .map(|rule| rule.id.len())
        .max()
        .unwrap_or(0);
    for rule in rules::RULES {
        println!(
            "{:<width$}  {:<12} {:<8} {}",
            rule.id,
            format!("{:?}", rule.category).to_lowercase(),
            severity_name(rule.default_severity),
            rule.summary,
        );
    }
    ExitCode::from(EXIT_OK)
}

/// `explain <rule>` — the long-form description.
pub fn explain(id: &str) -> ExitCode {
    let Some(rule) = rules::rule(id) else {
        eprintln!("surrealql-language-server: no rule `{id}`.");
        if let Some(nearest) = nearest_rule(id) {
            eprintln!("Did you mean `{nearest}`?");
        }
        return ExitCode::from(EXIT_USAGE);
    };
    println!("{}", rule.id);
    println!();
    println!("  category  {:?}", rule.category);
    println!("  severity  {}", severity_name(rule.default_severity));
    println!(
        "  fix       {}",
        match rule.fix {
            Some(Applicability::MachineApplicable) => "yes, safe to apply automatically",
            Some(Applicability::Suggestion) => "yes, review before applying",
            None => "none",
        }
    );
    println!();
    println!("{}", rule.docs);
    ExitCode::from(EXIT_OK)
}

fn nearest_rule(id: &str) -> Option<&'static str> {
    rules::ids()
        .map(|known| (strsim::jaro_winkler(id, known), known))
        .filter(|(score, _)| *score >= 0.8)
        .max_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, known)| known)
}

pub fn severity_name(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::ERROR => "error",
        DiagnosticSeverity::WARNING => "warning",
        DiagnosticSeverity::INFORMATION => "info",
        DiagnosticSeverity::HINT => "hint",
        _ => "unknown",
    }
}

pub fn code_of(diagnostic: &Diagnostic) -> &str {
    match &diagnostic.code {
        Some(NumberOrString::String(code)) => code,
        _ => "",
    }
}

/// One finding, in the shape a compiler prints. Positions are 1-based here,
/// the way an editor shows them.
pub fn human_line(path: &str, diagnostic: &Diagnostic) -> String {
    format!(
        "{path}:{}:{}: {}[{}]: {}",
        diagnostic.range.start.line + 1,
        diagnostic.range.start.character + 1,
        severity_name(diagnostic.severity.unwrap_or(DiagnosticSeverity::ERROR)),
        code_of(diagnostic),
        diagnostic.message,
    )
}

/// One finding as JSON. Positions stay 0-based, matching the protocol — said
/// out loud in `--help` so nobody has to discover it from output.
pub fn json_value(path: &str, diagnostic: &Diagnostic) -> serde_json::Value {
    serde_json::json!({
        "file": path,
        "rule": code_of(diagnostic),
        "severity": severity_name(diagnostic.severity.unwrap_or(DiagnosticSeverity::ERROR)),
        "message": diagnostic.message,
        "range": {
            "start": {
                "line": diagnostic.range.start.line,
                "character": diagnostic.range.start.character,
            },
            "end": {
                "line": diagnostic.range.end.line,
                "character": diagnostic.range.end.character,
            },
        },
    })
}
