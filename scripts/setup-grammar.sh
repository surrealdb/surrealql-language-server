#!/usr/bin/env bash
set -euo pipefail

REPO="https://github.com/surrealdb/surrealql-tree-sitter"
TARGET="$(dirname "$PWD")/surrealql-tree-sitter"

# Pinned grammar revision. The language server's node-kind layer
# (src/semantic/node_kind.rs) is tightly coupled to the grammar's emitted
# node kinds, so an unpinned `master` can silently break analysis when the
# grammar changes shape. Bump this deliberately alongside any node_kind.rs
# update. Override with GRAMMAR_REF for local grammar development.
GRAMMAR_REF="${GRAMMAR_REF:-826d0c2ca6733a1c201ea7015dd91f439f67b573}"

if [ -d "$TARGET" ]; then
    # Never disturb an existing checkout — a grammar developer may have
    # local branches/work here. Just report which revision is present.
    current="$(git -C "$TARGET" rev-parse --short HEAD 2>/dev/null || echo unknown)"
    echo "Grammar already checked out at $TARGET (HEAD $current)"
    if [ "$current" != "unknown" ] && ! git -C "$TARGET" merge-base --is-ancestor "$GRAMMAR_REF" HEAD 2>/dev/null; then
        echo "  note: pinned GRAMMAR_REF is ${GRAMMAR_REF}; current checkout may differ." >&2
    fi
else
    echo "Cloning $REPO -> $TARGET (ref $GRAMMAR_REF)"
    git clone "$REPO" "$TARGET"
    git -C "$TARGET" checkout -q "$GRAMMAR_REF"
fi

echo "Grammar ready at $TARGET"
