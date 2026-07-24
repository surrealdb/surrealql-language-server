use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Reads an environment variable when running natively. Returns `None`
/// on `wasm32-unknown-unknown` because the browser sandbox has no
/// environment to inspect — Surrealist supplies these values via the
/// LSP `initializationOptions` / `workspace/configuration` flow.
#[cfg(not(target_arch = "wasm32"))]
fn read_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[cfg(target_arch = "wasm32")]
fn read_env(_name: &str) -> Option<String> {
    None
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    #[serde(default)]
    pub connection: ConnectionSettings,
    #[serde(default)]
    pub metadata: MetadataSettings,
    #[serde(default)]
    pub analysis: AnalysisSettings,
    #[serde(default, alias = "auth_contexts")]
    pub auth_contexts: Vec<AuthContext>,
    #[serde(default, alias = "active_auth_context")]
    pub active_auth_context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ConnectionSettings {
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub access: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MetadataSettings {
    #[serde(default = "default_metadata_mode")]
    pub mode: String,
    #[serde(default = "default_true", alias = "enable_live_metadata")]
    pub enable_live_metadata: bool,
    #[serde(default = "default_true", alias = "refresh_on_save")]
    pub refresh_on_save: bool,
}

impl MetadataSettings {
    /// Returns true when the language server should scan local `.surql` workspace files.
    pub fn filesystem_enabled(&self) -> bool {
        matches!(
            self.mode.as_str(),
            "both" | "workspace+db" | "filesystem" | "workspace"
        )
    }

    /// Returns true when the language server should fetch schema from a remote SurrealDB.
    pub fn db_enabled(&self) -> bool {
        matches!(
            self.mode.as_str(),
            "both" | "workspace+db" | "db" | "remote"
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisSettings {
    #[serde(default = "default_true", alias = "enable_permission_analysis")]
    pub enable_permission_analysis: bool,
    #[serde(default = "default_true", alias = "enable_aggressive_schema_inference")]
    pub enable_aggressive_schema_inference: bool,
    #[serde(default = "default_true", alias = "enable_code_actions")]
    pub enable_code_actions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuthContext {
    pub name: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default, alias = "auth_record")]
    pub auth_record: Option<String>,
    #[serde(default)]
    pub claims: Value,
    #[serde(default)]
    pub session: Value,
    #[serde(default)]
    pub variables: Value,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            connection: ConnectionSettings::default(),
            metadata: MetadataSettings::default(),
            analysis: AnalysisSettings::default(),
            auth_contexts: vec![AuthContext::default()],
            active_auth_context: Some("viewer".to_string()),
        }
    }
}

impl Default for MetadataSettings {
    fn default() -> Self {
        Self {
            mode: default_metadata_mode(),
            enable_live_metadata: true,
            refresh_on_save: true,
        }
    }
}

impl Default for AnalysisSettings {
    fn default() -> Self {
        Self {
            enable_permission_analysis: true,
            enable_aggressive_schema_inference: true,
            enable_code_actions: true,
        }
    }
}

impl Default for AuthContext {
    fn default() -> Self {
        Self {
            name: "viewer".to_string(),
            roles: vec!["viewer".to_string()],
            auth_record: None,
            claims: Value::Object(Default::default()),
            session: Value::Object(Default::default()),
            variables: Value::Object(Default::default()),
        }
    }
}

/// The `metadata.mode` strings the server understands. Anything else
/// is repaired to the default with a warning instead of silently
/// disabling every schema source.
pub const ACCEPTED_METADATA_MODES: &[&str] = &[
    "both",
    "workspace+db",
    "filesystem",
    "workspace",
    "db",
    "remote",
];

impl ServerSettings {
    pub fn from_sources(
        initialization_options: Option<&Value>,
        configuration: Option<&Value>,
    ) -> Self {
        Self::from_sources_with_warnings(initialization_options, configuration).0
    }

    /// Like [`Self::from_sources`], but also returns human-readable
    /// warnings for every part of the payload that could not be used
    /// (malformed JSON shapes, unknown enum-like strings). Callers
    /// forward these to the client via `window/logMessage` so a typo
    /// in the editor settings is no longer a silent no-op.
    pub fn from_sources_with_warnings(
        initialization_options: Option<&Value>,
        configuration: Option<&Value>,
    ) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut settings = Self::default();
        let mut parsed_any = false;

        for (label, value) in [
            ("initializationOptions", initialization_options),
            ("workspace configuration", configuration),
        ] {
            let Some(value) = value else { continue };
            match parse_settings_value(value) {
                Ok(Some(parsed)) => {
                    settings = parsed.merge_with_env();
                    parsed_any = true;
                }
                Ok(None) => {}
                Err(error) => warnings.push(format!(
                    "invalid `surrealql` settings in {label}: {error}; the payload was ignored"
                )),
            }
        }

        // No usable payload (none given, `null` sections, or every
        // payload malformed): the SURREALDB_* environment fallbacks
        // must still apply, exactly as they did pre-0.3.
        if !parsed_any {
            settings = settings.merge_with_env();
        }

        if settings.auth_contexts.is_empty() {
            settings.auth_contexts.push(AuthContext::default());
        }

        if settings.active_auth_context.is_none() {
            settings.active_auth_context = settings
                .auth_contexts
                .first()
                .map(|context| context.name.clone());
        }

        warnings.extend(settings.validate_and_repair());

        (settings, warnings)
    }

    /// Repair unknown enum-like values back to safe defaults and
    /// describe each repair. Unknown `metadata.mode` previously turned
    /// off both the workspace scan *and* the live DB fetch with no
    /// feedback at all.
    fn validate_and_repair(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();

        if !ACCEPTED_METADATA_MODES.contains(&self.metadata.mode.as_str()) {
            // Don't promise a specific effective value here: a later
            // merge with in-flight settings may restore the previous
            // mode over the repaired default.
            warnings.push(format!(
                "unknown metadata.mode `{}` was ignored (accepted values: {})",
                self.metadata.mode,
                ACCEPTED_METADATA_MODES.join(", "),
            ));
            self.metadata.mode = default_metadata_mode();
        }

        if let Some(active) = &self.active_auth_context {
            let known = self
                .auth_contexts
                .iter()
                .any(|context| &context.name == active);
            if !known {
                let fallback = self
                    .auth_contexts
                    .first()
                    .map(|context| context.name.as_str())
                    .unwrap_or("<none>");
                warnings.push(format!(
                    "activeAuthContext `{active}` does not match any configured auth context; \
                     using `{fallback}` instead"
                ));
            }
        }

        warnings
    }

    pub fn merge_with_env(mut self) -> Self {
        self.connection.endpoint = self
            .connection
            .endpoint
            .or_else(|| read_env("SURREALDB_ENDPOINT"));
        self.connection.namespace = self
            .connection
            .namespace
            .or_else(|| read_env("SURREALDB_NAMESPACE"));
        self.connection.database = self
            .connection
            .database
            .or_else(|| read_env("SURREALDB_DATABASE"));
        self.connection.username = self
            .connection
            .username
            .or_else(|| read_env("SURREALDB_USERNAME"));
        self.connection.password = self
            .connection
            .password
            .or_else(|| read_env("SURREALDB_PASSWORD"));
        self.connection.token = self
            .connection
            .token
            .or_else(|| read_env("SURREALDB_TOKEN"));
        self
    }

    pub fn active_auth_context(&self) -> Option<&AuthContext> {
        self.active_auth_context
            .as_ref()
            .and_then(|name| {
                self.auth_contexts
                    .iter()
                    .find(|context| context.name == *name)
            })
            .or_else(|| self.auth_contexts.first())
    }
}

impl ConnectionSettings {
    pub fn is_configured(&self) -> bool {
        self.endpoint.is_some()
    }
}

/// Parse one settings payload. `Ok(None)` means the payload carried
/// nothing for us (e.g. `null`); `Err` carries the serde error for a
/// payload that *tried* to configure `surrealql` but was malformed —
/// previously that error was swallowed and the whole object silently
/// dropped.
fn parse_settings_value(value: &Value) -> Result<Option<ServerSettings>, String> {
    if value.is_null() {
        return Ok(None);
    }

    // A nested `{ "surrealql": { ... } }` root: parse the section
    // directly so a typo inside it surfaces instead of falling back
    // to an all-defaults flat parse.
    if let Some(section) = value.get("surrealql") {
        if section.is_null() {
            return Ok(None);
        }
        return serde_json::from_value::<ServerSettings>(section.clone())
            .map(Some)
            .map_err(|error| error.to_string());
    }

    serde_json::from_value::<ServerSettings>(value.clone())
        .map(Some)
        .map_err(|error| error.to_string())
}

fn default_true() -> bool {
    true
}

fn default_metadata_mode() -> String {
    "workspace+db".to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ServerSettings;

    #[test]
    fn reads_nested_surrealql_settings() {
        let value = json!({
            "surrealql": {
                "connection": { "endpoint": "ws://127.0.0.1:8000/rpc" },
                "activeAuthContext": "viewer"
            }
        });

        let settings = ServerSettings::from_sources(Some(&value), None);
        assert_eq!(
            settings.connection.endpoint.as_deref(),
            Some("ws://127.0.0.1:8000/rpc")
        );
        assert_eq!(settings.active_auth_context.as_deref(), Some("viewer"));
    }

    #[test]
    fn reads_camel_case_analysis_settings() {
        let value = json!({
            "surrealql": {
                "connection": {
                    "access": "viewer"
                },
                "metadata": {
                    "enableLiveMetadata": false,
                    "refreshOnSave": false
                },
                "analysis": {
                    "enablePermissionAnalysis": false,
                    "enableAggressiveSchemaInference": false,
                    "enableCodeActions": false
                },
                "authContexts": [{
                    "name": "admin",
                    "roles": ["admin"],
                    "authRecord": "user:admin"
                }],
                "activeAuthContext": "admin"
            }
        });

        let settings = ServerSettings::from_sources(Some(&value), None);
        assert!(!settings.metadata.enable_live_metadata);
        assert!(!settings.metadata.refresh_on_save);
        assert!(!settings.analysis.enable_permission_analysis);
        assert!(!settings.analysis.enable_aggressive_schema_inference);
        assert!(!settings.analysis.enable_code_actions);
        assert_eq!(settings.connection.access.as_deref(), Some("viewer"));
        assert_eq!(
            settings.auth_contexts[0].auth_record.as_deref(),
            Some("user:admin")
        );
        assert_eq!(settings.active_auth_context.as_deref(), Some("admin"));
    }
}
