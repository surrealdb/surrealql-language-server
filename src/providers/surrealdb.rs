use serde_json::Value as JsonValue;
use surrealdb::engine::any::connect;
use surrealdb::opt::auth::{Database, Root};
use surrealdb::types::Value as SurrealValue;
use tower_lsp::lsp_types::Url;

use crate::config::ServerSettings;
use crate::semantic::analyzer::analyze_document;
use crate::semantic::types::{LiveMetadataSnapshot, SymbolOrigin};

pub struct SurrealDbProvider;

impl SurrealDbProvider {
    pub async fn fetch_snapshot(settings: &ServerSettings) -> LiveMetadataSnapshot {
        if !settings.metadata.enable_live_metadata || !settings.connection.is_configured() {
            return LiveMetadataSnapshot::default();
        }

        match fetch_snapshot_inner(settings).await {
            Ok(snapshot) => snapshot,
            Err(error) => LiveMetadataSnapshot {
                documents: Default::default(),
                errors: vec![error],
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
    let db = connect(endpoint.clone())
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
        let uri = Url::parse(&format!("surrealdb:///metadata/{}.surql", index))
            .map_err(|error| format!("failed to build metadata uri: {error}"))?;
        if let Some(analysis) = analyze_document(uri.clone(), &define, SymbolOrigin::Remote) {
            snapshot.documents.insert(uri, analysis);
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
