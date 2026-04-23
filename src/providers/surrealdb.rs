use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value as JsonValue;
use tokio::time::timeout;
use tower_lsp_server::ls_types::Uri;

use crate::config::ServerSettings;
use crate::semantic::analyzer::analyze_document;
use crate::semantic::types::{LiveMetadataSnapshot, SymbolOrigin};

/// Hard ceiling on every HTTP roundtrip we make to SurrealDB. A misconfigured
/// or unreachable endpoint must not be able to stall the LSP cold-start.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Hard ceiling on the total INFO-FOR-DB + INFO-FOR-TABLE walk so that a
/// degenerate database (thousands of tables) still can't pin the cold-start
/// task forever.
const TOTAL_FETCH_TIMEOUT: Duration = Duration::from_secs(15);

pub struct SurrealDbProvider;

impl SurrealDbProvider {
    pub async fn fetch_snapshot(settings: &ServerSettings) -> LiveMetadataSnapshot {
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
        .as_deref()
        .ok_or_else(|| "missing SurrealDB endpoint".to_string())?;
    let sql_url = sql_endpoint_url(endpoint)?;

    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(REQUEST_TIMEOUT)
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|error| format!("failed to build HTTP client: {error}"))?;

    let auth_header = build_auth_header(settings);

    let info_db_response = run_sql(
        &client,
        &sql_url,
        settings,
        auth_header.as_deref(),
        "INFO FOR DB;",
    )
    .await?;

    let mut snapshot = LiveMetadataSnapshot::default();
    let mut define_strings = Vec::new();

    let info_value = first_result_value(&info_db_response).unwrap_or(&JsonValue::Null);
    collect_define_strings(info_value, &mut define_strings);

    if let Some(tables) = info_value.get("tables").and_then(JsonValue::as_object) {
        for table in tables.keys() {
            let query = format!("INFO FOR TABLE {table};");
            match run_sql(&client, &sql_url, settings, auth_header.as_deref(), &query).await {
                Ok(response) => {
                    if let Some(value) = first_result_value(&response) {
                        collect_define_strings(value, &mut define_strings);
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

/// Convert the user-supplied endpoint (which may be `ws[s]://host:port/rpc`,
/// `http[s]://host:port`, or anything in between) into the canonical
/// `http[s]://host:port/sql` URL that SurrealDB exposes for raw SQL requests.
fn sql_endpoint_url(endpoint: &str) -> Result<String, String> {
    let trimmed = endpoint.trim().trim_end_matches('/');
    let (scheme_idx, path_idx) = trimmed
        .find("://")
        .map(|i| (i, i + 3))
        .unwrap_or((0, 0));

    let scheme = if path_idx == 0 {
        "http"
    } else {
        let raw = &trimmed[..scheme_idx];
        match raw.to_ascii_lowercase().as_str() {
            "ws" | "http" => "http",
            "wss" | "https" => "https",
            other => {
                return Err(format!("unsupported endpoint scheme `{other}`"));
            }
        }
    };

    let host_and_path = &trimmed[path_idx..];
    if host_and_path.is_empty() {
        return Err("endpoint is missing host".to_string());
    }

    let (host, _existing_path) = match host_and_path.find('/') {
        Some(i) => (&host_and_path[..i], &host_and_path[i..]),
        None => (host_and_path, ""),
    };

    Ok(format!("{scheme}://{host}/sql"))
}

fn build_auth_header(settings: &ServerSettings) -> Option<String> {
    if let Some(token) = &settings.connection.token {
        return Some(format!("Bearer {token}"));
    }
    let username = settings.connection.username.as_deref()?;
    let password = settings.connection.password.as_deref().unwrap_or("");
    let encoded = BASE64.encode(format!("{username}:{password}"));
    Some(format!("Basic {encoded}"))
}

async fn run_sql(
    client: &Client,
    url: &str,
    settings: &ServerSettings,
    auth: Option<&str>,
    query: &str,
) -> Result<JsonValue, String> {
    let mut request = client
        .post(url)
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "text/plain")
        .body(query.to_string());

    if let Some(namespace) = &settings.connection.namespace {
        request = request.header("Surreal-NS", namespace);
    }
    if let Some(database) = &settings.connection.database {
        request = request.header("Surreal-DB", database);
    }
    if let Some(value) = auth {
        request = request.header(AUTHORIZATION, value);
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("HTTP request failed: {error}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("failed to read HTTP response: {error}"))?;

    if !status.is_success() {
        return Err(format!("SurrealDB returned HTTP {status}: {body}"));
    }

    serde_json::from_str(&body).map_err(|error| format!("failed to decode JSON response: {error}"))
}

/// SurrealDB's `/sql` endpoint returns an array of statement results, each
/// `{ "status": "OK" | "ERR", "result": <value>, "time": "..." }`. The first
/// statement in our query is what we care about.
fn first_result_value(response: &JsonValue) -> Option<&JsonValue> {
    let first = response.as_array()?.first()?;
    if first.get("status").and_then(JsonValue::as_str) == Some("OK") {
        first.get("result")
    } else {
        None
    }
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
    use super::sql_endpoint_url;
    use pretty_assertions::assert_eq;

    #[test]
    fn normalizes_websocket_rpc_endpoint() {
        assert_eq!(
            sql_endpoint_url("ws://127.0.0.1:8000/rpc").unwrap(),
            "http://127.0.0.1:8000/sql"
        );
    }

    #[test]
    fn normalizes_secure_websocket_endpoint() {
        assert_eq!(
            sql_endpoint_url("wss://example.com/rpc").unwrap(),
            "https://example.com/sql"
        );
    }

    #[test]
    fn keeps_https_scheme() {
        assert_eq!(
            sql_endpoint_url("https://db.example.com:443/").unwrap(),
            "https://db.example.com:443/sql"
        );
    }

    #[test]
    fn defaults_to_http_when_scheme_missing() {
        assert_eq!(
            sql_endpoint_url("127.0.0.1:8000").unwrap(),
            "http://127.0.0.1:8000/sql"
        );
    }

    #[test]
    fn rejects_unsupported_scheme() {
        assert!(sql_endpoint_url("ftp://example.com").is_err());
    }
}
