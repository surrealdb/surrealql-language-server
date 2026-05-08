// Freestanding `<stdlib.h>` shim for the wasm32-unknown-unknown
// build of the SurrealQL tree-sitter parser. Mirrors the shim that
// `tree-sitter-language` ships with — we vendor it locally so our
// build.rs doesn't have to chase the dep's metadata env vars.
//
// The matching definitions of `malloc` / `calloc` / `realloc` /
// `free` / `abort` are provided by the `tree-sitter` crate's
// `wasm/src/stdlib.c`, which is compiled into the final wasm module
// automatically when `TARGET == wasm32-unknown-unknown`.

#ifndef SURREALQL_WASM_STDLIB_H_
#define SURREALQL_WASM_STDLIB_H_

#include <stddef.h>
#include <stdint.h>

#ifndef NULL
#define NULL ((void*)0)
#endif

void* malloc(size_t);
void* calloc(size_t, size_t);
void free(void*);
void* realloc(void*, size_t);

__attribute__((noreturn)) void abort(void);

#endif // SURREALQL_WASM_STDLIB_H_
