#include "tree_sitter/parser.h"
#include <stdbool.h>
#include <stdint.h>

#if defined(__wasm__) || defined(__WASM__)
#define DBG(...) ((void)0)
#else
#include <stdio.h>
#include <stdlib.h>
#define DBG(...) do { if (getenv("TSDBG")) fprintf(stderr, __VA_ARGS__); } while (0)
#endif

// External tokens emitted by the SurrealQL scanner.
// Must stay in sync with the `externals:` declaration in grammar.js.
enum TokenType {
    JS_FUNCTION_BODY,
    OBJECT_OPEN,
};

// ---------------------------------------------------------------------------
// JS function body scanner (kept from previous grammar)
// ---------------------------------------------------------------------------

void *tree_sitter_surrealql_external_scanner_create(void) { return NULL; }
void tree_sitter_surrealql_external_scanner_destroy(void *payload) { (void)payload; }
void tree_sitter_surrealql_external_scanner_reset(void *payload) { (void)payload; }
unsigned tree_sitter_surrealql_external_scanner_serialize(void *payload, char *buffer) {
    (void)payload;
    (void)buffer;
    return 0;
}
void tree_sitter_surrealql_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
    (void)payload;
    (void)buffer;
    (void)length;
}

static void advance(TSLexer *lexer) { lexer->advance(lexer, false); }
static void skip(TSLexer *lexer) { lexer->advance(lexer, true); }

// Scan a JS single-line string delimited by `quote`. Handles \-escapes.
static void scan_js_string(TSLexer *lexer, int32_t quote) {
    while (lexer->lookahead != 0 && lexer->lookahead != '\n') {
        if (lexer->lookahead == '\\') {
            advance(lexer);
            if (lexer->lookahead != 0) advance(lexer);
        } else if (lexer->lookahead == quote) {
            advance(lexer);
            return;
        } else {
            advance(lexer);
        }
    }
}

static void scan_template_literal(TSLexer *lexer) {
    while (lexer->lookahead != 0) {
        if (lexer->lookahead == '\\') {
            advance(lexer);
            if (lexer->lookahead != 0) advance(lexer);
        } else if (lexer->lookahead == '`') {
            advance(lexer);
            return;
        } else if (lexer->lookahead == '$') {
            advance(lexer);
            if (lexer->lookahead == '{') {
                advance(lexer);
                int depth = 1;
                while (lexer->lookahead != 0 && depth > 0) {
                    int32_t c = lexer->lookahead;
                    advance(lexer);
                    if (c == '{') depth++;
                    else if (c == '}') depth--;
                    else if (c == '\'' || c == '"') scan_js_string(lexer, c);
                    else if (c == '`') scan_template_literal(lexer);
                }
            }
        } else {
            advance(lexer);
        }
    }
}

// Scan a JS function body — the entire `{...}` including its braces.
// The grammar's `JavaScriptBlock` rule simply references this token. We keep
// the leading `{` requirement so the scanner does not consume regular
// SurrealQL during error recovery at other `{`-adjacent positions.
static bool scan_js_function_body(TSLexer *lexer) {
    while (lexer->lookahead == ' ' || lexer->lookahead == '\t' ||
           lexer->lookahead == '\n' || lexer->lookahead == '\r') {
        skip(lexer);
    }

    if (lexer->lookahead != '{') return false;
    advance(lexer);

    int depth = 1;
    while (depth > 0 && lexer->lookahead != 0) {
        int32_t c = lexer->lookahead;
        advance(lexer);

        if (c == '{') depth++;
        else if (c == '}') depth--;
        else if (c == '\'') scan_js_string(lexer, '\'');
        else if (c == '"') scan_js_string(lexer, '"');
        else if (c == '`') scan_template_literal(lexer);
        else if (c == '/') {
            if (lexer->lookahead == '/') {
                advance(lexer);
                while (lexer->lookahead != 0 && lexer->lookahead != '\n') advance(lexer);
            } else if (lexer->lookahead == '*') {
                advance(lexer);
                while (lexer->lookahead != 0) {
                    if (lexer->lookahead == '*') {
                        advance(lexer);
                        if (lexer->lookahead == '/') { advance(lexer); break; }
                    } else {
                        advance(lexer);
                    }
                }
            }
        }
    }

    lexer->result_symbol = JS_FUNCTION_BODY;
    return true;
}

// ---------------------------------------------------------------------------
// Object-open scanner — mirrors lezer's tokens.js `objectToken`.
//
// Emits OBJECT_OPEN when `{` is followed by either:
//   - another `{` is NOT next (otherwise it's a Block),
//   - immediately `}` (empty object), or
//   - an identifier/string key followed by `:` (looking past whitespace +
//     comments).
// ---------------------------------------------------------------------------

