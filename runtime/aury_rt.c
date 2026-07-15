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

static uint32_t decode_utf8(const unsigned char* data, int64_t len, int* width) {
    unsigned char first = data[0];
    if (first < 0x80 || len < 2) {
        *width = 1;
        return first;
    }
    if ((first & 0xe0) == 0xc0 && len >= 2) {
        *width = 2;
        return ((uint32_t)(first & 0x1f) << 6) | (data[1] & 0x3f);
    }
    if ((first & 0xf0) == 0xe0 && len >= 3) {
        *width = 3;
        return ((uint32_t)(first & 0x0f) << 12)
            | ((uint32_t)(data[1] & 0x3f) << 6)
            | (data[2] & 0x3f);
    }
    if ((first & 0xf8) == 0xf0 && len >= 4) {
        *width = 4;
        return ((uint32_t)(first & 0x07) << 18)
            | ((uint32_t)(data[1] & 0x3f) << 12)
            | ((uint32_t)(data[2] & 0x3f) << 6)
            | (data[3] & 0x3f);
    }
    *width = 1;
    return first;
}

/* The Unicode White_Space set used by Rust `char::is_whitespace`. */
static int rust_whitespace(uint32_t cp) {
    return (cp >= 0x09 && cp <= 0x0d)
        || cp == 0x20
        || cp == 0x85
        || cp == 0xa0
        || cp == 0x1680
        || (cp >= 0x2000 && cp <= 0x200a)
        || cp == 0x2028
        || cp == 0x2029
        || cp == 0x202f
        || cp == 0x205f
        || cp == 0x3000;
}

static int rust_control(uint32_t cp) {
    return cp <= 0x1f || (cp >= 0x7f && cp <= 0x9f);
}

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
        while (start < end) {
            int width = 1;
            uint32_t cp = decode_utf8(
                (const unsigned char*)s->data + start,
                end - start,
                &width
            );
            if (!rust_whitespace(cp)) break;
            start += width;
        }
        while (end > start) {
            int64_t previous = end - 1;
            while (previous > start
                && (((unsigned char)s->data[previous] & 0xc0) == 0x80)) {
                previous--;
            }
            int width = 1;
            uint32_t cp = decode_utf8(
                (const unsigned char*)s->data + previous,
                end - previous,
                &width
            );
            if (!rust_whitespace(cp) || previous + width != end) break;
            end = previous;
        }
    }

    int64_t len = end - start;
    char* text = (char*)malloc((size_t)len + 1);
    memcpy(text, s->data + start, (size_t)len);
    text[len] = 0;

    errno = 0;
    char* parsed_end = text;
    intmax_t parsed = strtoimax(text, &parsed_end, 10);
    int first_width = 1;
    uint32_t first_cp = len > 0
        ? decode_utf8((const unsigned char*)text, len, &first_width)
        : 0;
    int valid = len > 0
        && (trim || !rust_whitespace(first_cp))
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
    for (int64_t i = 0; i < s->len;) {
        unsigned char byte = (unsigned char)s->data[i];
        if (byte == 0) {
            fputs("\\0", stdout);
            i++;
        } else if (byte == '\t') {
            fputs("\\t", stdout);
            i++;
        } else if (byte == '\n') {
            fputs("\\n", stdout);
            i++;
        } else if (byte == '\r') {
            fputs("\\r", stdout);
            i++;
        } else if (byte == '\\') {
            fputs("\\\\", stdout);
            i++;
        } else if (byte == '"') {
            fputs("\\\"", stdout);
            i++;
        } else {
            int width = 1;
            uint32_t cp = decode_utf8(
                (const unsigned char*)s->data + i,
                s->len - i,
                &width
            );
            if (rust_control(cp)) {
                fprintf(stdout, "\\u{%x}", cp);
            } else {
                fwrite(s->data + i, 1, (size_t)width, stdout);
            }
            i += width;
        }
    }
    fputs("\"\n", stdout);
}
