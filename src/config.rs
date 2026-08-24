use std::collections::BTreeMap;

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
    /// Accepted for compatibility. It has no effect.
    ///
    /// Nothing has ever read it. It is kept rather than removed because
    /// `tests/compat.rs` pins its parsing, its default and the fact that
    /// setting it produces no warning — a client that has it in a settings file
    /// must not start seeing errors. Remove it only in a release that says so.
    #[serde(default = "default_true", alias = "enable_aggressive_schema_inference")]
    pub enable_aggressive_schema_inference: bool,
    /// Whether `textDocument/codeAction` returns anything.
    ///
    /// The capability stays advertised when this is `false`; the handler
    /// returns an empty list. Withdrawing the capability would change the
    /// advertised server shape based on a setting, and a client reads that
    /// once at `initialize`.
    #[serde(default = "default_true", alias = "enable_code_actions")]
    pub enable_code_actions: bool,
    /// Report call arguments whose type cannot satisfy the declared
    /// parameter type. Only definite mismatches are reported — anything
    /// the inference engine is unsure about stays silent.
    #[serde(default = "default_true", alias = "enable_type_checking")]
    pub enable_type_checking: bool,
    /// Which diagnostics apply to a table declared `SCHEMALESS`. Such a
    /// table legitimately accepts ad-hoc fields, so the strict checks
    /// that fit a `SCHEMAFULL` table are noise there.
    ///
    /// - `quiet` (default) — report none of [`SCHEMALESS_SCOPED_CODES`].
    /// - `errors` — report only the two the engine itself rejects
    ///   (`field-type`, `unknown-type`); stay quiet about the advisory
    ///   ones (`unknown-field`, `permission-denied`, `permission-unknown`).
    /// - `strict` — no exemption at all; treat `SCHEMALESS` exactly like
    ///   `SCHEMAFULL`.
    ///
    /// Only a table that carries the keyword is covered. A bare `DEFINE
    /// TABLE t` is schemaless to the engine but leaves `schema_mode`
    /// unset, so it keeps the diagnostics it had before this setting
    /// existed — see
    /// [`crate::semantic::model::MergedSemanticModel::schemaless_hides`].
    #[serde(
        default = "default_schemaless_diagnostics",
        alias = "schemaless_diagnostics"
    )]
    pub schemaless_diagnostics: String,
    /// Upper bound on **syntax** diagnostics (`parse`, `unknown-type`) per
    /// document, so a pathological buffer cannot flood the problems panel.
    /// `0` reports every one.
    ///
    /// This counts diagnostics, not lines — no setting here limits how long a
    /// document may be. Semantic and type diagnostics are uncapped: they are
    /// derived from the query facts and definitions in the file, so their
    /// count is already bounded by the code itself.
    #[serde(
        default = "default_max_syntax_diagnostics",
        alias = "max_syntax_diagnostics"
    )]
    pub max_syntax_diagnostics: usize,
    /// How long to wait for typing to settle before analysing an edited buffer,
    /// in milliseconds. `0` disables the wait.
    ///
    /// Every keystroke otherwise triggers a full reparse and diagnostic
    /// publish. At ten characters a second that is ten of each, and only the
    /// last one describes what is on screen. The wait is per document: a newer
    /// edit arriving during it supersedes the older one, which is dropped.
    ///
    /// `didOpen` is never delayed. The file just appeared and the user is
    /// waiting to see what is wrong with it.
    #[serde(
        default = "default_diagnostic_debounce_ms",
        alias = "diagnostic_debounce_ms"
    )]
    pub diagnostic_debounce_ms: u64,
    /// Variable names the *caller* binds at runtime, without a `$` sigil —
    /// e.g. `["id", "limit"]` for a script run as
    /// `db.query(sql).bind(("id", id))`, or the names in Surrealist's
    /// variables panel.
    ///
    /// Such names are legitimately absent from the file, so the
    /// undefined-variable check would otherwise flag them. Declaring them
    /// here keeps the check strict everywhere else.
    #[serde(default, alias = "external_params")]
    pub external_params: Vec<String>,
    /// Per-rule severity, keyed by the rule id that appears in
    /// `Diagnostic.code` — see [`crate::semantic::rules`]. Accepted values are
    /// `off`, `hint`, `info`, `warning` and `error`.
    ///
    /// This wins over the coarse booleans above, so a single rule can be turned
    /// back on under `enableTypeChecking: false`, or turned off on its own
    /// without disabling its whole category.
    ///
    /// A `BTreeMap` rather than a `HashMap` so equality and warning order are
    /// deterministic — this struct derives `PartialEq` and the warnings are
    /// deduplicated by signature.
    #[serde(default, alias = "rule_severity")]
    pub rule_severity: BTreeMap<String, String>,
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
            enable_type_checking: true,
            schemaless_diagnostics: default_schemaless_diagnostics(),
            max_syntax_diagnostics: default_max_syntax_diagnostics(),
            diagnostic_debounce_ms: default_diagnostic_debounce_ms(),
            external_params: Vec::new(),
            rule_severity: BTreeMap::new(),
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

