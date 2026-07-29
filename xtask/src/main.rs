//! Build-time code generators for the SurrealQL language server.
//!
//! ```text
//! cargo xtask generate-builtins --surrealdb <path-to-surrealdb-checkout>
//! cargo xtask generate-builtins --surrealdb <path> --check
//! ```
//!
//! SurrealDB is a separate checkout, not a dependency of this crate, so the
//! path is an argument (or `SURREALDB_DIR`). `--check` writes nothing and exits
//! non-zero when the committed file is stale; that is what the freshness test
//! runs.

mod emit;
mod engine_tables;
mod kinds;
mod signatures;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Where the generated catalogue lives, relative to the repository root.
const OUTPUT: &str = "src/grammar_generated.rs";

fn main() -> ExitCode {
    match run() {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, String> {
    let mut args = std::env::args().skip(1);
    let task = args.next().unwrap_or_default();
    if task != "generate-builtins" {
        return Err(format!(
            "unknown task `{task}`. The only task is `generate-builtins`."
        ));
    }

    let mut surrealdb: Option<PathBuf> = std::env::var_os("SURREALDB_DIR").map(PathBuf::from);
    let mut check_only = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--surrealdb" => {
                surrealdb = Some(PathBuf::from(args.next().ok_or(
                    "`--surrealdb` needs a path to the SurrealDB checkout".to_string(),
                )?));
            }
            "--check" => check_only = true,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    let surrealdb = surrealdb.ok_or_else(|| {
        "pass `--surrealdb <path>` or set SURREALDB_DIR to a SurrealDB checkout".to_string()
    })?;

    let generated = generate(&surrealdb)?;
    let output = repository_root().join(OUTPUT);

    if check_only {
        let committed = std::fs::read_to_string(&output)
            .map_err(|error| format!("cannot read {}: {error}", output.display()))?;
        if committed == generated {
            return Ok(format!("{OUTPUT} is up to date"));
        }
        return Err(format!(
            "{OUTPUT} is stale. Regenerate it:\n    \
             cargo xtask generate-builtins --surrealdb {}",
            surrealdb.display()
        ));
    }

    std::fs::write(&output, &generated)
        .map_err(|error| format!("cannot write {}: {error}", output.display()))?;
    Ok(format!("wrote {OUTPUT} ({} bytes)", generated.len()))
}

fn generate(surrealdb: &Path) -> Result<String, String> {
    let core = surrealdb.join("surrealdb/core/src");
    let builtin_rs = core.join("syn/parser/builtin.rs");
    let fnc_dir = core.join("fnc");
    let fnc_mod_rs = fnc_dir.join("mod.rs");

    for path in [&builtin_rs, &fnc_mod_rs] {
        if !path.exists() {
            return Err(format!(
                "{} is missing — is {} a SurrealDB checkout?",
                path.display(),
                surrealdb.display()
            ));
        }
    }

    let paths = engine_tables::parse_paths(&builtin_rs)?;
    let dispatch = engine_tables::parse_dispatch(&fnc_mod_rs)?;
    let implementations = signatures::collect(&fnc_dir)?;
    let namespaces = engine_tables::namespaces(&paths);
    let revision = git_revision(surrealdb).unwrap_or_else(|| "unknown".to_string());

    let catalogue = emit::build(&paths, &dispatch, &implementations, revision, namespaces);

    // A catastrophic parse failure must not quietly emit a catalogue of unknown
    // signatures that then silences every argument check.
    let known = catalogue
        .functions
        .iter()
        .filter(|entry| entry.signature_known)
        .count();
    let callable = catalogue
        .functions
        .iter()
        .filter(|entry| !entry.not_callable)
        .count();
    let unread: Vec<&str> = catalogue
        .functions
        .iter()
        .filter(|entry| !entry.not_callable && !entry.signature_known)
        .map(|entry| entry.name.as_str())
        .collect();
    if known * 10 < callable * 9 {
        return Err(format!(
            "read a signature for only {known} of {callable} callable functions — the \
             engine's source layout probably changed, so the catalogue is not \
             trustworthy.\nUnread:\n  {}",
            unread.join("\n  ")
        ));
    }
    if !unread.is_empty() {
        // Not fatal: an unread signature is silent, not wrong. Reported so the
        // number cannot drift upwards unnoticed.
        eprintln!(
            "  {} callable functions have no readable signature: {}",
            unread.len(),
            unread.join(", ")
        );
    }

    let zero_arity = catalogue
        .functions
        .iter()
        .filter(|entry| entry.signature_known && entry.params.is_empty())
        .count();
    eprintln!(
        "  {} functions ({callable} callable), {known} signatures read, \
         {zero_arity} of those take no arguments, {} constants, {} renames, {} not callable",
        catalogue.functions.len(),
        catalogue.constants.len(),
        catalogue.renames.len(),
        catalogue.not_callable.len()
    );

    rustfmt(&emit::render(&catalogue))
}

/// Format the generated source with `rustfmt`.
///
/// Not cosmetic: `cargo fmt` runs over the whole crate, so unformatted output
/// would be rewritten the moment anyone formats the repository, and the
/// freshness check would then report a stale file forever.
fn rustfmt(source: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run rustfmt: {error}"))?;

    child
        .stdin
        .take()
        .ok_or("rustfmt has no stdin")?
        .write_all(source.as_bytes())
        .map_err(|error| format!("cannot write to rustfmt: {error}"))?;

    let output = child
        .wait_with_output()
        .map_err(|error| format!("rustfmt failed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "rustfmt rejected the generated source: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("rustfmt produced invalid UTF-8: {error}"))
}

fn git_revision(dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// The repository root, from this crate's manifest directory.
fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ always has a parent")
        .to_path_buf()
}
