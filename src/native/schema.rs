//! `surrealql-language-server schema`: the workspace's schema, as data.
//!
//! An agent writing SurrealQL against an unfamiliar database invents table and
//! field names, because nothing tells it what exists. `check` catches some of
//! that after the fact; this hands over the answer before the fact, from the
//! same merged model the editor uses.
//!
//! Two formats, for two consumers:
//!
//! * `--format json` is the machine one, versioned from day one so a consumer
//!   can branch on `schemaVersion` rather than sniff.
//! * `--format llm` is compact SurrealQL-shaped prose. A model reads DDL better
//!   than it reads JSON, and the token cost of the JSON envelope is real when
//!   this goes into a prompt for every request.
//!
//! Like `check`, this never connects to a database: the schema comes from the
//! `.surql` files it was pointed at.

use std::path::PathBuf;
use std::process::ExitCode;

use serde::Serialize;

use crate::core::client::WorkspaceLoader;
use crate::native::workspace_fs::FilesystemWorkspaceLoader;
use crate::semantic::types::{LiveMetadataSnapshot, MergedSemanticModel, SymbolOrigin};

pub const USAGE: &str = "\
Usage: surrealql-language-server schema [PATHS...] [OPTIONS]

Print the schema the workspace defines: tables, their fields and types,
permissions, events, indexes and functions.

Arguments:
  [PATHS...]             Directories or files to read definitions from.
                         Defaults to the current directory.

Options:
      --format <llm|json>  Output format (default: llm).
  -h, --help               Print this help.

Exit codes:
  0  Printed a schema.
  2  Usage error, or nothing readable at the given paths.

`schema` never connects to a database; it reads .surql files.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaFormat {
    Llm,
    Json,
}

#[derive(Debug)]
pub struct SchemaOptions {
    pub paths: Vec<PathBuf>,
    pub format: SchemaFormat,
}

#[derive(Debug)]
pub enum Parsed {
    Run(SchemaOptions),
    Help,
}