/// The `analysis.schemalessDiagnostics` strings the server understands.
/// Same treatment as [`ACCEPTED_METADATA_MODES`]: an unknown value is
/// repaired to the default with a warning rather than silently picking
/// one of the three behaviors.
pub const ACCEPTED_SCHEMALESS_DIAGNOSTICS: &[&str] = &["quiet", "errors", "strict"];

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
        Self::from_sources_with_project(None, initialization_options, configuration)
    }

    /// [`Self::from_sources_with_warnings`] with a project-configuration layer
    /// underneath the LSP payloads.
    ///
    /// Precedence is defaults, then the project file, then the LSP settings,
    /// then the environment fallback for connection fields.
    ///
    /// The merge happens on the JSON `Value`, before deserialization, and it
    /// has to: `parse_settings_value` *replaces* the settings from each source
    /// rather than merging them, and a deserialized struct cannot tell an
    /// explicitly-set `true` from a defaulted one. Merging afterwards would
    /// therefore let the file overwrite an LSP value the user actually set.
    ///
    /// `project` arrives as JSON rather than TOML so this stays
    /// target-agnostic: the native loader does the conversion, and a browser
    /// host can push a value in the same shape.
    pub fn from_sources_with_project(
        project: Option<&Value>,
        initialization_options: Option<&Value>,
        configuration: Option<&Value>,
    ) -> (Self, Vec<String>) {
        let Some(project) = project.filter(|value| !value.is_null()) else {
            return Self::from_lsp_sources(initialization_options, configuration);
        };
        // The file is entirely ours, so a `surrealql` wrapper is optional.
        let base = project.get("surrealql").unwrap_or(project);

        let merged_init = initialization_options.map(|value| layer_under(base, value));
        let merged_config = configuration.map(|value| layer_under(base, value));

        if merged_init.is_none() && merged_config.is_none() {
            // No LSP payload at all: the file is the only source.
            return Self::from_lsp_sources(Some(base), None);
        }
        Self::from_lsp_sources(merged_init.as_ref(), merged_config.as_ref())
    }

    fn from_lsp_sources(
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
            let mut sweep_warnings = Vec::new();
            match parse_settings_value(value, &mut sweep_warnings) {
                Ok(Some(parsed)) => {
                    settings = parsed.merge_with_env();
                    parsed_any = true;
                }
                Ok(None) => {}
                Err(error) => warnings.push(format!(
                    "invalid `surrealql` settings in {label}: {error}; the payload was ignored"
                )),
            }
            warnings.extend(
                sweep_warnings
                    .into_iter()
                    .map(|warning| format!("{warning} (in {label})")),
            );
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
    pub fn validate_and_repair(&mut self) -> Vec<String> {
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

        if !ACCEPTED_SCHEMALESS_DIAGNOSTICS.contains(&self.analysis.schemaless_diagnostics.as_str())
        {
            warnings.push(format!(
                "unknown analysis.schemalessDiagnostics `{}` was ignored (accepted values: {})",
                self.analysis.schemaless_diagnostics,
                ACCEPTED_SCHEMALESS_DIAGNOSTICS.join(", "),
            ));
            self.analysis.schemaless_diagnostics = default_schemaless_diagnostics();
        }

        // `ruleSeverity` keys are rule ids and its values are severity names.
        // Neither is a settings key, so `collect_unknown_keys` never reaches
        // them — it checks the keys of `analysis` and does not descend. They
        // are swept here instead, and a bad entry is dropped rather than
        // guessed at: silently reading `"warn "` as `warning` would hide a
        // typo that changes what a whole team sees.
        {
            let mut rejected = Vec::new();
            self.analysis.rule_severity.retain(|id, value| {
                let known_rule = crate::semantic::rules::rule(id).is_some();
                let known_severity = crate::semantic::rules::parse_severity(value).is_some();
                if !known_rule {
                    rejected.push(unknown_rule_warning(id));
                } else if !known_severity {
                    rejected.push(format!(
                        "unknown analysis.ruleSeverity value `{value}` for `{id}` was ignored \
                         (accepted values: {})",
                        crate::semantic::rules::ACCEPTED_RULE_SEVERITIES.join(", "),
                    ));
                }
                known_rule && known_severity
            });
            warnings.extend(rejected);
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
/// Put `base` underneath `source`, preserving `source`'s shape.
///
/// A nested `{"surrealql": {…}}` source keeps its wrapper, so the unknown-key
/// sweep still treats the inner object as entirely ours. A flat source stays
/// flat, so unrelated editor keys at its root are still left alone.
fn layer_under(base: &Value, source: &Value) -> Value {
    match source.get("surrealql") {
        Some(inner) => {
            let mut wrapper = serde_json::Map::new();
            wrapper.insert("surrealql".to_string(), deep_merge(base, inner));
            Value::Object(wrapper)
        }
        None => deep_merge(base, source),
    }
}

/// Recursive object merge. `overlay` wins at every leaf; a key present only in
/// `base` survives.
fn deep_merge(base: &Value, overlay: &Value) -> Value {
    let (Some(base_object), Some(overlay_object)) = (base.as_object(), overlay.as_object()) else {
        return overlay.clone();
    };
    let mut merged = base_object.clone();
    for (key, value) in overlay_object {
        match merged.get(key) {
            Some(existing) if existing.is_object() && value.is_object() => {
                merged.insert(key.clone(), deep_merge(existing, value));
            }
            _ => {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(merged)
}

fn parse_settings_value(
    value: &Value,
    warnings: &mut Vec<String>,
) -> Result<Option<ServerSettings>, String> {
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
        let settings = serde_json::from_value::<ServerSettings>(section.clone())
            .map_err(|error| error.to_string())?;
        // The nested section is entirely ours — sweep its top level
        // too. Only on the Ok path: a serde failure already warned.
        collect_unknown_keys(section, true, warnings);
        return Ok(Some(settings));
    }

    let settings = serde_json::from_value::<ServerSettings>(value.clone())
        .map_err(|error| error.to_string())?;
    // A flat root may legitimately carry unrelated editor keys, so
    // only the known sub-objects are swept.
    collect_unknown_keys(value, false, warnings);
    Ok(Some(settings))
}

/// Every key `ServerSettings` deserializes, per section, in both
/// casings. serde ignores unknown fields (deny_unknown_fields would
/// break the camelCase/snake_case aliases), so misspelled keys —
/// the most common settings mistake — parse Ok as all-defaults. This
/// sweep is what turns them into warnings. The
/// `known_key_lists_cover_every_settings_field` test guards against
/// these lists drifting from the structs.
const TOP_LEVEL_KEYS: &[&str] = &[
    "connection",
    "metadata",
    "analysis",
    "authContexts",
    "auth_contexts",
    "activeAuthContext",
    "active_auth_context",
];
const CONNECTION_KEYS: &[&str] = &[
    "endpoint",
    "namespace",
    "database",
    "username",
    "password",
    "token",
    "access",
];
const METADATA_KEYS: &[&str] = &[
    "mode",
    "enableLiveMetadata",
    "enable_live_metadata",
    "refreshOnSave",
    "refresh_on_save",
];
const ANALYSIS_KEYS: &[&str] = &[
    "enablePermissionAnalysis",
    "enable_permission_analysis",
    "enableAggressiveSchemaInference",
    "enable_aggressive_schema_inference",
    "enableCodeActions",
    "enable_code_actions",
    "enableTypeChecking",
    "enable_type_checking",
    "schemalessDiagnostics",
    "schemaless_diagnostics",
    "maxSyntaxDiagnostics",
    "max_syntax_diagnostics",
    "diagnosticDebounceMs",
    "diagnostic_debounce_ms",
    "externalParams",
    "external_params",
    "ruleSeverity",
    "rule_severity",
];
const AUTH_CONTEXT_KEYS: &[&str] = &[
    "name",
    "roles",
    "authRecord",
    "auth_record",
    "claims",
    "session",
    "variables",
];

/// Warn about object keys the settings structs don't know. When
/// `sweep_top_level` is false (flat root payloads), only the known
/// sub-objects are inspected.
fn collect_unknown_keys(section: &Value, sweep_top_level: bool, warnings: &mut Vec<String>) {
    let Some(object) = section.as_object() else {
        return;
    };

    if sweep_top_level {
        for key in object.keys() {
            if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
                warnings.push(unknown_key_warning("", key, TOP_LEVEL_KEYS));
            }
        }
    }

    for (sub_object, known_keys) in [
        ("connection", CONNECTION_KEYS),
        ("metadata", METADATA_KEYS),
        ("analysis", ANALYSIS_KEYS),
    ] {
        let Some(sub) = object.get(sub_object).and_then(Value::as_object) else {
            continue;
        };
        for key in sub.keys() {
            if !known_keys.contains(&key.as_str()) {
                warnings.push(unknown_key_warning(sub_object, key, known_keys));
            }
        }
    }

    for contexts_key in ["authContexts", "auth_contexts"] {
        let Some(contexts) = object.get(contexts_key).and_then(Value::as_array) else {
            continue;
        };
        for context in contexts {
            let Some(context) = context.as_object() else {
                continue;
            };
            for key in context.keys() {
                // `claims`/`session`/`variables` hold arbitrary JSON —
                // never descend into them; only their own key names
                // are validated here.
                if !AUTH_CONTEXT_KEYS.contains(&key.as_str()) {
                    warnings.push(unknown_key_warning(contexts_key, key, AUTH_CONTEXT_KEYS));
                }
            }
        }
    }
}

/// A rule id in `analysis.ruleSeverity` that no rule answers to.
///
/// Same shape as [`unknown_key_warning`], but the candidate set is the rule
/// registry rather than a static key list.
fn unknown_rule_warning(id: &str) -> String {
    let suggestion = crate::semantic::rules::ids()
        .map(|known| (strsim::jaro_winkler(id, known), known))
        .filter(|(score, _)| *score >= 0.8)
        .max_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, known)| known);
    match suggestion {
        Some(known) => {
            format!("unknown rule `{id}` in analysis.ruleSeverity — did you mean `{known}`?")
        }
        None => format!("unknown rule `{id}` in analysis.ruleSeverity was ignored"),
    }
}

fn unknown_key_warning(section: &str, key: &str, known_keys: &[&str]) -> String {
    let path = if section.is_empty() {
        format!("`{key}`")
    } else {
        format!("`{section}.{key}`")
    };
    let suggestion = known_keys
        .iter()
        .map(|known| (strsim::jaro_winkler(key, known), known))
        .filter(|(score, _)| *score >= 0.8)
        .max_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, known)| known);
    match suggestion {
        Some(known) => format!("unknown setting {path} — did you mean `{known}`?"),
        None => format!("unknown setting {path} was ignored"),
    }
}

