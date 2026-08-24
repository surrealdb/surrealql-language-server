//! `surrealql.toml` — the project configuration file.
//!
//! Lets a team commit one analysis policy to the repository, so the editor and
//! a build pipeline read the same rules instead of each being configured
//! separately.
//!
//! The file is converted to JSON here and merged as JSON by
//! [`crate::config::ServerSettings::from_sources_with_project`]. That keeps the
//! settings model itself target-agnostic: `toml` is a native-only dependency,
//! and the shared dependency block is also the `wasm32` dependency graph.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Accepted file names, in the order they are tried.
pub const PROJECT_CONFIG_FILENAMES: &[&str] = &["surrealql.toml", ".surrealql.toml"];

/// What a project-configuration lookup found.
#[derive(Debug, Default, Clone)]
pub struct ProjectConfig {
    /// The file that was read, if one was.
    pub path: Option<PathBuf>,
    /// Its contents as JSON, ready to merge under the LSP settings.
    pub value: Option<Value>,
    /// Anything the user needs to know: a malformed file, or a file that
    /// could not be read.
    pub warnings: Vec<String>,
}

/// Find and parse the project configuration for these workspace folders.
///
/// Only the first folder is consulted. A multi-root workspace with a different
/// policy per root would need a per-document lookup, and nothing asks for that
/// yet; picking one root and saying so beats merging several silently.
pub fn discover(folders: &[PathBuf]) -> ProjectConfig {
    let Some(root) = folders.first() else {
        return ProjectConfig::default();
    };
    for name in PROJECT_CONFIG_FILENAMES {
        let candidate = root.join(name);
        if !candidate.is_file() {
            continue;
        }
        return read(&candidate);
    }
    ProjectConfig::default()
}

fn read(path: &Path) -> ProjectConfig {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            return ProjectConfig {
                path: Some(path.to_path_buf()),
                value: None,
                warnings: vec![format!("could not read {}: {error}", path.display())],
            };
        }
    };
    // The whole file is refused on a parse error rather than partially applied.
    // A half-read policy is worse than none: nobody can tell which half is in
    // force.
    match toml::from_str::<Value>(&text) {
        Ok(value) => ProjectConfig {
            path: Some(path.to_path_buf()),
            value: Some(value),
            warnings: Vec::new(),
        },
        Err(error) => ProjectConfig {
            path: Some(path.to_path_buf()),
            value: None,
            warnings: vec![format!(
                "{} is not valid TOML and was ignored: {error}",
                path.display()
            )],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("write");
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("surrealql-project-config-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create");
        dir
    }

    #[test]
    fn no_folder_yields_nothing() {
        let found = discover(&[]);
        assert!(found.value.is_none());
        assert!(found.warnings.is_empty());
    }

    #[test]
    fn a_missing_file_is_not_a_warning() {
        let dir = temp_dir("missing");
        let found = discover(&[dir]);
        assert!(found.value.is_none());
        assert!(found.warnings.is_empty());
    }

    #[test]
    fn a_toml_file_becomes_json() {
        let dir = temp_dir("valid");
        write(
            &dir,
            "surrealql.toml",
            "[analysis]\nschemalessDiagnostics = \"errors\"\n\n[analysis.ruleSeverity]\nlet-type = \"off\"\n",
        );
        let found = discover(&[dir]);
        let value = found.value.expect("parsed");
        assert_eq!(value["analysis"]["schemalessDiagnostics"], "errors");
        assert_eq!(value["analysis"]["ruleSeverity"]["let-type"], "off");
        assert!(found.warnings.is_empty());
    }

    /// A malformed file is refused whole. Applying the half that parsed would
    /// leave nobody able to say which policy is in force.
    #[test]
    fn a_malformed_file_is_refused_with_a_warning() {
        let dir = temp_dir("malformed");
        write(&dir, "surrealql.toml", "[analysis\nbroken");
        let found = discover(&[dir]);
        assert!(found.value.is_none());
        assert_eq!(found.warnings.len(), 1);
        assert!(found.warnings[0].contains("not valid TOML"));
    }

    #[test]
    fn the_dotted_name_is_a_fallback() {
        let dir = temp_dir("dotted");
        write(
            &dir,
            ".surrealql.toml",
            "[analysis]\nenableTypeChecking = false\n",
        );
        let found = discover(std::slice::from_ref(&dir));
        assert_eq!(
            found.value.expect("parsed")["analysis"]["enableTypeChecking"],
            false
        );
        assert_eq!(found.path, Some(dir.join(".surrealql.toml")));
    }

    #[test]
    fn the_undotted_name_wins_when_both_exist() {
        let dir = temp_dir("both");
        write(
            &dir,
            "surrealql.toml",
            "[analysis]\nenableTypeChecking = true\n",
        );
        write(
            &dir,
            ".surrealql.toml",
            "[analysis]\nenableTypeChecking = false\n",
        );
        let found = discover(std::slice::from_ref(&dir));
        assert_eq!(found.path, Some(dir.join("surrealql.toml")));
    }
}
