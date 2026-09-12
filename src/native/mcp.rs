//! `surrealql-language-server mcp`: the Model Context Protocol over stdio.
//!
//! The same analysis `check` and the editor run, reachable as tools an agent
//! calls directly rather than by shelling out and parsing output. Five of them,
//! each a thin adapter over machinery that already exists and is already tested:
//!
//! | Tool | Backed by |
//! | --- | --- |
//! | `validate_surrealql` | the analyzer and merged model |
//! | `get_schema` | [`crate::native::schema`] |
//! | `lookup_function` / `search_functions` | the engine-generated catalogue |
//! | `explain_diagnostic` | [`crate::native::check::explain`] |
//!
//! # No dependency
//!
//! MCP is JSON-RPC 2.0 with a small method set (`initialize`, `tools/list`,
//! `tools/call`) carried as newline-delimited JSON over stdio. Implementing it
//! directly is a few hundred lines and keeps the rule this repository already
//! follows for its argument parsers and its LSP dispatcher: no dependency enters
//! the graph for something this size. `[dependencies]` is also the wasm
//! dependency graph, and an MCP crate has no business there.
//!
//! # Compatibility
//!
//! Tool names and their input schemas are a **permanent** surface: an agent
//! keys on them and a rename breaks it silently. They are pinned by
//! `tests/compat.rs`, and the same rule applies as everywhere else here:
//! additive changes only.
//!
//! Like `check` and `schema`, this never connects to a database.

use std::fs;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::config::ServerSettings;
use crate::core::client::WorkspaceLoader;
use crate::native::workspace_fs::FilesystemWorkspaceLoader;
use crate::semantic::types::{
    LiveMetadataSnapshot, MergedSemanticModel, SymbolOrigin, WorkspaceIndex,
};

pub const USAGE: &str = "\
Usage: surrealql-language-server mcp [OPTIONS]

Serve the Model Context Protocol over stdio, so an agent can validate
SurrealQL, read the schema and look up builtins as tool calls.

Options:
      --workspace <dir>  Read schema definitions from this directory.
                         Repeatable. Without one, only the tools that need
                         no schema are useful.
      --config <file>    The same settings file `check --config` takes.
  -h, --help             Print this help.

The workspace is re-read before every tool that answers from the schema, so a
file this agent has just written is in scope for the next call.

`mcp` never connects to a database.";

/// The MCP revision this server implements.
const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug)]
pub struct McpOptions {
    pub workspace_dirs: Vec<PathBuf>,
    /// The same `--config` the `check` subcommand takes, so a workspace whose
    /// settings declare external params or turn a check off behaves the same
    /// way through an agent as it does through an editor.
    pub config: Option<PathBuf>,
}

#[derive(Debug)]
pub enum Parsed {
    Run(McpOptions),
    Help,
}

