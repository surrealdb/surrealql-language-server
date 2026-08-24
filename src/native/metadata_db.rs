//! [`MetadataProvider`] implementation that talks to a real SurrealDB
//! instance via the [`surrealdb`] Rust SDK.
//!
//! Walks `INFO FOR DB` and every `INFO FOR TABLE <name>`, harvests the
//! returned `DEFINE …` statements, and re-parses them through the
//! analyzer so live tables/fields/functions appear alongside the
//! workspace ones in the merged semantic model.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ls_types::Uri;
use serde_json::Value as JsonValue;
use surrealdb::engine::any::connect;
use surrealdb::opt::auth::{Database, Root};
use surrealdb::types::Value as SurrealValue;
use tokio::time::timeout;

use crate::config::ServerSettings;
use crate::core::client::MetadataProvider;
use crate::semantic::analyzer::analyze_document;
use crate::semantic::types::{LiveMetadataSnapshot, SymbolOrigin};

/// Hard ceiling on the total INFO-FOR-DB + INFO-FOR-TABLE walk so that
/// a degenerate database (thousands of tables) or an unreachable
/// endpoint can't pin the cold-start task forever.
const TOTAL_FETCH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct SurrealDbMetadataProvider;

impl SurrealDbMetadataProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl MetadataProvider for SurrealDbMetadataProvider {
    async fn fetch(&self, settings: &ServerSettings) -> LiveMetadataSnapshot {
        if !settings.metadata.enable_live_metadata
            || !settings.metadata.db_enabled()
            || !settings.connection.is_configured()
        {
            return LiveMetadataSnapshot::default();
        }

        match timeout(TOTAL_FETCH_TIMEOUT, fetch_snapshot_inner(settings)).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(error)) => LiveMetadataSnapshot {
                documents: Default::default(),
                errors: vec![error],
            },
            Err(_) => LiveMetadataSnapshot {
                documents: Default::default(),
                errors: vec![format!(
                    "SurrealDB metadata fetch exceeded {}s timeout",
                    TOTAL_FETCH_TIMEOUT.as_secs()
                )],
            },
        }
    }
}

async fn fetch_snapshot_inner(settings: &ServerSettings) -> Result<LiveMetadataSnapshot, String> {
    let endpoint = settings
        .connection
        .endpoint
        .clone()
        .ok_or_else(|| "missing SurrealDB endpoint".to_string())?;

    let db = connect(endpoint)
        .await
        .map_err(|error| format!("failed to connect to SurrealDB: {error}"))?;

    if let Some(token) = &settings.connection.token {
        db.authenticate(token.clone())
            .await
            .map_err(|error| format!("failed to authenticate with token: {error}"))?;
    } else if let (Some(username), Some(password)) = (
        settings.connection.username.clone(),
        settings.connection.password.clone(),
    ) {
        if db
            .signin(Root {
                username: username.clone(),
                password: password.clone(),
            })
            .await
            .is_err()
        {
            let namespace = settings
                .connection
                .namespace
                .clone()
                .ok_or_else(|| "database auth requires namespace".to_string())?;
            let database = settings
                .connection
                .database
                .clone()
                .ok_or_else(|| "database auth requires database".to_string())?;
            db.signin(Database {
                namespace,
                database,
                username,
                password,
            })
            .await
            .map_err(|error| format!("failed to authenticate with username/password: {error}"))?;
        }
    }

    if let Some(namespace) = &settings.connection.namespace {
        if let Some(database) = &settings.connection.database {
            db.use_ns(namespace)
                .use_db(database)
                .await
                .map_err(|error| format!("failed to select namespace/database: {error}"))?;
        }
    }

    let mut snapshot = LiveMetadataSnapshot::default();
    let mut response = db
        .query("INFO FOR DB;")
        .await
        .map_err(|error| format!("failed to query INFO FOR DB: {error}"))?
        .check()
        .map_err(|error| format!("INFO FOR DB returned an error: {error}"))?;
    let info_value: SurrealValue = response
        .take(0)
        .map_err(|error| format!("failed to decode INFO FOR DB: {error}"))?;
    let info_json = serde_json::to_value(info_value)
        .map_err(|error| format!("failed to serialize INFO FOR DB: {error}"))?;

    let mut define_strings = Vec::new();
    collect_define_strings(&info_json, &mut define_strings);

    if let Some(tables) = info_json.get("tables").and_then(JsonValue::as_object) {
        for table in tables.keys() {
            let query = format!("INFO FOR TABLE {table};");
            match db.query(query).await.and_then(|result| result.check()) {
                Ok(mut result) => {
                    if let Ok(value) = result.take::<SurrealValue>(0) {
                        if let Ok(json) = serde_json::to_value(value) {
                            collect_define_strings(&json, &mut define_strings);
                        }
                    }
                }
                Err(error) => snapshot
                    .errors
                    .push(format!("failed to query INFO FOR TABLE {table}: {error}")),
            }
        }
    }

    for (index, define) in define_strings.into_iter().enumerate() {
        let uri = format!("surrealdb:///metadata/{}.surql", index)
            .parse::<Uri>()
            .map_err(|error| format!("failed to build metadata uri: {error}"))?;
        if let Some(analysis) = analyze_document(uri.clone(), &define, SymbolOrigin::Remote) {
            snapshot.documents.insert(uri, Arc::new(analysis));
        }
    }

    Ok(snapshot)
}

