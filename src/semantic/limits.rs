//! Bounds on how deep the analyzer will descend.
//!
//! Nearly thirty functions across `semantic/` walk the tree-sitter tree by
//! recursion, and several more walk a [`TypeExpr`](super::type_expr::TypeExpr)
//! the same way. Tree-sitter's own parser is iterative, so it happily builds a
//! tree as deep as the text asks for, and `panic = 'abort'` means the first
//! walk to run out of stack kills the process, taking every open document with
//! it. Measured on the real binary before this guard existed: a `didOpen`
//! carrying `RETURN` and six thousand nested parentheses (a 12 KB file)
//! aborted a `tokio-rt-worker` thread with `has overflowed its stack`.
//!
//! SurrealDB defends the same class of bug in its own parser, and its comment
//! on `expr_recursion_limit` names the same failure: a deep `Expr` tree whose
//! `Drop`, `ToSql` and lowering each recurse once per node, "overflowing the
//! call stack on long enough chains: a denial of service reachable from query
//! text alone".
//!
//! # Where the number comes from
//!
//! Measured, not guessed: `measure_corpus_tree_depth` in
//! `tests/conformance.rs` reports the distribution over SurrealDB's own 1,897
//! test queries:
//!
//! ```text
//! depth    0-9: 1558      depth  20-29: 3
//! depth  10-19: 335       depth    70+: 1   (204, and the engine rejects it)
//! ```
//!
//! The single outlier is `reproductions/expression_depth_limit.surql`, a
//! ~200-term operator spine that exists to prove the *engine* refuses it. So
//! real SurrealQL lives under depth 30, and the deepest thing SurrealDB will
//! even parse is bounded by its own defaults: `object_recursion_limit: 100`,
//! `query_recursion_limit: 20`, `expr_recursion_limit: 128`.
//!
//! A tree-sitter tree spends a few nodes per level of source nesting, so the
//! deepest *valid* query the engine accepts lands in the high hundreds at worst.
//! 1024 sits above that and an order of magnitude below where the stack
//! actually runs out, which is the shape this constant wants: never reached by
//! a query SurrealDB would run, always reached before the process dies.
pub const MAX_NODE_DEPTH: u32 = 1024;

/// True when a walk at `depth` must stop rather than descend.
///
/// Written as a function so every caller reads the same way and the constant
/// has exactly one comparison against it.
#[inline]
pub fn too_deep(depth: u32) -> bool {
    depth >= MAX_NODE_DEPTH
}

/// The deepest run of unclosed brackets in `source`, ignoring string contents
/// and line comments.
///
/// A pre-filter, checked *before* parsing, and it exists because the tree-depth
/// guard is too late for the worst input. `tree_depth` can only run on a tree
/// that already exists, and tree-sitter frees a tree by recursing through it,
/// so a document deep enough overflows the stack in tree-sitter's own `Drop`,
/// after the guard has had its say. Measured: 50,000 nested parentheses aborts
/// while dropping the tree, with every Rust walk correctly skipped.
///
/// Counting brackets in the text costs one linear pass and no allocation, and it
/// is a sound proxy: every level of tree nesting the analyzer can drown in comes
/// from a bracket, a brace or a paren in the source.
///
/// Strings and `--`/`//`/`#` comments are skipped so a literal full of
/// parentheses cannot trip it. A block comment is *not* tracked, which can only
/// over-count, and over-counting past this threshold takes more than a thousand
/// unmatched opening brackets inside one comment.
pub fn max_bracket_depth(source: &str) -> u32 {
    let mut depth: u32 = 0;
    let mut deepest: u32 = 0;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let bytes = source.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        index += 1;

        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == open {
                quote = None;
            }
            continue;
        }

        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'-' | b'/' if index < bytes.len() && bytes[index] == byte => {
                // `--` and `//` run to the end of the line.
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'#' => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                deepest = deepest.max(depth);
                if deepest > MAX_NODE_DEPTH {
                    // Nothing above the threshold needs an exact answer.
                    return deepest;
                }
            }
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }

    deepest
}

