//! The receiver tables that decide which function a `value.method()` call runs.
//!
//! SurrealQL lets most builtins be called as a method, and the mapping from
//! method to function is **not** `<receiver type>::<method>`. `(5).round()` is
//! `math::round`, `123.to_float()` is `type::float`, `"abc".is_alphanum()` is
//! `string::is_alphanum`. A language server that guesses the convention gets
//! roughly half of them.
//!
//! The whole mapping lives in one engine function, `build_method_registry` in
//! `exec/function/method.rs`, as a list of registrations:
//!
//! ```text
//! m.register_generic("to_float", get(funcs, "type::float"));
//! m.register_typed("len", ValueKind::String, get(funcs, "string::len"));
//! m.register_alias("every", "all");
//! ```
//!
//! A typed method serves one `ValueKind` and wins over a generic one of the same
//! name. A generic method is the fallback for every receiver, so it is in every
//! table here, and alone in the catch-all. The calls wrap across lines, so this
//! reads tokens rather than lines.

use std::collections::BTreeMap;
use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use syn::Item;

/// One method, and the SurrealQL function it runs.
#[derive(Debug, Clone)]
pub struct MethodArm {
    /// The name written after the dot.
    pub method: String,
    /// The function name an author would write, such as `string::is_alphanum`.
    pub function: String,
}

/// The methods available on one receiver.
#[derive(Debug, Clone)]
pub struct Receiver {
    /// The engine `Value` variant, or an empty string for the catch-all table
    /// that serves `bool`, `uuid`, `regex`, `range`, `none` and the rest.
    pub kind: String,
    pub methods: Vec<MethodArm>,
}

/// Read every receiver table out of `exec/function/method.rs`.
pub fn parse(method_rs: &Path) -> Result<Vec<Receiver>, String> {
    let source = std::fs::read_to_string(method_rs)
        .map_err(|error| format!("cannot read {}: {error}", method_rs.display()))?;
    let file = syn::parse_file(&source)
        .map_err(|error| format!("cannot parse {}: {error}", method_rs.display()))?;
    let registry = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(function) if function.sig.ident == "build_method_registry" => Some(function),
            _ => None,
        })
        .ok_or_else(|| {
            format!(
                "no `fn build_method_registry` in {} — the engine's method dispatch moved",
                method_rs.display()
            )
        })?;

    let mut calls = Vec::new();
    registrations(registry.block.to_token_stream(), &mut calls);

    let mut generic: BTreeMap<String, String> = BTreeMap::new();
    let mut typed: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (register, args) in calls {
        let strings = string_literals(args.clone());
        match (register.as_str(), strings.as_slice(), value_kind(args)) {
            ("register_generic", [method, function], None) => {
                generic.insert(method.clone(), function.clone());
            }
            // The engine checks typed implementations in order, so the first wins.
            ("register_typed", [method, function], Some(kind)) => {
                typed
                    .entry(kind)
                    .or_default()
                    .entry(method.clone())
                    .or_insert_with(|| function.clone());
            }
            // The alias becomes a copy of the target, replacing whatever it held.
            ("register_alias", [alias, target], None) => {
                for table in std::iter::once(&mut generic).chain(typed.values_mut()) {
                    match table.get(target).cloned() {
                        Some(function) => table.insert(alias.clone(), function),
                        None => table.remove(alias),
                    };
                }
            }
            _ => {
                return Err(format!(
                    "unrecognised method registration `{register}({})` in {}",
                    strings.join(", "),
                    method_rs.display()
                ));
            }
        }
    }

    let arms = |table: BTreeMap<String, String>| {
        table
            .into_iter()
            .map(|(method, function)| MethodArm { method, function })
            .collect()
    };
    let mut receivers: Vec<Receiver> = typed
        .into_iter()
        .map(|(kind, methods)| {
            let mut table = generic.clone();
            table.extend(methods);
            Receiver {
                // `ValueKind::Record` classifies `Value::RecordId`, the name the
                // tables have always used.
                kind: if kind == "Record" {
                    "RecordId".to_string()
                } else {
                    kind
                },
                methods: arms(table),
            }
        })
        .collect();
    receivers.push(Receiver {
        kind: String::new(),
        methods: arms(generic),
    });

    if receivers.len() < 11 {
        return Err(format!(
            "found only {} receiver tables, expected at least 11",
            receivers.len()
        ));
    }
    let total: usize = receivers.iter().map(|entry| entry.methods.len()).sum();
    if total < 700 {
        return Err(format!(
            "read only {total} method arms, expected about 820 — the engine's \
             dispatch layout probably changed, so the tables are not trustworthy"
        ));
    }

    Ok(receivers)
}

