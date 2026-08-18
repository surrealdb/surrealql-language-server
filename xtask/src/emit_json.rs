//! Render the catalogue as `builtins.json`.
//!
//! The JSON is a published artifact: attached to every GitHub release and
//! shipped in the npm package, so anyone building SurrealQL tooling (AI
//! agents included) gets the engine-derived ground truth without scraping
//! documentation. The shape is a compatibility surface — `tests/compat.rs`
//! pins one entry exactly, and changes must be additive.
//!
//! One deliberate encoding: **an unknown signature is `params: null`, never
//! `[]`.** The Rust catalogue carries a separate `signature_known` flag, and
//! every consumer must remember to read it or it reports "expects 0
//! arguments" for functions the generator could not read. Making the unknown
//! state structurally distinct removes that trap for JSON consumers.

use serde::Serialize;

use crate::emit::Catalogue;
use crate::kinds::ParamForm;

/// Format version for the file itself, independent of the language-server
/// release that produced it. Bump only for a breaking shape change.
const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Meta<'a> {
    schema_version: u32,
    language_server_version: &'a str,
    surrealdb_revision: &'a str,
    generated_by: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Function<'a> {
    name: &'a str,
    /// `None` (JSON `null`) when the generator could not read the
    /// implementation — unknown, not zero-arity.
    params: Option<Vec<Param<'a>>>,
    is_async: bool,
    not_callable: bool,
    returns: &'a str,
}

#[derive(Serialize)]
struct Param<'a> {
    name: &'a str,
    #[serde(rename = "type")]
    ty: &'a str,
    form: &'static str,
}

#[derive(Serialize)]
struct Rename<'a> {
    old: &'a str,
    new: &'a str,
}

#[derive(Serialize)]
struct Receiver<'a> {
    /// The engine `Value` variant, or `null` for the catch-all table
    /// (the empty string in the Rust catalogue).
    kind: Option<&'a str>,
    methods: Vec<Method<'a>>,
}

#[derive(Serialize)]
struct Method<'a> {
    method: &'a str,
    function: &'a str,
    experimental: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Document<'a> {
    meta: Meta<'a>,
    functions: Vec<Function<'a>>,
    constants: &'a [String],
    renames: Vec<Rename<'a>>,
    namespaces: Vec<&'a str>,
    receivers: Vec<Receiver<'a>>,
}

/// Render the whole catalogue. `language_server_version` is the root crate's
/// version (xtask's own is a meaningless `0.0.0`), so a consumer can tell
/// which release produced the file.
pub fn render(catalogue: &Catalogue, language_server_version: &str) -> String {
    let document = Document {
        meta: Meta {
            schema_version: SCHEMA_VERSION,
            language_server_version,
            surrealdb_revision: &catalogue.revision,
            generated_by: "cargo xtask generate-builtins",
        },
        functions: catalogue
            .functions
            .iter()
            .map(|entry| Function {
                name: &entry.name,
                params: entry.signature_known.then(|| {
                    entry
                        .params
                        .iter()
                        .map(|param| Param {
                            name: &param.name,
                            ty: &param.ty,
                            form: match param.form {
                                ParamForm::Required => "required",
                                ParamForm::Optional => "optional",
                                ParamForm::Variadic => "variadic",
                            },
                        })
                        .collect()
                }),
                is_async: entry.is_async,
                not_callable: entry.not_callable,
                returns: &entry.returns,
            })
            .collect(),
        constants: &catalogue.constants,
        renames: catalogue
            .renames
            .iter()
            .map(|(old, new)| Rename { old, new })
            .collect(),
        namespaces: catalogue.namespaces.iter().map(String::as_str).collect(),
        receivers: catalogue
            .receivers
            .iter()
            .map(|receiver| Receiver {
                kind: (!receiver.kind.is_empty()).then_some(receiver.kind.as_str()),
                methods: receiver
                    .methods
                    .iter()
                    .map(|(method, function, experimental)| Method {
                        method,
                        function,
                        experimental: experimental.as_deref(),
                    })
                    .collect(),
            })
            .collect(),
    };
    let mut rendered = serde_json::to_string_pretty(&document)
        .expect("the catalogue serializes: no maps with non-string keys, no floats");
    rendered.push('\n');
    rendered
}
