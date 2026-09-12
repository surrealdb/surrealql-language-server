#!/usr/bin/env bash
#
# Clone or update the sibling tree-sitter SurrealQL grammar to the revision
# `grammar.pin` names.
#
# The old version of this script refused to touch an existing checkout, on the
# reasoning that a grammar developer might have work there. The cost was worse
# than the protection: a checkout that predated the pin failed ~22 tests with
# `parse` diagnostics on valid SurrealQL and nothing said why: it could not
# even resolve the pinned SHA to report the difference. This version fetches and
# moves a *clean* checkout onto the pin, and refuses to touch a dirty one.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PIN_FILE="$ROOT/grammar.pin"
TARGET="${TREE_SITTER_SURREALQL_DIR:-$(dirname "$ROOT")/surrealql-tree-sitter}"

pin_value() {
    # One `key=value` per line, `#` comments and blanks ignored.
    sed -e 's/#.*//' -e '/^[[:space:]]*$/d' "$PIN_FILE" |
        awk -F= -v key="$1" '$1 == key { print $2; exit }'
}

if [ ! -f "$PIN_FILE" ]; then
    echo "error: $PIN_FILE is missing; it is the single source of the grammar revision." >&2
    exit 1
fi

# GRAMMAR_REF overrides the pin for local grammar development. Nothing in CI
# sets it: CI must build exactly what the pin names.
GRAMMAR_REF="${GRAMMAR_REF:-$(pin_value ref)}"
REPO="$(pin_value repo)"

if [ -z "$GRAMMAR_REF" ] || [ -z "$REPO" ]; then
    echo "error: $PIN_FILE must define both 'ref' and 'repo'." >&2
    exit 1
fi

if [ ! -d "$TARGET/.git" ]; then
    echo "Cloning $REPO -> $TARGET"
    git clone -q "$REPO" "$TARGET"
fi

current="$(git -C "$TARGET" rev-parse HEAD 2>/dev/null || echo unknown)"

if [ "$current" = "$GRAMMAR_REF" ]; then
    echo "Grammar at $TARGET already on the pin (${GRAMMAR_REF:0:7})."
    exit 0
fi

# A dirty tree is the one case worth protecting: someone is mid-change on the
# grammar itself. Say exactly what to do rather than moving their work.
if [ -n "$(git -C "$TARGET" status --porcelain 2>/dev/null)" ]; then
    echo "error: $TARGET has uncommitted changes; refusing to move it to ${GRAMMAR_REF:0:7}." >&2
    echo "       Commit or stash them, or point TREE_SITTER_SURREALQL_DIR at another checkout." >&2
    exit 1
fi

if ! git -C "$TARGET" cat-file -e "${GRAMMAR_REF}^{commit}" 2>/dev/null; then
    echo "Fetching ${GRAMMAR_REF:0:7} from $REPO"
    git -C "$TARGET" fetch -q origin "$GRAMMAR_REF" 2>/dev/null ||
        git -C "$TARGET" fetch -q --tags origin
fi

echo "Moving $TARGET from ${current:0:7} to ${GRAMMAR_REF:0:7}"
git -C "$TARGET" checkout -q --detach "$GRAMMAR_REF"

echo "Grammar ready at $TARGET (${GRAMMAR_REF:0:7})"
