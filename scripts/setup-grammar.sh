#!/usr/bin/env bash
set -euo pipefail

REPO="https://github.com/surrealdb/surrealql-tree-sitter"
TARGET="$(dirname "$PWD")/surrealql-tree-sitter"

if [ -d "$TARGET" ]; then
    echo "Updating $TARGET"
    git -C "$TARGET" pull --ff-only
else
    echo "Cloning $REPO -> $TARGET"
    git clone --depth 1 "$REPO" "$TARGET"
fi

echo "Grammar ready at $TARGET"
