//! SURREALDB_* environment-fallback compat test.
//!
//! Lives in its own test binary on purpose: it mutates process
//! environment variables (unsafe in edition 2024) and other suites
//! call `from_sources`/`merge_with_env` — which read those variables —
//! from parallel test threads. A separate binary is a separate
//! process, so no other test can observe the mutation.

use surrealql_language_server::config::ServerSettings;

/// SURREALDB_* environment fallbacks. All env mutation lives in this
/// single test (edition 2024 makes `set_var` unsafe and the test
/// runner is multi-threaded — sentinel values + restore keep it
/// hermetic enough).
#[test]
fn environment_variable_fallbacks_are_stable() {
    let vars = [
        ("SURREALDB_ENDPOINT", "ws://env-endpoint:8000/rpc"),
        ("SURREALDB_NAMESPACE", "env-ns"),
        ("SURREALDB_DATABASE", "env-db"),
        ("SURREALDB_USERNAME", "env-user"),
        ("SURREALDB_PASSWORD", "env-pass"),
        ("SURREALDB_TOKEN", "env-token"),
    ];
    let previous: Vec<_> = vars
        .iter()
        .map(|(name, value)| {
            let old = std::env::var(name).ok();
            unsafe { std::env::set_var(name, value) };
            (*name, old)
        })
        .collect();

    let settings = ServerSettings::from_sources(None, None);

    for (name, old) in previous {
        match old {
            Some(value) => unsafe { std::env::set_var(name, value) },
            None => unsafe { std::env::remove_var(name) },
        }
    }

    assert_eq!(
        settings.connection.endpoint.as_deref(),
        Some("ws://env-endpoint:8000/rpc")
    );
    assert_eq!(settings.connection.namespace.as_deref(), Some("env-ns"));
    assert_eq!(settings.connection.database.as_deref(), Some("env-db"));
    assert_eq!(settings.connection.username.as_deref(), Some("env-user"));
    assert_eq!(settings.connection.password.as_deref(), Some("env-pass"));
    assert_eq!(settings.connection.token.as_deref(), Some("env-token"));
}
