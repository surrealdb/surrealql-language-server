//! Argument types, read from the engine's `pub fn` signatures.
//!
//! SurrealDB has no signature table. A builtin's argument types *are* the
//! destructured tuple type of its implementation:
//!
//! ```ignore
//! pub fn slice(
//!     (val, Optional(range_start), Optional(end)): (String, Optional<Value>, Optional<i64>),
//! ) -> Result<Value>
//! ```
//!
//! This must be an abstract-syntax-tree parse, not a line scan. Of the 326
//! top-level `pub fn` items under `fnc/`, only 268 fit on one line, a further
//! 125 functions live in nested `pub mod` blocks, and patterns bind with `mut`
//! (`array.rs:204`). A regular expression gets all three wrong.

use std::collections::BTreeMap;
use std::path::Path;

use quote::ToTokens;
use syn::{FnArg, Item, ItemFn, ItemMod, Pat, ReturnType, Type, Visibility};

use crate::engine_tables::strip_raw;
use crate::kinds::{Param, is_context_type, map_type};

/// One implementation found under `fnc/`, keyed by its Rust path.
#[derive(Debug, Clone)]
pub struct Implementation {
    /// `string::is::alphanum` — the module path, not the name authors write.
    pub path: String,
    pub params: Vec<Param>,
    pub is_async: bool,
}

/// Every `pub fn` under `fnc/`, keyed by module path.
///
/// Recurses into subdirectories: `api/` holds `api::req::body` and friends in
/// `req.rs` and `res.rs` rather than in one `api.rs`.
pub fn collect(fnc_dir: &Path) -> Result<BTreeMap<String, Implementation>, String> {
    let mut found = BTreeMap::new();
    collect_dir(fnc_dir, "", &mut found)?;
    Ok(found)
}

