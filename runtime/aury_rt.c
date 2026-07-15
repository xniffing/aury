// Aury native runtime — linked into every `aury compile`d executable.
//
// Provides the heap-backed operations for the parts of Aury that are tedious
// or fragile to emit as inline LLVM IR: string allocation/concat/compare,
// i64<->str conversion, and the result(i64,str) constructor. The lowering
// (src/lower.rs) declares these `extern` and calls them; `cmd_compile` links
// this file alongside the generated LLVM IR.
//
// v0 memory model: allocations are never freed (Aury values are immutable;
// the process is short-lived). Region-based arenas (the proposal's actual
// memory model) are the planned next step.

#include <ctype.h>
#include <errno.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

/* aury str  ==  ptr to { i64 len, i8* data }   (matches the lowering's layout) */
typedef struct { int64_t len; char* data; } aury_str_t;
typedef aury_str_t* aury_str;

/* aury result(i64,str)  ==  ptr to { i1 ok, i64 val, aury_str err }
   (int8_t ok maps to LLVM i1 in the first byte; i64 val at offset 8.) */
typedef struct { int8_t ok; int64_t val; aury_str err; } aury_result_t;
typedef aury_result_t* aury_result;

aury_str aury_str_concat(aury_str a, aury_str b) {
    int64_t n = a->len + b->len;
    char* d = (char*)malloc((size_t)(n + 1));
    memcpy(d, a->data, (size_t)a->len);
    memcpy(d + a->len, b->data, (size_t)b->len);
    d[n] = 0;
    aury_str r = (aury_str)malloc(sizeof(aury_str_t));
    r->len = n; r->data = d;
    return r;
}

int64_t aury_str_eq(aury_str a, aury_str b) {
    if (a->len != b->len) return 0;
    return memcmp(a->data, b->data, (size_t)a->len) == 0 ? 1 : 0;
}

aury_str aury_i64_to_str(int64_t n) {
    char buf[32];
    int len = snprintf(buf, sizeof(buf), "%lld", (long long)n);
    if (len < 0) len = 0;
    char* d = (char*)malloc((size_t)(len + 1));
    memcpy(d, buf, (size_t)len);
    d[len] = 0;
    aury_str r = (aury_str)malloc(sizeof(aury_str_t));
    r->len = len; r->data = d;
    return r;
}

static aury_str make_string(const char* data, int64_t len) {
    char* copy = (char*)malloc((size_t)len + 1);
    memcpy(copy, data, (size_t)len);
    copy[len] = 0;
    aury_str result = (aury_str)malloc(sizeof(aury_str_t));
    result->len = len;
    result->data = copy;
    return result;
}

static aury_result parse_i64(aury_str s, int trim) {
    int64_t start = 0;
    int64_t end = s->len;
    if (trim) {
        while (start < end && isspace((unsigned char)s->data[start])) start++;
        while (end > start && isspace((unsigned char)s->data[end - 1])) end--;
    }

    int64_t len = end - start;
    char* text = (char*)malloc((size_t)len + 1);
    memcpy(text, s->data + start, (size_t)len);
    text[len] = 0;

    errno = 0;
    char* parsed_end = text;
    intmax_t parsed = strtoimax(text, &parsed_end, 10);
    int valid = len > 0
        && (trim || !isspace((unsigned char)text[0]))
        && parsed_end == text + len
        && errno != ERANGE
        && parsed >= INT64_MIN
        && parsed <= INT64_MAX;
    free(text);

    aury_result result = (aury_result)malloc(sizeof(aury_result_t));
    result->ok = valid ? 1 : 0;
    result->val = valid ? (int64_t)parsed : 0;
    result->err = valid ? NULL : make_string("not an i64", 10);
    return result;
}

/* `i64.parse` trims like Rust's `str::trim().parse::<i64>()`. */
aury_result aury_i64_parse(aury_str s) {
    return parse_i64(s, 1);
}

/* Casts use strict `str::parse::<i64>()` semantics (no surrounding space). */
aury_result aury_i64_parse_strict(aury_str s) {
    return parse_i64(s, 0);
}

/* Print using the interpreter's string Debug representation, including quotes. */
void aury_str_print(aury_str s) {
    putchar('"');
    for (int64_t i = 0; i < s->len; i++) {
        unsigned char byte = (unsigned char)s->data[i];
        switch (byte) {
            case 0: fputs("\\0", stdout); break;
            case '\t': fputs("\\t", stdout); break;
            case '\n': fputs("\\n", stdout); break;
            case '\r': fputs("\\r", stdout); break;
            case '\\': fputs("\\\\", stdout); break;
            case '"': fputs("\\\"", stdout); break;
            default:
                if (byte < 0x20 || byte == 0x7f) {
                    fprintf(stdout, "\\u{%x}", byte);
                } else {
                    putchar(byte);
                }
        }
    }
    fputs("\"\n", stdout);
}
