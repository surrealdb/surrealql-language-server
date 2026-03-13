#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_dir="${TREE_SITTER_SURREALQL_DIR:-$repo_root/../tree-sitter-surrealql}/"
target_dir="$repo_root/extensions/zed-surrealql/grammars/surrealql/"
wasm_path="$repo_root/extensions/zed-surrealql/grammars/surrealql.wasm"

if [[ ! -d "${source_dir%/}" ]]; then
	echo "missing grammar source at ${source_dir%/}" >&2
	exit 1
fi

(
	cd "${source_dir%/}"
	if command -v bun >/dev/null 2>&1; then
		bunx tree-sitter generate --abi 14
	else
		npx tree-sitter generate --abi 14
	fi
)

rsync -a \
	--exclude 'node_modules' \
	--exclude 'tree-sitter-surrealql.wasm' \
	"$source_dir" \
	"$target_dir"
(
	cd "$target_dir"
	bunx tree-sitter build --wasm
	cp tree-sitter-surrealql.wasm "$wasm_path"
)
