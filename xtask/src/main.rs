//! Build-time code generators for the SurrealQL language server.
//!
//! ```text
//! cargo xtask generate-builtins --surrealdb <path-to-surrealdb-checkout>
//! cargo xtask generate-builtins --surrealdb <path> --check
//! cargo run -p xtask --features probe -- verify-returns --surrealdb <path>
//! ```
//!
//! The `cargo xtask` alias ends in `--`, so a cargo flag such as `--features`
//! cannot be passed through it. `verify-returns` needs one, hence the longer
//! third form.
//!
//! SurrealDB is a separate checkout, not a dependency of this crate, so the
//! path is an argument (or `SURREALDB_DIR`). `--check` writes nothing and exits
//! non-zero when the committed file is stale; that is what the freshness test
//! runs.
//!
//! `verify-returns` calls every builtin in an in-memory engine and compares the
//! answer with the return type the catalogue records. It needs the engine, so it
//! sits behind a feature — see [`probe`].

// The `probe` feature compiles the engine, and `Expr::compute` is a deeply
// recursive async function whose layout needs more depth than the default
// allows. Only that feature reaches it; the default build never does.
#![recursion_limit = "512"]

mod emit;
mod engine_tables;
mod kinds;
mod methods;
#[cfg(feature = "probe")]
mod probe;
mod returns;
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
    if !matches!(task.as_str(), "generate-builtins" | "verify-returns") {
        return Err(format!(
            "unknown task `{task}`. The tasks are `generate-builtins` and \
             `verify-returns`."
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

    if task == "verify-returns" {
        return verify_returns(&surrealdb);
    }

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

/// Call every builtin and compare the answer with the recorded return type.
///
/// Needs the engine, so the body only exists with `--features probe`. Without
/// it the task still resolves, and says how to enable it — a missing task would
/// read as a typo.
#[cfg(feature = "probe")]
fn verify_returns(surrealdb: &Path) -> Result<String, String> {
    let catalogue = build_catalogue(surrealdb)?;
    let disagreements = probe::verify(&catalogue.functions)?;
    if disagreements.is_empty() {
        return Ok("every recorded return type matches what the engine answered".to_string());
    }
    Err(format!(
        "{} recorded return types disagree with the engine:\n{}",
        disagreements.len(),
        probe::report(&disagreements)
    ))
}

#[cfg(not(feature = "probe"))]
fn verify_returns(_surrealdb: &Path) -> Result<String, String> {
    Err("`verify-returns` needs the engine. Re-run it with:\n    \
         cargo run -p xtask --features probe -- verify-returns --surrealdb <path>"
        .to_string())
}

fn generate(surrealdb: &Path) -> Result<String, String> {
    rustfmt(&emit::render(&build_catalogue(surrealdb)?))
}

/// Read the engine's tables and join them, with the coverage gates applied.
fn build_catalogue(surrealdb: &Path) -> Result<emit::Catalogue, String> {
    let core = surrealdb.join("surrealdb/core/src");
    let builtin_rs = core.join("syn/parser/builtin.rs");
    let fnc_dir = core.join("fnc");
    let fnc_mod_rs = fnc_dir.join("mod.rs");
    let registry_dir = core.join("exec/function/builtin");

    for path in [&builtin_rs, &fnc_mod_rs, &registry_dir] {
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
    let declared_returns = returns::collect(&registry_dir)?;
    let namespaces = engine_tables::namespaces(&paths);
    let revision = git_revision(surrealdb).unwrap_or_else(|| "unknown".to_string());

    let receivers = methods::parse(&fnc_mod_rs)?;
    let catalogue = emit::build(
        &paths,
        &dispatch,
        &implementations,
        &declared_returns,
        revision,
        namespaces,
        &receivers,
    );

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

    // A correction overrides what the engine declares, so one that no longer
    // matches must stop the run rather than pass unnoticed.
    let stale = emit::unmatched_corrections(&declared_returns);
    if !stale.is_empty() {
        return Err(format!(
            "these return-type corrections no longer apply: {}.\nEither SurrealDB \
             fixed the declaration, in which case delete the entry from \
             `CORRECTIONS`, or the function was renamed, in which case re-run \
             `verify-returns` and correct the new name.",
            stale.join(", ")
        ));
    }

    // The same argument as the signature gate above, for the other half of the
    // catalogue. A registry the generator cannot read leaves every return type
    // `any`, and the type checker then stays silent everywhere it used to speak.
    let with_return: Vec<&str> = catalogue
        .functions
        .iter()
        .filter(|entry| !entry.not_callable && entry.returns != "any")
        .map(|entry| entry.name.as_str())
        .collect();
    if with_return.len() * 10 < callable * 5 {
        return Err(format!(
            "read a return type for only {} of {callable} callable functions — the \
             engine's registry layout probably changed, so the catalogue is not \
             trustworthy.",
            with_return.len()
        ));
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
    eprintln!(
        "  {} of {callable} callable functions have a return type; {} are `any`",
        with_return.len(),
        callable - with_return.len()
    );

    Ok(catalogue)
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
        // A fixed width, not `--short`: git picks that length from the object
        // count, so the same commit abbreviates to 8 characters in a shallow
        // clone and 9 in a full one. The revision goes into the generated file,
        // so an adaptive length makes the freshness check depend on how the
        // checkout was made.
        .args(["rev-parse", "--short=9", "HEAD"])
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
