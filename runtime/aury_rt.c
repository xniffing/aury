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

aury_result aury_i64_parse(aury_str s) {
    aury_result r = (aury_result)malloc(sizeof(aury_result_t));
    const char* d = s->data;
    int64_t n = 0; int i = 0; int neg = 0; int any = 0;
    if (d[0] == '-') { neg = 1; i = 1; } else if (d[0] == '+') { i = 1; }
    for (; i < s->len; i++) {
        if (d[i] < '0' || d[i] > '9') break;
        n = n * 10 + (d[i] - '0');
        any = 1;
    }
    if (neg) n = -n;
    r->ok = any ? 1 : 0;
    r->val = any ? n : 0;
    r->err = 0;
    return r;
}