use std::fs;
use std::path::{Path, PathBuf};

use zed::serde_json::Value;
use zed::settings::LspSettings;
use zed::{Command, LanguageServerId};
use zed_extension_api as zed;

const LANGUAGE_SERVER_ID: &str = "surreal-language-server";
const LANGUAGE_SERVER_BINARY: &str = "surreal-language-server";

struct SurrealExtension;

impl zed::Extension for SurrealExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<Command> {
        if language_server_id.as_ref() != LANGUAGE_SERVER_ID {
            return Err(format!(
                "unrecognized language server for SurrealQL: {language_server_id}"
            ));
        }

        let lsp_settings =
            LspSettings::for_worktree(LANGUAGE_SERVER_ID, worktree).unwrap_or_default();
        let command = lsp_settings
            .binary
            .as_ref()
            .and_then(|binary| binary.path.clone())
            .or_else(|| resolve_default_binary(worktree))
            .ok_or_else(|| missing_binary_message(worktree))?;
        let args = lsp_settings
            .binary
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_default();
        let env = lsp_settings
            .binary
            .as_ref()
            .and_then(|binary| binary.env.clone())
            .map(|env| env.into_iter().collect())
            .unwrap_or_default();

        Ok(Command { command, args, env })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<Option<Value>> {
        if language_server_id.as_ref() != LANGUAGE_SERVER_ID {
            return Ok(None);
        }

        Ok(LspSettings::for_worktree(LANGUAGE_SERVER_ID, worktree)
            .ok()
            .and_then(|settings| settings.initialization_options))
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<Option<Value>> {
        if language_server_id.as_ref() != LANGUAGE_SERVER_ID {
            return Ok(None);
        }

        Ok(LspSettings::for_worktree(LANGUAGE_SERVER_ID, worktree)
            .ok()
            .and_then(|settings| settings.settings))
    }
}

fn resolve_default_binary(worktree: &zed::Worktree) -> Option<String> {
    let root = PathBuf::from(worktree.root_path());
    for candidate in [
        root.join("target/release").join(LANGUAGE_SERVER_BINARY),
        root.join("target/debug").join(LANGUAGE_SERVER_BINARY),
    ] {
        if is_executable(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }

    if let Some(path) = worktree.which(LANGUAGE_SERVER_BINARY) {
        return Some(path);
    }

    let shell_env = worktree.shell_env();
    if let Some(path_value) = shell_env
        .iter()
        .find_map(|(key, value)| (key == "PATH").then_some(value))
    {
        for directory in path_value.split(':') {
            let candidate = Path::new(directory).join(LANGUAGE_SERVER_BINARY);
            if is_executable(&candidate) {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }

    shell_env
        .iter()
        .find_map(|(key, value)| (key == "HOME").then_some(value))
        .and_then(|home| {
            let candidate = Path::new(home)
                .join(".cargo/bin")
                .join(LANGUAGE_SERVER_BINARY);
            is_executable(&candidate).then(|| candidate.to_string_lossy().into_owned())
        })
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

fn missing_binary_message(worktree: &zed::Worktree) -> String {
    format!(
        "Unable to find `{}` for worktree `{}`. Set `lsp.{}.binary.path` in Zed settings, or install it globally with `cargo install --path /absolute/path/to/surreal-language-server/crates/language-server`.",
        LANGUAGE_SERVER_BINARY,
        worktree.root_path(),
        LANGUAGE_SERVER_ID
    )
}

zed::register_extension!(SurrealExtension);
