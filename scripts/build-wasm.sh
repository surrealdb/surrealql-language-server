#!/usr/bin/env bash
# Build the SurrealQL language server as a `wasm-bindgen` browser
# package consumable by Surrealist (and any other browser host).
#
# Outputs:
#   pkg/surrealql_language_server.js        ES-module wrapper
#   pkg/surrealql_language_server_bg.wasm   compiled wasm module
#   pkg/surrealql_language_server.d.ts      TypeScript typings
#   pkg/package.json                        npm metadata
#
# The build deliberately stays on stable Rust and avoids the
# `+atomics`/`build-std` flags `tokio_with_wasm` only needs when
# `spawn_blocking` is used. The portable core never touches it.
#
# Requirements:
#   * `wasm-pack` (cargo install wasm-pack | brew install wasm-pack)
#   * A clang with the WebAssembly backend enabled (Apple's bundled
#     clang does NOT have it). On macOS the easiest fix is
#     `brew install llvm` — this script auto-detects the result.

set -euo pipefail

cd "$(dirname "$0")/.."

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
    echo "         falling back to the system 'clang' which usually does not"  >&2
    echo "         support wasm32-unknown-unknown. Install Homebrew's LLVM" >&2
    echo "         (\`brew install llvm\`) or set CC_wasm32_unknown_unknown" >&2
    echo "         manually if the build fails." >&2
fi

exec wasm-pack build \
    --release \
    --target web \
    --out-dir pkg \
    --out-name surrealql_language_server \
    --no-default-features