pub fn parse_args(args: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut options = McpOptions {
        workspace_dirs: Vec::new(),
        config: None,
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--workspace" => {
                let value = args
                    .next()
                    .ok_or_else(|| "`--workspace` needs a value".to_string())?;
                options.workspace_dirs.push(PathBuf::from(value));
            }
            "--config" => {
                let value = args
                    .next()
                    .ok_or_else(|| "`--config` needs a value".to_string())?;
                options.config = Some(PathBuf::from(value));
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(Parsed::Run(options))
}

/// Every tool, with the schema an agent validates its call against.
///
/// Descriptions are written for a model deciding *whether* to call, not for a
/// human reading reference docs: each says what the tool answers and when it is
/// the right one.
pub fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "validate_surrealql",
            "description":
                "Check a SurrealQL query or schema for errors before running it. Returns \
                 diagnostics with stable codes, 0-based line and UTF-16 column ranges, and a \
                 documentation link per code. Checked against the workspace's schema, so it \
                 catches misspelled tables and fields as well as syntax and type errors. Call \
                 this after writing or editing any SurrealQL.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The SurrealQL to check.",
                    },
                    "params": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description":
                            "Variables the caller binds at run time, without the `$`. Declaring \
                             them stops `undefined-variable` being reported for them.",
                    },
                },
                "required": ["query"],
            },
        }),
        json!({
            "name": "get_schema",
            "description":
                "Read the tables, fields, types, permissions, indexes and functions the \
                 workspace defines. Call this BEFORE writing SurrealQL against an unfamiliar \
                 database, rather than guessing names and correcting afterwards. Marks tables \
                 that were inferred from queries rather than defined.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "format": {
                        "type": "string",
                        "enum": ["llm", "json"],
                        "description":
                            "`llm` (default) is compact SurrealQL-shaped DDL; `json` is the \
                             versioned machine shape.",
                    },
                },
            },
        }),
        json!({
            "name": "lookup_function",
            "description":
                "Get one SurrealDB builtin's exact signature, argument types, arity and return \
                 type, from the engine's own source. Use it instead of recalling a function's \
                 shape. Also reports when a name has been renamed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Full name, such as `string::concat` or `time::now`.",
                    },
                },
                "required": ["name"],
            },
        }),
        json!({
            "name": "search_functions",
            "description":
                "Find SurrealDB builtins whose name contains a substring: a namespace like \
                 `time::`, or a word like `concat`. Use it to discover what exists before \
                 inventing a function that does not.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Substring to match against function names.",
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum results (default 50).",
                    },
                },
                "required": ["query"],
            },
        }),
        json!({
            "name": "explain_diagnostic",
            "description":
                "Explain one diagnostic code from `validate_surrealql`: what it means, why \
                 SurrealDB refuses the query, and what fixing it looks like. Call it when a \
                 diagnostic's message alone does not say how to repair the query.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "A diagnostic code, such as `unknown-table`.",
                    },
                },
                "required": ["code"],
            },
        }),
    ]
}

/// The schema the tools answer against.
struct Context {
    workspace: WorkspaceIndex,
    model: MergedSemanticModel,
    settings: ServerSettings,
    dirs: Vec<PathBuf>,
}

impl Context {
    async fn load(options: &McpOptions) -> Result<Self, String> {
        let config =
            match &options.config {
                Some(path) => {
                    let text = fs::read_to_string(path)
                        .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
                    Some(serde_json::from_str::<Value>(&text).map_err(|error| {
                        format!("`{}` is not valid JSON: {error}", path.display())
                    })?)
                }
                None => None,
            };
        // The same parser the LSP and `check` paths use, so a config file an
        // editor accepts is accepted here too.
        let (settings, _warnings) =
            ServerSettings::from_sources_with_warnings(config.as_ref(), None);

        let mut context = Self {
            workspace: WorkspaceIndex::default(),
            model: MergedSemanticModel::build(
                &WorkspaceIndex::default(),
                &LiveMetadataSnapshot::default(),
            ),
            settings,
            dirs: options.workspace_dirs.clone(),
        };
        context.reload().await;
        Ok(context)
    }

    /// Re-read the workspace from disk.
    ///
    /// Called before every tool that answers from the schema, because the agent
    /// on the other end of this connection is the thing most likely to have
    /// just changed it: writing a `DEFINE TABLE` and then asking `get_schema`
    /// what the schema is, and being told what it was at startup, is the
    /// obvious way for an agent to talk itself into a wrong edit.
    ///
    /// One walk plus one model build, on a call an agent makes deliberately
    /// rather than per keystroke, so the cost lands in the right place.
    async fn reload(&mut self) {
        self.workspace = FilesystemWorkspaceLoader::new().load(&self.dirs).await;
        self.model = MergedSemanticModel::build(&self.workspace, &LiveMetadataSnapshot::default());
    }
}