/// Every `register_*( … )` call in `tokens`, as its name and argument tokens.
fn registrations(tokens: TokenStream, out: &mut Vec<(String, TokenStream)>) {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    for (index, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Ident(ident) if ident.to_string().starts_with("register_") => {
                if let Some(TokenTree::Group(args)) = tokens.get(index + 1) {
                    out.push((ident.to_string(), args.stream()));
                }
            }
            // The file methods sit inside an `if`.
            TokenTree::Group(group) => registrations(group.stream(), out),
            _ => {}
        }
    }
}

/// The string literals in `tokens`, in order, including those inside `get(…)`.
fn string_literals(tokens: TokenStream) -> Vec<String> {
    tokens
        .into_iter()
        .flat_map(|token| match token {
            TokenTree::Literal(literal) => {
                let text = literal.to_string();
                text.strip_prefix('"')
                    .and_then(|rest| rest.strip_suffix('"'))
                    .map(str::to_string)
                    .into_iter()
                    .collect()
            }
            TokenTree::Group(group) => string_literals(group.stream()),
            _ => Vec::new(),
        })
        .collect()
}

/// `String` out of `ValueKind::String`.
fn value_kind(tokens: TokenStream) -> Option<String> {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    tokens
        .windows(4)
        .find_map(|window| match (&window[0], &window[3]) {
            (TokenTree::Ident(head), TokenTree::Ident(kind)) if head == "ValueKind" => {
                Some(kind.to_string())
            }
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_method_wins_and_an_alias_copies_its_target() {
        let dir = std::env::temp_dir().join("xtask-methods");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("method.rs");
        let tables: String = ["Array", "Bytes", "Datetime", "Duration", "File", "Geometry"]
            .iter()
            .chain(&["Number", "Object", "Record", "Set", "String"])
            .map(|kind| {
                format!("m.register_typed(\"len\", ValueKind::{kind}, get(funcs, \"x::len\"));")
            })
            .collect();
        let generic: String = (0..70)
            .map(|n| format!("m.register_generic(\"g{n}\", get(funcs, \"type::g{n}\"));"))
            .collect();
        std::fs::write(
            &file,
            format!(
                "fn build_method_registry(funcs: &F) -> M {{
                    {generic}
                    m.register_generic(\"to_string\", get(funcs, \"type::string\"));
                    m.register_typed(
                        \"to_string\",
                        ValueKind::Array,
                        get(funcs, \"array::to_string\"),
                    );
                    {tables}
                    if funcs.get(\"file::bucket\").is_some() {{
                        m.register_typed(\"bucket\", ValueKind::File, get(funcs, \"file::bucket\"));
                    }}
                    m.register_alias(\"size\", \"len\");
                    m
                }}"
            ),
        )
        .unwrap();

        let receivers = parse(&file).unwrap();
        let table = |kind: &str| {
            let receiver = receivers.iter().find(|entry| entry.kind == kind).unwrap();
            let lookup = |method: &str| {
                receiver
                    .methods
                    .iter()
                    .find(|arm| arm.method == method)
                    .map(|arm| arm.function.clone())
            };
            (lookup("to_string"), lookup("size"), lookup("bucket"))
        };

        assert_eq!(
            table("Array"),
            (Some("array::to_string".into()), Some("x::len".into()), None)
        );
        assert_eq!(
            table("RecordId"),
            (Some("type::string".into()), Some("x::len".into()), None)
        );
        assert_eq!(table("File").2.as_deref(), Some("file::bucket"));
        assert_eq!(table(""), (Some("type::string".into()), None, None));
    }
}
