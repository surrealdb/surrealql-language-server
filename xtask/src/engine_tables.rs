//! The two engine tables that decide which names exist and where each one is
//! implemented.
//!
//! * `syn/parser/builtin.rs` holds `PATHS`, a `phf_map!` of every name the
//!   parser accepts. It is the authority on the spelling authors write, and its
//!   third tuple slot carries the previous spelling for the names that were
//!   renamed.
//! * `fnc/mod.rs` holds the `dispatch!` tables, which map that spelling to the
//!   implementation. A name in `PATHS` with no dispatch arm parses and then
//!   fails at run time.
//!
//! Both files list one entry per line, so these are line scans. The
//! *signatures* are the part that needs an abstract-syntax-tree parse, and
//! `crate::signatures` does that.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One `PATHS` entry.
#[derive(Debug, Clone)]
pub struct PathEntry {
    pub name: String,
    /// False for `PathKind::Constant` entries such as `math::PI`, which take no
    /// arguments and are not called.
    pub is_function: bool,
    /// The spelling this name replaced, when the engine records one.
    pub previous_name: Option<String>,
}

/// Parse `PATHS` out of `syn/parser/builtin.rs`.
pub fn parse_paths(builtin_rs: &Path) -> Result<Vec<PathEntry>, String> {
    let source = std::fs::read_to_string(builtin_rs)
        .map_err(|error| format!("cannot read {}: {error}", builtin_rs.display()))?;

    let mut entries = Vec::new();
    for line in source.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("UniCase::ascii(\"") else {
            continue;
        };
        let Some((name, rest)) = rest.split_once('"') else {
            continue;
        };
        if !rest.contains("=>") {
            continue;
        }
        let is_function = rest.contains("PathKind::Function");
        let is_constant = rest.contains("PathKind::Constant");
        if !is_function && !is_constant {
            continue;
        }
        entries.push(PathEntry {
            name: name.to_string(),
            is_function,
            previous_name: previous_name(rest),
        });
    }

    if entries.is_empty() {
        return Err(format!(
            "found no PATHS entries in {} — the engine's table shape changed",
            builtin_rs.display()
        ));
    }
    Ok(entries)
}

/// The `Some(UniCase::ascii("old::name"))` in an entry's third slot.
fn previous_name(rest: &str) -> Option<String> {
    let after = rest.split_once("Some(UniCase::ascii(\"")?.1;
    let (name, _) = after.split_once('"')?;
    Some(name.to_string())
}

/// The names that reach an implementation, and the Rust path each one reaches.
///
/// The value is the implementation path as written in the dispatch arm, with the
/// leading `crate::fnc::` stripped, so it lines up with
/// [`crate::signatures::Implementation::path`].
pub fn parse_dispatch(fnc_mod_rs: &Path) -> Result<BTreeMap<String, String>, String> {
    let source = std::fs::read_to_string(fnc_mod_rs)
        .map_err(|error| format!("cannot read {}: {error}", fnc_mod_rs.display()))?;

    let mut map = BTreeMap::new();
    for line in source.lines() {
        let trimmed = line.trim();
        // `"string::len" => string::len,`
        // `exp(Files) "file::put" => file::put((stk, ctx)).await,`
        let Some(rest) = trimmed.split_once('"') else {
            continue;
        };
        // Only the capability wrapper may precede the literal.
        let before = rest.0.trim();
        if !before.is_empty() && !before.starts_with("exp(") {
            continue;
        }
        let Some((name, after)) = rest.1.split_once('"') else {
            continue;
        };
        let Some(target) = after.trim().strip_prefix("=>") else {
            continue;
        };
        if !name.contains("::") && !is_bare_builtin(name) {
            continue;
        }
        let path = implementation_path(target);
        if path.is_empty() {
            continue;
        }
        map.entry(name.to_string()).or_insert(path);
    }

    if map.is_empty() {
        return Err(format!(
            "found no dispatch arms in {} — the macro shape changed",
            fnc_mod_rs.display()
        ));
    }
    Ok(map)
}

/// `count`, `not`, `rand` and `sleep` are callable without a namespace.
fn is_bare_builtin(name: &str) -> bool {
    matches!(name, "count" | "not" | "rand" | "sleep")
}

