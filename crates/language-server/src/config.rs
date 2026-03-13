use std::env;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

#[derive(Debug, Deserialize)]
struct RootSettings {
    #[serde(default)]
    surrealql: Option<ServerSettings>,
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

impl ServerSettings {
    pub fn from_sources(
        initialization_options: Option<&Value>,
        configuration: Option<&Value>,
    ) -> Self {
        let mut settings = Self::default();

        for value in [initialization_options, configuration] {
            if let Some(parsed) = value.and_then(parse_settings_value) {
                settings = parsed.merge_with_env();
            }
        }

        if initialization_options.is_none() && configuration.is_none() {
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

        settings
    }

    pub fn merge_with_env(mut self) -> Self {
        self.connection.endpoint = self
            .connection
            .endpoint
            .or_else(|| env::var("SURREALDB_ENDPOINT").ok());
        self.connection.namespace = self
            .connection
            .namespace
            .or_else(|| env::var("SURREALDB_NAMESPACE").ok());
        self.connection.database = self
            .connection
            .database
            .or_else(|| env::var("SURREALDB_DATABASE").ok());
        self.connection.username = self
            .connection
            .username
            .or_else(|| env::var("SURREALDB_USERNAME").ok());
        self.connection.password = self
            .connection
            .password
            .or_else(|| env::var("SURREALDB_PASSWORD").ok());
        self.connection.token = self
            .connection
            .token
            .or_else(|| env::var("SURREALDB_TOKEN").ok());
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

fn parse_settings_value(value: &Value) -> Option<ServerSettings> {
    serde_json::from_value::<RootSettings>(value.clone())
        .ok()
        .and_then(|root| root.surrealql)
        .or_else(|| serde_json::from_value::<ServerSettings>(value.clone()).ok())
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