/// Whether a tool reads the workspace schema, and therefore needs it fresh.
///
/// The catalogue-only tools (`lookup_function`, `search_functions`,
/// `explain_diagnostic`) answer from data compiled into the binary and cannot
/// go stale, so they skip the walk.
fn reads_the_workspace(tool: &str) -> bool {
    matches!(tool, "validate_surrealql" | "get_schema")
}

/// Run one tool, returning its text content.
///
/// Errors are returned as `Err(message)` and rendered as an MCP tool error:
/// a *result* with `isError`, not a protocol error, because a tool that cannot
/// answer is not a broken connection.
fn call_tool(context: &Context, name: &str, arguments: &Value) -> Result<String, String> {
    let string_arg = |key: &str| -> Result<String, String> {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("`{key}` is required and must be a string"))
    };

    match name {
        "validate_surrealql" => {
            let query = string_arg("query")?;
            let params: Vec<String> = arguments
                .get("params")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();

            // The workspace's own settings, with the caller's params added
            // rather than replacing them: `--config` names what the project
            // binds, the call names what this query binds.
            let mut settings = context.settings.clone();
            settings.analysis.external_params.extend(params);

            let Ok(uri) = "file:///surrealql/validate".parse() else {
                return Err("could not build a document uri".to_string());
            };
            let Some(analysis) = crate::semantic::analyzer::analyze_document_with_limit(
                uri,
                query,
                SymbolOrigin::Local,
                settings.analysis.max_syntax_diagnostics,
            ) else {
                return Err("the SurrealQL grammar could not be loaded".to_string());
            };

            let diagnostics = context.model.document_diagnostics(&analysis, &settings);
            if diagnostics.is_empty() {
                return Ok("No problems found.".to_string());
            }
            serde_json::to_string_pretty(&json!({
                "diagnostics": diagnostics,
                "count": diagnostics.len(),
            }))
            .map_err(|error| error.to_string())
        }

        "get_schema" => {
            if context.workspace.documents.is_empty() {
                return Err(
                    "No schema is loaded. Start the server with `--workspace <dir>` pointing at \
                     the directory holding your .surql definitions."
                        .to_string(),
                );
            }
            let report = crate::native::schema::report(&context.model);
            match arguments.get("format").and_then(Value::as_str) {
                Some("json") => {
                    serde_json::to_string_pretty(&report).map_err(|error| error.to_string())
                }
                _ => Ok(crate::native::schema::render_llm(&report)),
            }
        }

        "lookup_function" => {
            let name = string_arg("name")?;
            let trimmed = name.trim().trim_end_matches("()");

            if let Some(current) = crate::grammar::renamed_builtin(trimmed) {
                return Ok(format!(
                    "`{trimmed}` has been renamed to `{current}`. SurrealDB still accepts the old \
                     name, but write the new one.\n\n{}",
                    describe_function(current).unwrap_or_default()
                ));
            }
            describe_function(trimmed).ok_or_else(|| {
                format!(
                    "`{trimmed}` is not a SurrealDB builtin. Use `search_functions` to find what \
                     exists."
                )
            })
        }

        "search_functions" => {
            let query = string_arg("query")?.to_ascii_lowercase();
            let limit = arguments
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(50)
                .clamp(1, 500) as usize;

            let mut matches: Vec<&str> = crate::grammar_generated::GENERATED_FUNCTIONS
                .iter()
                .map(|function| function.name)
                .filter(|name| name.to_ascii_lowercase().contains(&query))
                .collect();
            matches.sort_unstable();

            if matches.is_empty() {
                return Ok(format!("No builtin matches `{query}`."));
            }
            let total = matches.len();
            let shown: Vec<String> = matches
                .into_iter()
                .take(limit)
                .map(|name| describe_function(name).unwrap_or_else(|| name.to_string()))
                .collect();

            let mut out = shown.join("\n");
            if total > limit {
                out.push_str(&format!("\n\n… and {} more.", total - limit));
            }
            Ok(out)
        }

        "explain_diagnostic" => {
            let code = string_arg("code")?;
            crate::native::check::explain(code.trim()).ok_or_else(|| {
                format!(
                    "`{code}` is not a diagnostic code. Known codes: {}",
                    crate::native::check::known_codes().join(", ")
                )
            })
        }

        other => Err(format!("unknown tool `{other}`")),
    }
}

