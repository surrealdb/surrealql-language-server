use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("missing manifest dir"));
    let grammar_dir = grammar_dir(&manifest_dir);
    let parser_c = grammar_dir.join("src/parser.c");
    let grammar_js = grammar_dir.join("grammar.js");
    let header_dir = grammar_dir.join("src");

    for path in [&parser_c, &grammar_js] {
        if !path.exists() {
            panic!(
                "expected SurrealQL grammar asset at {}. Set TREE_SITTER_SURREALQL_DIR to a valid checkout.",
                path.display()
            );
        }
    }

    println!("cargo:rerun-if-env-changed=TREE_SITTER_SURREALQL_DIR");
    println!("cargo:rerun-if-changed={}", parser_c.display());
    println!("cargo:rerun-if-changed={}", grammar_js.display());

    let scanner_c = grammar_dir.join("src/scanner.c");
    let mut build = cc::Build::new();
    build.file(&parser_c).include(&header_dir).warnings(false);
    if scanner_c.exists() {
        println!("cargo:rerun-if-changed={}", scanner_c.display());
        build.file(&scanner_c);
    }

    // On wasm32-unknown-unknown the tree-sitter parser.c needs a clang
    // that has the WebAssembly backend enabled. Apple's bundled clang
    // does not (it only ships native targets); auto-detect the
    // Homebrew / system LLVM install and surface a helpful error if
    // nothing usable is available.
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        if let Some(compiler) = locate_wasm_clang() {
            build.compiler(&compiler);
        }
        // Vendored freestanding shims for `<stdlib.h>` / `<string.h>`
        // matching what `tree-sitter-language` ships. The actual
        // definitions are linked in by the `tree-sitter` crate's
        // `wasm/src/stdlib.c` (compiled automatically when the target
        // is wasm32-unknown-unknown). Vendoring locally avoids the
        // dep-metadata env-var dance with `tree-sitter-language`.
        build.include(manifest_dir.join("wasm/include"));
        build
            .flag_if_supported("-fno-builtin")
            .flag_if_supported("-nostdlib")
            .flag_if_supported("-fvisibility=hidden");
    }

    build.compile("tree-sitter-surrealql");

    let grammar_source = fs::read_to_string(&grammar_js).expect("failed to read grammar.js");
    let keywords = extract_keywords(&grammar_source);
    let generated = format!("pub const KEYWORDS: &[&str] = &{:?};\n", keywords);
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("missing OUT_DIR"));
    fs::write(out_dir.join("keywords.rs"), generated).expect("failed to write generated keywords");
}

/// Returns a clang capable of producing wasm32 object files, or `None`
/// to leave cc-rs's default detection in place. Honors the
/// conventional cc-rs override env vars first, then falls back to the
/// Homebrew LLVM location so a fresh `brew install llvm` "just works"
/// without requiring per-developer environment setup.
fn locate_wasm_clang() -> Option<PathBuf> {
    println!("cargo:rerun-if-env-changed=CC_wasm32_unknown_unknown");
    println!("cargo:rerun-if-env-changed=TARGET_CC");
    println!("cargo:rerun-if-env-changed=WASM_CLANG");

    for var in ["CC_wasm32_unknown_unknown", "TARGET_CC", "WASM_CLANG"] {
        if let Some(value) = env::var_os(var) {
            return Some(PathBuf::from(value));
        }
    }

    for candidate in [
        "/opt/homebrew/opt/llvm/bin/clang",
        "/usr/local/opt/llvm/bin/clang",
        "/usr/local/llvm/bin/clang",
    ] {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Some(path);
        }
    }

    None
}

fn grammar_dir(manifest_dir: &Path) -> PathBuf {
    let configured = env::var_os("TREE_SITTER_SURREALQL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../surrealql-tree-sitter"));

    if configured.is_absolute() {
        configured
    } else {
        manifest_dir.join(configured)
    }
}

fn extract_keywords(grammar_source: &str) -> Vec<String> {
    let mut keywords = BTreeSet::new();
    let needle = "make_keyword('";
    let mut rest = grammar_source;

    while let Some(start) = rest.find(needle) {
        let after = &rest[start + needle.len()..];
        if let Some(end) = after.find("')") {
            keywords.insert(after[..end].to_string());
            rest = &after[end + 2..];
        } else {
            break;
        }
    }

    keywords.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::extract_keywords;
    use pretty_assertions::assert_eq;

    #[test]
    fn extracts_keywords_from_grammar() {
        let source = "keyword_select: (_) => make_keyword('SELECT'),\nkeyword_set: (_) => make_keyword('SET')";
        let keywords = extract_keywords(source);
        assert_eq!(keywords, vec!["SELECT".to_string(), "SET".to_string()]);
    }
}