static inline bool is_space(int32_t c) {
    return c == ' ' || c == '\t' || c == '\n' || c == '\r';
}

static inline bool is_id_char(int32_t c) {
    return c == '_' ||
           (c >= 'A' && c <= 'Z') ||
           (c >= 'a' && c <= 'z') ||
           (c >= '0' && c <= '9');
}

// Skip whitespace and line comments after the opening `{`. Mirrors lezer's
// skipSpace in tokens.js (handles #, //, --).
static void skip_obj_whitespace(TSLexer *lexer) {
    while (true) {
        int32_t c = lexer->lookahead;
        if (is_space(c)) {
            skip(lexer);
            continue;
        }
        if (c == '#') {
            skip(lexer);
            while (lexer->lookahead != 0 && lexer->lookahead != '\n') skip(lexer);
            continue;
        }
        if ((c == '/' || c == '-') && lexer->lookahead != 0) {
            // Peek the next char: we need lookahead == lookahead too. tree-sitter
            // doesn't have a peek-2 API, so we eagerly advance and check.
            skip(lexer);
            if (lexer->lookahead == c) {
                skip(lexer);
                while (lexer->lookahead != 0 && lexer->lookahead != '\n') skip(lexer);
                continue;
            }
            // Not a comment — we already consumed a char, so we cannot
            // back-track. Bail out — caller will treat next non-space char as
            // the object key (which is correct for our purposes since `/` and
            // `-` are not valid identifier starts).
            return;
        }
        break;
    }
}

// Try to consume an identifier-like or string key starting at the current
// lookahead position. Returns true if a key was consumed.
static bool consume_obj_key(TSLexer *lexer) {
    int32_t c = lexer->lookahead;
    if (is_id_char(c) && !(c >= '0' && c <= '9')) {
        // identifier
        while (is_id_char(lexer->lookahead)) skip(lexer);
        return true;
    }
    if (c >= '0' && c <= '9') {
        // numeric key — used by some object literals; treat as identifier-like
        while (is_id_char(lexer->lookahead)) skip(lexer);
        return true;
    }
    if (c == '\'' || c == '"') {
        int32_t quote = c;
        skip(lexer);
        while (lexer->lookahead != 0) {
            if (lexer->lookahead == '\\') {
                skip(lexer);
                if (lexer->lookahead != 0) skip(lexer);
                continue;
            }
            if (lexer->lookahead == quote) {
                skip(lexer);
                return true;
            }
            skip(lexer);
        }
        return false;
    }
    return false;
}

static bool scan_object_open(TSLexer *lexer) {
    // Skip any leading whitespace/comments that come before the `{`. Tree-sitter
    // sometimes invokes the external scanner before extras are processed, so
    // we need to be tolerant here. Using skip() so they don't extend the token.
    while (true) {
        int32_t c = lexer->lookahead;
        if (is_space(c)) {
            skip(lexer);
            continue;
        }
        if (c == '#') {
            skip(lexer);
            while (lexer->lookahead != 0 && lexer->lookahead != '\n') skip(lexer);
            continue;
        }
        break;
    }

    if (lexer->lookahead != '{') return false;
    // Consume the `{`. We must use advance() so the token includes it.
    advance(lexer);
    lexer->mark_end(lexer);

    // Skip whitespace and comments — we use skip() so they don't extend the
    // token, but we already marked the end past `{`.
    skip_obj_whitespace(lexer);

    int32_t c = lexer->lookahead;

    if (c == '{') {
        // Direct nested brace → this is a Block, not an Object.
        return false;
    }
    if (c == '}') {
        // Empty {} — treat as Object.
        lexer->result_symbol = OBJECT_OPEN;
        return true;
    }
    // Otherwise look for `key:`
    if (!consume_obj_key(lexer)) return false;
    skip_obj_whitespace(lexer);
    if (lexer->lookahead == ':') {
        lexer->result_symbol = OBJECT_OPEN;
        return true;
    }
    return false;
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

bool tree_sitter_surrealql_external_scanner_scan(
    void *payload,
    TSLexer *lexer,
    const bool *valid_symbols
) {
    (void)payload;

    DBG("scan: valid=[js=%d,obj=%d] la='%c'(%d)\n",
        valid_symbols[JS_FUNCTION_BODY], valid_symbols[OBJECT_OPEN],
        lexer->lookahead >= 32 && lexer->lookahead < 127 ? (char)lexer->lookahead : '?',
        lexer->lookahead);

    if (valid_symbols[OBJECT_OPEN]) {
        bool ok = scan_object_open(lexer);
        DBG("  object_open => %d\n", ok);
        if (ok) return true;
    }

    if (valid_symbols[JS_FUNCTION_BODY]) {
        if (scan_js_function_body(lexer)) return true;
    }

    return false;
}