/// One line describing a builtin's shape, from the generated catalogue.
fn describe_function(name: &str) -> Option<String> {
    let function = crate::grammar_generated::GENERATED_FUNCTIONS
        .iter()
        .find(|candidate| candidate.name == name)?;

    // `signature_known: false` means the generator could not read the
    // implementation. Printing `()` there would claim zero arity, which is a
    // different and wrong statement: see `GeneratedFunction::signature_known`.
    let params = if function.signature_known {
        function
            .params
            .iter()
            .map(|param| format!("{}: {}", param.name, param.ty))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        "…signature unknown".to_string()
    };

    let mut line = format!("{}({params}) -> {}", function.name, function.returns);
    if function.not_callable {
        line.push_str("   [parses, but no implementation backs it in call form]");
    }
    if function.is_async {
        line.push_str("   [async]");
    }
    Some(line)
}

/// Serve MCP over stdio until the client closes it.
pub async fn run(options: McpOptions) -> ExitCode {
    let mut context = match Context::load(&options).await {
        Ok(context) => context,
        Err(message) => {
            // Before the handshake there is no client to send an error to, so
            // this goes to stderr and the process stops rather than serving a
            // schema built from settings nobody asked for.
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if requested_tool(line)
            .as_deref()
            .is_some_and(reads_the_workspace)
        {
            context.reload().await;
        }

        let Some(response) = handle_message(&context, line) else {
            // A notification. MCP requires no reply, and sending one would be a
            // protocol error.
            continue;
        };
        if writeln!(stdout, "{response}").is_err() || stdout.flush().is_err() {
            break;
        }
    }
    ExitCode::SUCCESS
}

/// The tool a `tools/call` message names, if it is one.
///
/// Read before dispatch so the reload can be `await`ed: `handle_message` is
/// synchronous, and keeping it that way keeps every tool a pure function of the
/// context it is handed.
fn requested_tool(message: &str) -> Option<String> {
    let request: Value = serde_json::from_str(message).ok()?;
    if request.get("method").and_then(Value::as_str)? != "tools/call" {
        return None;
    }
    request
        .get("params")?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

/// Handle one JSON-RPC message, returning the response to write, if any.
fn handle_message(context: &Context, message: &str) -> Option<String> {
    let request: Value = match serde_json::from_str(message) {
        Ok(value) => value,
        // -32700 is JSON-RPC's parse error. The id is unknowable, so it is null.
        Err(error) => {
            return Some(error_response(
                Value::Null,
                -32700,
                &format!("could not parse message: {error}"),
            ));
        }
    };

    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    // No id means a notification: act on it, answer nothing. Replying to one is
    // a protocol error, and a client that sees an unexpected response can wedge.
    let id = id?;

    match method {
        "initialize" => Some(result_response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "surrealql-language-server",
                    "version": crate::core::server::build_version(),
                },
            }),
        )),

        "tools/list" => Some(result_response(id, json!({ "tools": tools() }))),

        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            match call_tool(context, name, &arguments) {
                Ok(text) => Some(result_response(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )),
                // A tool that cannot answer is a *result* carrying `isError`,
                // not a JSON-RPC error: the call was well-formed and the
                // connection is fine, and the model needs to read why.
                Err(message) => Some(result_response(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": message }],
                        "isError": true,
                    }),
                )),
            }
        }

        "ping" => Some(result_response(id, json!({}))),

        other => Some(error_response(
            id,
            -32601,
            &format!("unknown method `{other}`"),
        )),
    }
}

fn result_response(id: Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error_response(id: Value, code: i32, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}