pub fn parse_args(args: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut options = SchemaOptions {
        paths: Vec::new(),
        format: SchemaFormat::Llm,
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--format" => {
                let value = args
                    .next()
                    .ok_or_else(|| "`--format` needs a value".to_string())?;
                options.format = match value.as_str() {
                    "llm" => SchemaFormat::Llm,
                    "json" => SchemaFormat::Json,
                    other => return Err(format!("unknown format `{other}` (llm or json)")),
                };
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`"));
            }
            path => options.paths.push(PathBuf::from(path)),
        }
    }

    if options.paths.is_empty() {
        options.paths.push(PathBuf::from("."));
    }
    Ok(Parsed::Run(options))
}

// ── The JSON shape ────────────────────────────────────────────────────
//
// A compatibility surface from the first release, like `builtins.json` and the
// `check` report. Additive changes only; `schemaVersion` moves if that ever
// stops being possible.

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaReport {
    pub schema_version: u32,
    pub version: String,
    pub tables: Vec<TableReport>,
    pub functions: Vec<FunctionReport>,
    pub params: Vec<ParamReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableReport {
    pub name: String,
    /// `SCHEMAFULL`, `SCHEMALESS`, or absent when the definition says neither.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_mode: Option<String>,
    /// False when this table was inferred from a query rather than defined.
    ///
    /// Worth knowing before you write against it: an inferred table is a guess
    /// about what the queries imply, not a promise about what exists.
    pub explicit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub fields: Vec<FieldReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldReport {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    pub explicit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionReport {
    pub name: String,
    pub parameters: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returns: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamReport {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
}

/// Build the report from a merged model.
pub fn report(model: &MergedSemanticModel) -> SchemaReport {
    let mut tables: Vec<TableReport> = model
        .tables
        .values()
        .filter(|table| table.origin == SymbolOrigin::Local)
        .map(|table| {
            let mut fields: Vec<FieldReport> = model
                .fields
                .get(&table.name)
                .into_iter()
                .flat_map(|fields| fields.values())
                .map(|field| FieldReport {
                    name: field.name.clone(),
                    r#type: field.type_expr.as_ref().map(|ty| ty.to_string()),
                    explicit: field.explicit,
                    comment: field.comment.clone(),
                })
                .collect();
            fields.sort_by(|a, b| a.name.cmp(&b.name));

            let mut indexes: Vec<String> = model
                .indexes
                .iter()
                .filter(|((owner, _), _)| *owner == table.name)
                .map(|((_, name), _)| name.clone())
                .collect();
            indexes.sort();

            let mut events: Vec<String> = model
                .events
                .iter()
                .filter(|((owner, _), _)| *owner == table.name)
                .map(|((_, name), _)| name.clone())
                .collect();
            events.sort();

            TableReport {
                name: table.name.clone(),
                schema_mode: table.schema_mode.clone(),
                explicit: table.explicit,
                comment: table.comment.clone(),
                fields,
                // The clause as written. A `Debug`-formatted mode would leak
                // an internal enum spelling into a compatibility surface, and
                // the raw text is what a reader (human or model) can act on.
                permissions: table
                    .permissions
                    .iter()
                    .map(|rule| rule.raw.trim().to_string())
                    .filter(|raw| !raw.is_empty())
                    .collect(),
                indexes,
                events,
            }
        })
        .collect();
    tables.sort_by(|a, b| a.name.cmp(&b.name));

    let mut functions: Vec<FunctionReport> = model
        .functions
        .values()
        .filter(|function| function.origin == SymbolOrigin::Local)
        .map(|function| FunctionReport {
            name: function.name.clone(),
            parameters: function
                .params
                .iter()
                // `FunctionParam.name` already carries its `$`.
                .map(|parameter| {
                    let name = parameter.name.trim_start_matches('$');
                    match &parameter.type_expr {
                        Some(ty) => format!("${name}: {ty}"),
                        None => format!("${name}"),
                    }
                })
                .collect(),
            returns: function.return_type.as_ref().map(ToString::to_string),
            comment: function.comment.clone(),
        })
        .collect();
    functions.sort_by(|a, b| a.name.cmp(&b.name));

    let mut params: Vec<ParamReport> = model
        .params
        .values()
        .filter(|param| param.origin == SymbolOrigin::Local)
        .map(|param| ParamReport {
            name: param.name.clone(),
            // `DEFINE PARAM` has a value, not a declared type; the preview is
            // what the definition actually says.
            r#type: param.value_preview.clone(),
        })
        .collect();
    params.sort_by(|a, b| a.name.cmp(&b.name));

    SchemaReport {
        schema_version: 1,
        version: crate::core::server::build_version(),
        tables,
        functions,
        params,
    }
}

/// Render the report as compact SurrealQL-shaped prose.
///
/// Shaped like DDL because that is the form a model has seen most of, and
/// because it is denser than JSON: this goes into a prompt, where the envelope
/// is a real cost.
pub fn render_llm(report: &SchemaReport) -> String {
    let mut out = String::new();

    if report.tables.is_empty() && report.functions.is_empty() {
        return "-- No SurrealQL definitions found.\n".to_string();
    }

    for table in &report.tables {
        if let Some(comment) = &table.comment {
            out.push_str(&format!("-- {comment}\n"));
        }
        if !table.explicit {
            out.push_str("-- (inferred from queries; not defined anywhere)\n");
        }
        let mode = table
            .schema_mode
            .as_deref()
            .map(|mode| format!(" {}", mode.to_uppercase()))
            .unwrap_or_default();
        out.push_str(&format!("DEFINE TABLE {}{mode};\n", table.name));

        for field in &table.fields {
            let ty = field
                .r#type
                .as_deref()
                .map(|ty| format!(" TYPE {ty}"))
                .unwrap_or_default();
            let inferred = if field.explicit { "" } else { "  -- inferred" };
            out.push_str(&format!(
                "DEFINE FIELD {} ON {}{ty};{inferred}\n",
                field.name, table.name
            ));
        }
        for permission in &table.permissions {
            out.push_str(&format!("-- {permission}\n"));
        }
        if !table.indexes.is_empty() {
            out.push_str(&format!("-- indexes: {}\n", table.indexes.join(", ")));
        }
        if !table.events.is_empty() {
            out.push_str(&format!("-- events: {}\n", table.events.join(", ")));
        }
        out.push('\n');
    }

    for function in &report.functions {
        if let Some(comment) = &function.comment {
            out.push_str(&format!("-- {comment}\n"));
        }
        let returns = function
            .returns
            .as_deref()
            .map(|ty| format!(" -> {ty}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "DEFINE FUNCTION {}({}){returns} {{ … }};\n",
            function.name,
            function.parameters.join(", ")
        ));
    }
    if !report.functions.is_empty() {
        out.push('\n');
    }

    for param in &report.params {
        let ty = param
            .r#type
            .as_deref()
            .map(|ty| format!(" TYPE {ty}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "DEFINE PARAM ${}{ty};\n",
            param.name.trim_start_matches('$')
        ));
    }

    out
}

pub async fn run(options: SchemaOptions) -> ExitCode {
    let index = FilesystemWorkspaceLoader::new().load(&options.paths).await;
    if index.documents.is_empty() {
        eprintln!(
            "error: no readable .surql files under {}",
            options
                .paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        return ExitCode::from(2);
    }

    let model = MergedSemanticModel::build(&index, &LiveMetadataSnapshot::default());
    let report = report(&model);

    match options.format {
        SchemaFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
            );
        }
        SchemaFormat::Llm => print!("{}", render_llm(&report)),
    }
    ExitCode::SUCCESS
}
