#!/usr/bin/env bash
# Build the SurrealQL language server as a wasm-bindgen browser
# package consumable by Surrealist (and any other browser host).
#
# Outputs:
#   pkg/surrealql_language_server.js        ES-module wrapper
#   pkg/surrealql_language_server_bg.wasm   compiled wasm module
#   pkg/surrealql_language_server.d.ts      TypeScript typings
#   pkg/package.json                        npm metadata (source-controlled)
#   pkg/LICENSE                             license file
#
# The build deliberately stays on stable Rust and avoids the
# `+atomics`/`build-std` flags `tokio_with_wasm` only needs when
# `spawn_blocking` is used. The portable core never touches it.
#
# Requirements:
#   * `wasm-bindgen` CLI (cargo install wasm-bindgen-cli --version 0.2.108)
#   * `wasm-opt` (cargo install wasm-opt)
#   * A clang with the WebAssembly backend enabled (Apple's bundled
#     clang does NOT have it). On macOS the easiest fix is
#     `brew install llvm` — this script auto-detects the result.

set -euo pipefail

cd "$(dirname "$0")/.."

OUT_NAME="surrealql_language_server"
WASM_ARTIFACT="target/wasm32-unknown-unknown/release/${OUT_NAME}.wasm"

# Auto-locate a wasm-capable clang. cc-rs honors these env vars for
# all transitive C compiles (including tree-sitter's own runtime),
# not just our build.rs.
if [ -z "${CC_wasm32_unknown_unknown:-}" ]; then
    for candidate in \
        /opt/homebrew/opt/llvm/bin/clang \
        /usr/local/opt/llvm/bin/clang \
        /usr/local/llvm/bin/clang; do
        if [ -x "$candidate" ]; then
            export CC_wasm32_unknown_unknown="$candidate"
            export AR_wasm32_unknown_unknown="$(dirname "$candidate")/llvm-ar"
            break
        fi
    done
fi

if [ -z "${CC_wasm32_unknown_unknown:-}" ]; then
    echo "warning: no wasm-capable clang detected on PATH;" >&2
    echo "         falling back to the system 'clang' which usually does not" >&2
    echo "         support wasm32-unknown-unknown. Install Homebrew's LLVM" >&2
    echo "         (\`brew install llvm\`) or set CC_wasm32_unknown_unknown" >&2
    echo "         manually if the build fails." >&2
fi

echo "Building the WASM module"
cargo build --release --target wasm32-unknown-unknown --no-default-features

echo "Generating wasm-bindgen bindings"
wasm-bindgen --target web \
    --out-dir pkg \
    --out-name "$OUT_NAME" \
    --typescript \
    "$WASM_ARTIFACT"

echo "Optimizing the WASM module"
wasm-opt -O \
    --enable-bulk-memory \
    --enable-nontrapping-float-to-int \
    --enable-sign-ext \
    --enable-mutable-globals \
    "pkg/${OUT_NAME}_bg.wasm" \
    -o "pkg/${OUT_NAME}_bg.wasm"

cp LICENSE pkg/LICENSE

echo "WASM package ready in pkg/"