fn collect_dir(
    dir: &Path,
    prefix: &str,
    found: &mut BTreeMap<String, Implementation>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    for entry in entries {
        let path = entry
            .map_err(|error| format!("cannot read a directory entry: {error}"))?
            .path();
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();

        if path.is_dir() {
            // `script/` mirrors the catalogue for JavaScript blocks and declares
            // no builtin of its own.
            if matches!(stem.as_str(), "script" | "util") {
                continue;
            }
            let nested = join(prefix, &stem);
            collect_dir(&path, &nested, found)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        // `args.rs` holds the arity machinery and `operate.rs` the operators.
        // A `mod.rs` contributes its parent's path, not a new segment: the
        // dispatch tables live in `fnc/mod.rs`, but `api/mod.rs` declares
        // `api::invoke`.
        if matches!(stem.as_str(), "args" | "operate") {
            continue;
        }
        let module_path = if stem == "mod" {
            if prefix.is_empty() {
                // `fnc/mod.rs` is the dispatch table, not a declaration site.
                continue;
            }
            prefix.to_string()
        } else {
            join(prefix, &stem)
        };

        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let file = syn::parse_file(&source)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        walk(&file.items, &module_path, found);
    }
    Ok(())
}

fn join(prefix: &str, segment: &str) -> String {
    if prefix.is_empty() {
        segment.to_string()
    } else {
        format!("{prefix}::{segment}")
    }
}

/// Recurse through items, extending the module path at each nested `pub mod`.
fn walk(items: &[Item], module_path: &str, out: &mut BTreeMap<String, Implementation>) {
    for item in items {
        match item {
            Item::Fn(function) if is_public(&function.vis) => {
                if let Some(implementation) = read_function(function, module_path) {
                    out.insert(implementation.path.clone(), implementation);
                }
            }
            Item::Mod(ItemMod {
                vis,
                ident,
                content: Some((_, nested)),
                ..
            }) if is_public(vis) => {
                // `pub mod is { pub fn alphanum }` inside `string.rs` is
                // `string::is::alphanum`.
                let ident = ident.to_string();
                let segment = strip_raw(&ident);
                walk(nested, &format!("{module_path}::{segment}"), out);
            }
            _ => {}
        }
    }
}

fn is_public(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
}

/// One `pub fn` → its user-visible parameters.
///
/// Returns `None` for a function that is not a builtin implementation: a
/// helper with a plain argument list, or one that does not return
/// `Result<Value>`.
fn read_function(function: &ItemFn, module_path: &str) -> Option<Implementation> {
    if !returns_a_value(&function.sig.output) {
        return None;
    }

    // A receiver means a method, which is never a builtin declaration.
    let mut typed = Vec::new();
    for argument in &function.sig.inputs {
        match argument {
            FnArg::Typed(pat_type) => typed.push(pat_type),
            FnArg::Receiver(_) => return None,
        }
    }

    // The engine injects its context either as a leading tuple
    // (`(stk, ctx, opt, doc): (&mut Stk, …)`) or as bare leading arguments
    // (`ctx: &FrozenContext`, `http.rs:89`). Both drop out here.
    let author_supplied: Vec<&&syn::PatType> = typed
        .iter()
        .filter(|pat_type| !is_context_argument(&pat_type.ty))
        .collect();

    let name = function.sig.ident.to_string();
    let path = format!("{module_path}::{}", strip_raw(&name));
    let is_async = function.sig.asyncness.is_some();

    match author_supplied.as_slice() {
        // Context only, or no arguments at all: the arity is genuinely zero.
        [] => Some(Implementation {
            path,
            params: Vec::new(),
            is_async,
        }),
        // `read_params` gives up when a parameter's arity cannot be known, and
        // an unknown arity must make the whole signature unknown rather than a
        // guess.
        [only] => Some(Implementation {
            path,
            params: read_params(&only.ty, &only.pat)?,
            is_async,
        }),
        // A helper with several plain arguments. Not a builtin, and guessing
        // which one is the signature would be worse than staying silent.
        _ => None,
    }
}

/// True when the function returns `Result<Value>`, which every builtin does and
/// helpers such as `limit() -> Result<()>` do not.
fn returns_a_value(output: &ReturnType) -> bool {
    let ReturnType::Type(_, ty) = output else {
        return false;
    };
    render(ty).starts_with("Result<Value")
}

/// True when this argument is engine-injected rather than author-supplied —
/// either a context type itself, or a tuple built only from context types.
fn is_context_argument(ty: &Type) -> bool {
    match ty {
        Type::Tuple(tuple) => {
            !tuple.elems.is_empty()
                && tuple
                    .elems
                    .iter()
                    .all(|elem| is_context_type(&render(elem)))
        }
        other => is_context_type(&render(other)),
    }
}

fn binding_name(pattern: &Pat) -> String {
    match pattern {
        Pat::Ident(ident) => ident.ident.to_string(),
        // `Optional(end)` / `Rest(arrays)` — the useful name is inside.
        Pat::TupleStruct(tuple_struct) => tuple_struct
            .elems
            .first()
            .map(binding_name)
            .unwrap_or_else(|| "value".to_string()),
        Pat::Wild(_) => "value".to_string(),
        other => render_pat(other),
    }
}

/// The parameters of the author-supplied argument.
///
/// A tuple type declares one parameter per element. Anything else is a single
/// parameter, which is how the bare-wrapper form arrives:
/// `pub fn concat(Rest(arrays): Rest<Array>)` at `array.rs:186`.
///
/// Names come from the *pattern*, not the type, because patterns bind with
/// `mut` and through wrappers such as `Optional(end)`.
fn read_params(ty: &Type, pattern: &Pat) -> Option<Vec<Param>> {
    match ty {
        Type::Tuple(tuple) => {
            let names: Vec<String> = match pattern {
                Pat::Tuple(pat_tuple) => pat_tuple.elems.iter().map(binding_name).collect(),
                _ => Vec::new(),
            };
            tuple
                .elems
                .iter()
                .enumerate()
                .map(|(index, elem)| {
                    let (ty, form) = map_type(&render(elem))?;
                    Some(Param {
                        name: names
                            .get(index)
                            .cloned()
                            .unwrap_or_else(|| format!("arg{}", index + 1)),
                        ty,
                        form,
                    })
                })
                .collect()
        }
        single => {
            let (ty, form) = map_type(&render(single))?;
            Some(vec![Param {
                name: binding_name(pattern),
                ty,
                form,
            }])
        }
    }
}

/// Render a type back to source-like text for the mapper.
///
/// `quote`'s spacing is irrelevant here because the mapper trims and matches on
/// identifiers, but the generic brackets must survive, so the tokens are joined
/// without spaces around `<`, `>` and `,`.
fn render(ty: &Type) -> String {
    tidy(&ty.to_token_stream().to_string())
}

fn render_pat(pattern: &Pat) -> String {
    tidy(&pattern.to_token_stream().to_string())
}

/// `Optional < i64 >` → `Optional<i64>`.
fn tidy(rendered: &str) -> String {
    rendered
        .replace(" <", "<")
        .replace("< ", "<")
        .replace(" >", ">")
        .replace("> ", ">")
        .replace(" ,", ",")
        .replace(":: ", "::")
        .replace(" ::", "::")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::ParamForm;

    fn parse(source: &str) -> BTreeMap<String, Implementation> {
        let file = syn::parse_file(source).expect("test source must parse");
        let mut out = BTreeMap::new();
        walk(&file.items, "string", &mut out);
        out
    }

    #[test]
    fn a_single_line_signature_is_read() {
        let found = parse("pub fn len((arg,): (String,)) -> Result<Value> { todo!() }");
        let implementation = &found["string::len"];
        assert_eq!(implementation.params.len(), 1);
        assert_eq!(implementation.params[0].ty, "string");
        assert_eq!(implementation.params[0].form, ParamForm::Required);
        assert_eq!(implementation.params[0].name, "arg");
    }

    #[test]
    fn a_multi_line_signature_with_mut_is_read() {
        // The shape a line scan cannot handle (`array.rs:204`).
        let found = parse(
            "pub fn fill(
                (mut array, value, Optional(range_start), Optional(end)): (
                    Array,
                    Value,
                    Optional<Value>,
                    Optional<i64>,
                ),
            ) -> Result<Value> { todo!() }",
        );
        let implementation = &found["string::fill"];
        let types: Vec<&str> = implementation
            .params
            .iter()
            .map(|param| param.ty.as_str())
            .collect();
        assert_eq!(types, vec!["array", "any", "any", "int"]);
        let forms: Vec<ParamForm> = implementation.params.iter().map(|p| p.form).collect();
        assert_eq!(
            forms,
            vec![
                ParamForm::Required,
                ParamForm::Required,
                ParamForm::Optional,
                ParamForm::Optional
            ]
        );
        assert_eq!(
            implementation.params[0].name, "array",
            "`mut` must not leak"
        );
        assert_eq!(implementation.params[3].name, "end", "unwrap Optional(end)");
    }

    #[test]
    fn a_nested_module_extends_the_path() {
        let found =
            parse("pub mod is { pub fn alphanum((arg,): (String,)) -> Result<Value> { todo!() } }");
        assert!(
            found.contains_key("string::is::alphanum"),
            "got {:?}",
            found.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_injected_context_tuple_is_skipped() {
        let found = parse(
            "pub async fn field(
                (stk, ctx, opt, doc): (&mut Stk, &FrozenContext, Option<&Options>, Option<&CursorDoc>),
                (val,): (String,),
            ) -> Result<Value> { todo!() }",
        );
        let implementation = &found["string::field"];
        assert_eq!(
            implementation.params.len(),
            1,
            "context must not be counted"
        );
        assert_eq!(implementation.params[0].ty, "string");
        assert!(implementation.is_async);
    }

    #[test]
    fn a_context_only_function_takes_no_author_arguments() {
        let found = parse("pub fn db((ctx,): (&FrozenContext,)) -> Result<Value> { todo!() }");
        assert!(found["string::db"].params.is_empty());
    }

    #[test]
    fn a_helper_with_plain_arguments_is_not_a_builtin() {
        let found = parse("pub fn helper(a: usize, b: usize) -> Result<Value> { todo!() }");
        assert!(
            found.is_empty(),
            "got {:?}",
            found.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_function_that_does_not_return_result_is_skipped() {
        let found = parse("pub fn helper((a,): (String,)) -> usize { todo!() }");
        assert!(found.is_empty());
    }

    #[test]
    fn a_private_function_is_skipped() {
        let found = parse("fn hidden((a,): (String,)) -> Result<Value> { todo!() }");
        assert!(found.is_empty());
    }

    #[test]
    fn a_typed_variadic_is_read_as_variadic() {
        let found =
            parse("pub fn concat((Rest(arrays),): (Rest<Array>,)) -> Result<Value> { todo!() }");
        assert_eq!(found["string::concat"].params[0].form, ParamForm::Variadic);
    }

    #[test]
    fn a_self_validating_variadic_is_read_as_variadic_any() {
        let found = parse("pub fn concat((Any(args),): (Any,)) -> Result<Value> { todo!() }");
        let param = &found["string::concat"].params[0];
        assert_eq!(param.form, ParamForm::Variadic);
        assert_eq!(param.ty, "any");
    }
}