fn default_true() -> bool {
    true
}

fn default_metadata_mode() -> String {
    "workspace+db".to_string()
}

fn default_schemaless_diagnostics() -> String {
    "quiet".to_string()
}

/// 200 ms. Long enough that a burst of keystrokes collapses to one analysis,
/// short enough that a pause between words still feels immediate.
fn default_diagnostic_debounce_ms() -> u64 {
    200
}

fn default_max_syntax_diagnostics() -> usize {
    crate::semantic::analyzer::DEFAULT_MAX_SYNTAX_DIAGNOSTICS
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

    #[test]
    fn reads_schemaless_diagnostics_in_both_casings() {
        for key in ["schemalessDiagnostics", "schemaless_diagnostics"] {
            let value = json!({ "surrealql": { "analysis": { key: "strict" } } });
            let (settings, warnings) =
                ServerSettings::from_sources_with_warnings(Some(&value), None);
            assert_eq!(settings.analysis.schemaless_diagnostics, "strict", "{key}");
            assert_eq!(warnings, Vec::<String>::new(), "{key}");
        }
    }

    #[test]
    fn reads_max_syntax_diagnostics_in_both_casings() {
        for key in ["maxSyntaxDiagnostics", "max_syntax_diagnostics"] {
            let value = json!({ "surrealql": { "analysis": { key: 250 } } });
            let (settings, warnings) =
                ServerSettings::from_sources_with_warnings(Some(&value), None);
            assert_eq!(settings.analysis.max_syntax_diagnostics, 250, "{key}");
            assert_eq!(warnings, Vec::<String>::new(), "{key}");
        }
    }

    #[test]
    fn unknown_schemaless_diagnostics_repairs_to_the_default() {
        let value = json!({ "surrealql": { "analysis": { "schemalessDiagnostics": "loud" } } });
        let (settings, warnings) = ServerSettings::from_sources_with_warnings(Some(&value), None);
        assert_eq!(
            settings.analysis.schemaless_diagnostics, "quiet",
            "an unknown value must not select a behavior"
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("`loud`")
                    && warning.contains("schemalessDiagnostics")),
            "{warnings:?}"
        );
    }

    #[test]
    fn unknown_connection_key_warns_with_suggestion() {
        let value = json!({
            "surrealql": { "connection": { "endpint": "ws://127.0.0.1:8000/rpc" } }
        });

        let (settings, warnings) = ServerSettings::from_sources_with_warnings(Some(&value), None);
        assert!(settings.connection.endpoint.is_none());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("`connection.endpint`"), "{warnings:?}");
        assert!(
            warnings[0].contains("did you mean `endpoint`?"),
            "{warnings:?}"
        );
        assert!(
            warnings[0].contains("initializationOptions"),
            "warning must name its source: {warnings:?}"
        );
    }

    #[test]
    fn unknown_nested_top_level_key_warns() {
        let value = json!({
            "surrealql": { "connektion": { "endpoint": "ws://127.0.0.1:8000/rpc" } }
        });

        let (_, warnings) = ServerSettings::from_sources_with_warnings(Some(&value), None);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("`connektion`")
                    && warning.contains("did you mean `connection`?")),
            "{warnings:?}"
        );
    }

    #[test]
    fn flat_root_ignores_unrelated_top_level_keys() {
        // Editors commonly hand the whole settings object over in the
        // flat shape — foreign top-level keys are not our business.
        let value = json!({
            "editor.fontSize": 14,
            "rust-analyzer": { "check": true },
            "connection": { "endpoint": "ws://127.0.0.1:8000/rpc" },
        });

        let (settings, warnings) = ServerSettings::from_sources_with_warnings(Some(&value), None);
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            settings.connection.endpoint.as_deref(),
            Some("ws://127.0.0.1:8000/rpc")
        );
    }

    #[test]
    fn rule_severity_accepts_a_known_rule_and_severity() {
        let value = json!({
            "surrealql": {
                "analysis": { "ruleSeverity": { "unknown-field": "error", "let-type": "off" } }
            }
        });
        let (settings, warnings) = ServerSettings::from_sources_with_warnings(Some(&value), None);
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            settings.analysis.rule_severity.get("unknown-field"),
            Some(&"error".to_string())
        );
        assert_eq!(
            settings.analysis.rule_severity.get("let-type"),
            Some(&"off".to_string())
        );
    }

    /// A rule id nobody answers to is dropped, not silently kept. Keeping it
    /// would let a typo look like a configured rule that never fires.
    #[test]
    fn an_unknown_rule_id_is_dropped_with_a_hint() {
        let value = json!({
            "surrealql": { "analysis": { "ruleSeverity": { "unknown-feild": "off" } } }
        });
        let (settings, warnings) = ServerSettings::from_sources_with_warnings(Some(&value), None);
        assert!(settings.analysis.rule_severity.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("unknown-feild") && warnings[0].contains("unknown-field"),
            "expected a did-you-mean hint, got: {}",
            warnings[0]
        );
    }

    #[test]
    fn an_unrecognisable_rule_id_is_dropped_without_a_hint() {
        let value = json!({
            "surrealql": { "analysis": { "ruleSeverity": { "zzzzzzzz": "off" } } }
        });
        let (settings, warnings) = ServerSettings::from_sources_with_warnings(Some(&value), None);
        assert!(settings.analysis.rule_severity.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("was ignored"), "got: {}", warnings[0]);
    }

    /// A severity outside the vocabulary is dropped rather than guessed at.
    /// Reading `"warn "` as `warning` would hide a typo that changes what a
    /// whole team sees.
    #[test]
    fn an_unknown_severity_value_is_dropped_with_the_accepted_list() {
        let value = json!({
            "surrealql": { "analysis": { "ruleSeverity": { "let-type": "loud" } } }
        });
        let (settings, warnings) = ServerSettings::from_sources_with_warnings(Some(&value), None);
        assert!(settings.analysis.rule_severity.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("`loud`") && warnings[0].contains("error"),
            "expected the accepted list, got: {}",
            warnings[0]
        );
    }

    /// The keys inside `ruleSeverity` are rule ids, not settings keys. The
    /// sweep must never report them as unknown settings.
    #[test]
    fn rule_severity_keys_are_not_swept_as_settings_keys() {
        let value = json!({
            "surrealql": {
                "analysis": { "ruleSeverity": { "unknown-table": "warning" } }
            }
        });
        let (_, warnings) = ServerSettings::from_sources_with_warnings(Some(&value), None);
        assert_eq!(warnings, Vec::<String>::new());
    }

    #[test]
    fn auth_context_payload_keys_are_not_swept() {
        // claims/session/variables carry arbitrary JSON — their inner
        // keys must never be reported as unknown settings.
        let value = json!({
            "surrealql": {
                "authContexts": [{
                    "name": "admin",
                    "claims": { "custom_claim": true },
                    "session": { "whatever": 1 },
                    "variables": { "x": "y" },
                }],
            }
        });

        let (_, warnings) = ServerSettings::from_sources_with_warnings(Some(&value), None);
        assert_eq!(warnings, Vec::<String>::new());
    }

    /// Drift guard: every key the settings structs serialize must be
    /// in the sweep's known-key lists, so a newly added field can't
    /// start warning as "unknown". (Dropped snake_case aliases are
    /// caught separately by `config_accepts_all_historical_shapes`
    /// in tests/compat.rs, which asserts zero warnings for the
    /// historical payload shapes.)
    #[test]
    fn known_key_lists_cover_every_settings_field() {
        let mut settings = ServerSettings::default();
        settings.auth_contexts = vec![super::AuthContext::default()];
        let value = serde_json::to_value(&settings).expect("serializable");
        let object = value.as_object().expect("object");

        for key in object.keys() {
            assert!(
                super::TOP_LEVEL_KEYS.contains(&key.as_str()),
                "top-level key `{key}` missing from TOP_LEVEL_KEYS"
            );
        }
        for (section, known) in [
            ("connection", super::CONNECTION_KEYS),
            ("metadata", super::METADATA_KEYS),
            ("analysis", super::ANALYSIS_KEYS),
        ] {
            let sub = object[section].as_object().expect("sub object");
            for key in sub.keys() {
                assert!(
                    known.contains(&key.as_str()),
                    "`{section}.{key}` missing from its known-key list"
                );
            }
        }
        let context = value["authContexts"][0].as_object().expect("context");
        for key in context.keys() {
            assert!(
                super::AUTH_CONTEXT_KEYS.contains(&key.as_str()),
                "auth-context key `{key}` missing from AUTH_CONTEXT_KEYS"
            );
        }
    }
}
