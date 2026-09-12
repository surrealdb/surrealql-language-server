#!/usr/bin/env bash
#
# Copy the pinned grammar's build inputs into `vendor/`, so the published crate
# can compile without a sibling checkout.
#
# `cargo install surrealql-language-server` has never worked: build.rs looks for
# `../surrealql-tree-sitter`, which exists in this repository's layout and in
# nowhere a crates.io consumer unpacks to, so the build panicked. Vendoring is
# what makes the published artifact self-contained.
#
# Only the four build inputs are copied: the grammar's tests, corpus, bindings
# and git history are not the crate's business. `parser.c` is ~6 MB of generated
# C that compresses to about 0.3 MB in the `.crate`.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PIN_FILE="$ROOT/grammar.pin"
VENDOR="$ROOT/vendor/surrealql-tree-sitter"
SOURCE="${TREE_SITTER_SURREALQL_DIR:-$(dirname "$ROOT")/surrealql-tree-sitter}"

pin_ref="$(sed -e 's/#.*//' -e '/^[[:space:]]*$/d' "$PIN_FILE" |
    awk -F= '$1 == "ref" { print $2; exit }')"

if [ ! -f "$SOURCE/src/parser.c" ]; then
    echo "error: no grammar checkout at $SOURCE. Run scripts/setup-grammar.sh first." >&2
    exit 1
fi

# Vendoring anything but the pin would publish a crate that behaves differently
# from this repository's own test run.
current="$(git -C "$SOURCE" rev-parse HEAD 2>/dev/null || echo unknown)"
if [ "$current" != "$pin_ref" ]; then
    echo "error: $SOURCE is ${current:0:7}, not the pinned ${pin_ref:0:7}." >&2
    echo "       Run scripts/setup-grammar.sh, then this script." >&2
    exit 1
fi

rm -rf "$VENDOR"
mkdir -p "$VENDOR/src/tree_sitter"
cp "$SOURCE/src/parser.c" "$VENDOR/src/parser.c"
cp "$SOURCE/grammar.js" "$VENDOR/grammar.js"
[ -f "$SOURCE/src/scanner.c" ] && cp "$SOURCE/src/scanner.c" "$VENDOR/src/scanner.c"
cp "$SOURCE"/src/tree_sitter/*.h "$VENDOR/src/tree_sitter/"

# The revision this copy came from. build.rs compares it against grammar.pin, so
# a vendored tree that drifts is a build error rather than a silent difference
# between what CI tested and what crates.io ships.
printf '%s\n' "$pin_ref" > "$VENDOR/REVISION"

cat > "$VENDOR/README.md" <<EOF
# Vendored SurrealQL grammar

Generated: do not edit. Refresh with \`make vendor-grammar\`.

Revision: \`$pin_ref\` (see [\`grammar.pin\`](../../grammar.pin))

These are the four build inputs \`build.rs\` compiles, copied from
[surrealql-tree-sitter](https://github.com/surrealdb/surrealql-tree-sitter) so
the published crate builds without a sibling checkout. A local checkout still
wins: \`build.rs\` prefers \`TREE_SITTER_SURREALQL_DIR\`, then
\`../surrealql-tree-sitter\`, and falls back here.
EOF

echo "Vendored ${pin_ref:0:7} into $VENDOR"
du -sh "$VENDOR" | awk '{print "  size: " $1}'
