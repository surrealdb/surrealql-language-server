// Freestanding `<string.h>` shim for the wasm32-unknown-unknown
// build. See [`stdlib.h`] for the rationale; the matching mem* /
// str* implementations live in the `tree-sitter` crate's
// `wasm/src/string.c`.

#ifndef SURREALQL_WASM_STRING_H_
#define SURREALQL_WASM_STRING_H_

#include <stddef.h>
#include <stdint.h>

int memcmp(const void *lhs, const void *rhs, size_t count);
void *memcpy(void *restrict dst, const void *restrict src, size_t size);
void *memmove(void *dst, const void *src, size_t count);
void *memset(void *dst, int value, size_t count);
int strncmp(const char *left, const char *right, size_t n);
size_t strlen(const char *str);

#endif // SURREALQL_WASM_STRING_H_