/// The leading path of a dispatch arm's target expression.
///
/// `array::all((stk, ctx, opt, doc)).await,` → `array::all`
/// `(cpu_intensive) crypto::argon2::compare,` → `crypto::argon2::compare`
/// `r#type::array,` → `type::array`
///
/// Raw identifiers matter: `type`, `enum` and `gen` are Rust keywords, so the
/// engine writes `r#type::array`, `rand::r#enum` and `crypto::argon2::r#gen`.
/// Dropping the `r#` here is what lets these line up with the module paths
/// `crate::signatures` reports — 52 functions, the whole `type::` namespace
/// among them, otherwise go unmatched.
fn implementation_path(target: &str) -> String {
    let target = target.trim();
    // Strip a leading wrapper such as `(cpu_intensive)`.
    let target = match target.strip_prefix('(') {
        Some(rest) => rest.split_once(')').map(|(_, tail)| tail).unwrap_or(rest),
        None => target,
    };
    let mut path = String::new();
    for ch in target.trim().chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == ':' || ch == '#' {
            path.push(ch);
        } else {
            break;
        }
    }
    let path = path
        .trim_end_matches(':')
        .trim_start_matches("crate::fnc::");
    path.split("::")
        .map(strip_raw)
        .collect::<Vec<_>>()
        .join("::")
}

/// `r#type` → `type`.
pub fn strip_raw(segment: &str) -> &str {
    segment.strip_prefix("r#").unwrap_or(segment)
}

/// The namespaces that really exist, derived from the names themselves.
///
/// The hand-written list in the language server advertises `not::` and
/// `sleep::`, but `not`, `sleep`, `count` and `rand` are bare functions — there
/// is no such namespace, so offering the prefix is a wrong answer.
pub fn namespaces(entries: &[PathEntry]) -> BTreeSet<String> {
    entries
        .iter()
        .filter_map(|entry| entry.name.split_once("::"))
        .map(|(namespace, _)| format!("{namespace}::"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_function_entry_is_read() {
        let entries =
            parse_from_str("UniCase::ascii(\"array::at\") => (PathKind::Function, None),");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "array::at");
        assert!(entries[0].is_function);
        assert_eq!(entries[0].previous_name, None);
    }

    #[test]
    fn a_constant_entry_is_marked_as_not_a_function() {
        let entries = parse_from_str(
            "UniCase::ascii(\"math::PI\") => (PathKind::Constant(Constant::MathPi), None),",
        );
        assert!(!entries[0].is_function);
    }

    #[test]
    fn a_renamed_entry_keeps_its_previous_spelling() {
        let entries = parse_from_str(
            "UniCase::ascii(\"type::record\") => (PathKind::Function, Some(UniCase::ascii(\"type::thing\"))),",
        );
        assert_eq!(entries[0].previous_name.as_deref(), Some("type::thing"));
    }

    fn parse_from_str(source: &str) -> Vec<PathEntry> {
        let dir = std::env::temp_dir().join(format!("xtask-paths-{}", source.len()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("builtin.rs");
        std::fs::write(&file, source).unwrap();
        parse_paths(&file).unwrap()
    }

    #[test]
    fn a_plain_dispatch_arm_maps_name_to_path() {
        assert_eq!(implementation_path("string::len,"), "string::len");
    }

    #[test]
    fn a_context_passing_arm_keeps_only_the_path() {
        assert_eq!(
            implementation_path("array::all((stk, ctx, opt, doc)).await,"),
            "array::all"
        );
    }

    #[test]
    fn a_wrapped_arm_strips_the_wrapper() {
        assert_eq!(
            implementation_path("(cpu_intensive) crypto::argon2::compare,"),
            "crypto::argon2::compare"
        );
    }

    #[test]
    fn namespaces_exclude_the_bare_functions() {
        let entries = vec![
            PathEntry {
                name: "array::at".into(),
                is_function: true,
                previous_name: None,
            },
            PathEntry {
                name: "count".into(),
                is_function: true,
                previous_name: None,
            },
        ];
        let found = namespaces(&entries);
        assert!(found.contains("array::"));
        assert_eq!(found.len(), 1, "`count` has no namespace: {found:?}");
    }
}