fn collect_define_strings(value: &JsonValue, target: &mut Vec<String>) {
    match value {
        JsonValue::String(text) if text.trim_start().starts_with("DEFINE ") => {
            if !target.contains(text) {
                target.push(text.clone());
            }
        }
        JsonValue::Array(items) => {
            for item in items {
                collect_define_strings(item, target);
            }
        }
        JsonValue::Object(object) => {
            for value in object.values() {
                collect_define_strings(value, target);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collected(value: JsonValue) -> Vec<String> {
        let mut found = Vec::new();
        collect_define_strings(&value, &mut found);
        found
    }

    /// `INFO FOR DB` answers with a nested object whose leaves are the `DEFINE`
    /// statements. The harvest has to reach them wherever they sit, because the
    /// shape has moved between engine versions and this code cannot see which
    /// version answered.
    #[test]
    fn defines_are_found_at_any_depth() {
        let value = serde_json::json!({
            "tables": {
                "person": "DEFINE TABLE person SCHEMAFULL",
                "nested": { "deeper": ["DEFINE FIELD email ON person TYPE string"] }
            },
            "functions": { "fn::f": "DEFINE FUNCTION fn::f() { RETURN 1; }" },
        });
        let found = collected(value);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(
            found
                .iter()
                .any(|text| text.contains("DEFINE TABLE person"))
        );
        assert!(found.iter().any(|text| text.contains("DEFINE FIELD email")));
        assert!(found.iter().any(|text| text.contains("DEFINE FUNCTION")));
    }

    /// A string that merely mentions `DEFINE` is not a definition. The check is
    /// a prefix on the trimmed text, so a comment or a description cannot leak
    /// into the schema.
    #[test]
    fn only_strings_that_start_with_define_are_taken() {
        let value = serde_json::json!({
            "a": "this DEFINE is inside a sentence",
            "b": "  DEFINE TABLE indented SCHEMAFULL",
            "c": "SELECT * FROM person",
            "d": 42,
            "e": null,
            "f": true,
        });
        assert_eq!(
            collected(value),
            vec!["  DEFINE TABLE indented SCHEMAFULL".to_string()]
        );
    }

    /// The same statement can appear under several keys — `INFO FOR DB` and
    /// `INFO FOR TABLE` overlap. Duplicates would each be re-parsed and then
    /// merged against themselves.
    #[test]
    fn duplicates_are_dropped() {
        let value = serde_json::json!({
            "one": "DEFINE TABLE person SCHEMAFULL",
            "two": "DEFINE TABLE person SCHEMAFULL",
            "three": ["DEFINE TABLE person SCHEMAFULL"],
        });
        assert_eq!(
            collected(value),
            vec!["DEFINE TABLE person SCHEMAFULL".to_string()]
        );
    }

    /// Whitespace difference makes two statements distinct, deliberately: this
    /// layer does not normalise SurrealQL, and pretending two spellings are one
    /// would need a parse it does not do.
    #[test]
    fn whitespace_makes_two_statements_distinct() {
        let value = serde_json::json!([
            "DEFINE TABLE person SCHEMAFULL",
            "DEFINE  TABLE person SCHEMAFULL",
        ]);
        assert_eq!(collected(value).len(), 2);
    }

    #[test]
    fn an_empty_answer_yields_nothing() {
        assert!(collected(serde_json::json!({})).is_empty());
        assert!(collected(serde_json::json!([])).is_empty());
        assert!(collected(JsonValue::Null).is_empty());
    }

    /// Deep nesting must not overflow the stack. `INFO FOR DB` is shallow in
    /// practice, but this walks whatever the server sends.
    #[test]
    fn deep_nesting_is_survivable() {
        let mut value = JsonValue::String("DEFINE TABLE deep SCHEMAFULL".to_string());
        for _ in 0..200 {
            value = serde_json::json!({ "next": value });
        }
        assert_eq!(collected(value).len(), 1);
    }
}
