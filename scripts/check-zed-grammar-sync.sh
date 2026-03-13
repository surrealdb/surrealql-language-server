#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_dir="${TREE_SITTER_SURREALQL_DIR:-$repo_root/../tree-sitter-surrealql}"
target_dir="$repo_root/extensions/zed-surrealql/grammars/surrealql"

if [[ ! -d "$source_dir" ]]; then
	echo "missing grammar source at $source_dir" >&2
	exit 1
fi

diff -ru \
	--exclude '.gitignore' \
	"$source_dir" \
	"$target_dir"