/// The depth of the deepest node under `root`, counting `root` as 1.
///
/// Iterative on purpose. Measuring recursion depth by recursing would overflow
/// on exactly the input this exists to reject, and a cursor walk is the same
/// traversal the analyzer's hot paths already use (see `collect_descendants` in
/// `semantic::analyzer`, which is iterative for a measured 1.8x).
///
/// Stops as soon as `ceiling` is exceeded: a hostile document is rejected after
/// `ceiling` levels rather than after a full walk of however many million nodes
/// it holds.
pub fn tree_depth(root: tree_sitter::Node<'_>, ceiling: u32) -> u32 {
    let mut cursor = root.walk();
    let mut depth: u32 = 1;
    let mut deepest: u32 = 1;

    loop {
        if cursor.goto_first_child() {
            depth += 1;
            deepest = deepest.max(depth);
            if deepest > ceiling {
                return deepest;
            }
            continue;
        }
        // No children: step sideways, climbing until a sibling exists or the
        // walk is back at the root it started from.
        loop {
            if depth == 1 {
                return deepest;
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return deepest;
            }
            depth -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::language;

    fn depth_of(source: &str) -> u32 {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language()).expect("grammar");
        let tree = parser.parse(source, None).expect("tree");
        tree_depth(tree.root_node(), u32::MAX)
    }

    #[test]
    fn brackets_in_a_string_do_not_count() {
        let source = format!("RETURN '{}';", "(".repeat(5_000));
        assert_eq!(
            max_bracket_depth(&source),
            0,
            "a string literal full of parentheses must not read as nesting"
        );
    }

    #[test]
    fn brackets_in_a_comment_do_not_count() {
        let source = format!("-- {}\nRETURN 1;", "(".repeat(5_000));
        assert_eq!(max_bracket_depth(&source), 0);
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        let source = "RETURN 'a\\'((((b';";
        assert_eq!(
            max_bracket_depth(source),
            0,
            "the escaped quote must not close the string early"
        );
    }

    #[test]
    fn ordinary_nesting_is_counted() {
        assert_eq!(max_bracket_depth("RETURN ((1));"), 2);
        assert_eq!(max_bracket_depth("RETURN [{ a: [1] }];"), 3);
    }

    #[test]
    fn real_nesting_trips_the_threshold() {
        let source = format!("RETURN {}1{};", "(".repeat(5_000), ")".repeat(5_000));
        assert!(max_bracket_depth(&source) > MAX_NODE_DEPTH);
    }

    #[test]
    fn a_flat_statement_is_shallow() {
        // Whatever the exact wrapper count, ordinary SurrealQL must sit far
        // below the cap: the corpus measurement says under 30.
        let depth = depth_of("DEFINE TABLE person SCHEMAFULL;");
        assert!(depth > 1, "a real statement has structure");
        assert!(
            depth < 30,
            "an ordinary statement measured {depth}, which is near the cap"
        );
    }

    #[test]
    fn nesting_deepens_the_measurement() {
        let shallow = depth_of("RETURN (1);");
        let deeper = depth_of("RETURN ((((((((((1))))))))));");
        assert!(
            deeper > shallow,
            "nesting must register: {shallow} then {deeper}"
        );
    }

    #[test]
    fn the_ceiling_stops_the_walk_early() {
        let source = format!("RETURN {}1{};", "(".repeat(5_000), ")".repeat(5_000));
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language()).expect("grammar");
        let tree = parser.parse(&source, None).expect("tree");
        let measured = tree_depth(tree.root_node(), MAX_NODE_DEPTH);
        assert!(
            measured > MAX_NODE_DEPTH,
            "the guard must see this as over the cap, measured {measured}"
        );
    }

    #[test]
    fn measuring_does_not_recurse() {
        // The point of the iterative walk: the measurement itself must survive
        // input far past what any recursive walk could. 200k parens is ~400k
        // levels of tree.
        let source = format!("RETURN {}1{};", "(".repeat(200_000), ")".repeat(200_000));
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language()).expect("grammar");
        let tree = parser.parse(&source, None).expect("tree");
        assert!(tree_depth(tree.root_node(), MAX_NODE_DEPTH) > MAX_NODE_DEPTH);
    }
}
