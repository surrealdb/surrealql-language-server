#!/usr/bin/env bash
#
# Fetch the SurrealDB checkout the catalogue tests and the conformance sweep
# read, at the revision `surrealdb.pin` names.
#
# Only the pinned commit is fetched (`--depth 1`), because both consumers read
# source files rather than history. Without this checkout the catalogue
# freshness test skips and the sweep skips, and they skipped for the whole life
# of the generator, which is how the catalogue was free to drift from the engine.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PIN_FILE="$ROOT/surrealdb.pin"
TARGET="${SURREALDB_DIR:-$(dirname "$ROOT")/surrealdb}"

pin_value() {
    sed -e 's/#.*//' -e '/^[[:space:]]*$/d' "$PIN_FILE" |
        awk -F= -v key="$1" '$1 == key { print $2; exit }'
}

if [ ! -f "$PIN_FILE" ]; then
    echo "error: $PIN_FILE is missing; it is the single source of the SurrealDB revision." >&2
    exit 1
fi

REF="$(pin_value ref)"
REPO="$(pin_value repo)"

if [ -z "$REF" ] || [ -z "$REPO" ]; then
    echo "error: $PIN_FILE must define both 'ref' and 'repo'." >&2
    exit 1
fi

if [ -d "$TARGET/.git" ]; then
    current="$(git -C "$TARGET" rev-parse HEAD 2>/dev/null || echo unknown)"
    if [ "$current" = "$REF" ]; then
        echo "SurrealDB at $TARGET already on the pin (${REF:0:9})."
        exit 0
    fi
    # Unlike the grammar, this checkout is not a build input: it feeds two test
    # suites. A developer's own clone is theirs; say what to do and stop.
    echo "error: $TARGET is ${current:0:9}, not the pinned ${REF:0:9}." >&2
    echo "       The catalogue and the corpus must come from one revision." >&2
    echo "       Point SURREALDB_DIR at a checkout of ${REF:0:9}, or fetch it here:" >&2
    echo "         git -C $TARGET fetch --depth 1 origin $REF && git -C $TARGET checkout FETCH_HEAD" >&2
    exit 1
fi

echo "Fetching ${REF:0:9} from $REPO -> $TARGET"
git init -q "$TARGET"
git -C "$TARGET" remote add origin "$REPO" 2>/dev/null || true
git -C "$TARGET" fetch -q --depth 1 origin "$REF"
git -C "$TARGET" checkout -q FETCH_HEAD

echo "SurrealDB ready at $TARGET (${REF:0:9})"
