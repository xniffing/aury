// Aury native runtime. Immutable aggregate values use uniform 8-byte slots:
// vectors are { i64 len, i64* slots }, while structs and results are boxed
// contiguous slot arrays. Allocations intentionally live for the process.

#include <ctype.h>
#include <errno.h>
#include <inttypes.h>
#include <limits.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct { int64_t len; char *data; } aury_str_t;
typedef aury_str_t *aury_str;
typedef struct { int64_t len; int64_t *slots; } aury_vec_t;
typedef struct { int64_t tag; int64_t payload; } aury_result_t;

static void *checked_calloc(size_t count, size_t size) {
    if (size != 0 && count > SIZE_MAX / size) abort();
    void *value = calloc(count == 0 ? 1 : count, size == 0 ? 1 : size);
    if (value == NULL) abort();
    return value;
}

int64_t *aury_box_new(int64_t slots) {
    if (slots < 0) abort();
    return (int64_t *)checked_calloc((size_t)slots, sizeof(int64_t));
}

int64_t *aury_box_slot(int64_t *box, int64_t index) {
    if (index < 0) abort();
    return box + index;
}

aury_vec_t *aury_vec_new(int64_t len) {
    if (len < 0) abort();
    aury_vec_t *value = (aury_vec_t *)checked_calloc(1, sizeof(aury_vec_t));
    value->len = len;
    value->slots = aury_box_new(len);
    return value;
}

int64_t *aury_vec_slot(aury_vec_t *value, int64_t index) {
    if (index < 0 || index >= value->len) abort();
    return value->slots + index;
}

// Value-semantics append: returns a fresh vector of length len+1 with `elem`
// (already coerced to i64 slot bits by the caller) at the end. The interpreter
// clones on push, so a growable vec never mutates its source in place — this
// keeps native observably identical.
aury_vec_t *aury_vec_push(aury_vec_t *src, int64_t elem) {
    int64_t n = src ? src->len : 0;
    aury_vec_t *out = aury_vec_new(n + 1);
    for (int64_t i = 0; i < n; i++) {
        out->slots[i] = src->slots[i];
    }
    out->slots[n] = elem;
    return out;
}

static uint64_t rng_seed;
static uint64_t rng_step;

void aury_rng_init(uint64_t seed) {
    rng_seed = seed;
    rng_step = 0;
}

int64_t aury_rng_next(void) {
    rng_step += UINT64_C(0x9E3779B97F4A7C15);
    uint64_t z = rng_seed + rng_step;
    z = (z ^ (z >> 30)) * UINT64_C(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)) * UINT64_C(0x94D049BB133111EB);
    return (int64_t)(z ^ (z >> 31));
}

int64_t aury_i64_div(int64_t a, int64_t b) {
    if (b == 0) abort();
    if (a == INT64_MIN && b == -1) return INT64_MIN;
    return a / b;
}

int64_t aury_i64_mod(int64_t a, int64_t b) {
    if (b == 0) abort();
    if (a == INT64_MIN && b == -1) return 0;
    return a % b;
}

static uint32_t decode_utf8(const unsigned char *data, int64_t len, int *width) {
    unsigned char first = data[0];
    if (first < 0x80 || len < 2) { *width = 1; return first; }
    if ((first & 0xe0) == 0xc0 && len >= 2) {
        *width = 2; return ((uint32_t)(first & 0x1f) << 6) | (data[1] & 0x3f);
    }
    if ((first & 0xf0) == 0xe0 && len >= 3) {
        *width = 3;
        return ((uint32_t)(first & 0x0f) << 12) | ((uint32_t)(data[1] & 0x3f) << 6) | (data[2] & 0x3f);
    }
    if ((first & 0xf8) == 0xf0 && len >= 4) {
        *width = 4;
        return ((uint32_t)(first & 7) << 18) | ((uint32_t)(data[1] & 0x3f) << 12)
            | ((uint32_t)(data[2] & 0x3f) << 6) | (data[3] & 0x3f);
    }
    *width = 1; return first;
}

static int rust_whitespace(uint32_t cp) {
    return (cp >= 0x09 && cp <= 0x0d) || cp == 0x20 || cp == 0x85 || cp == 0xa0
        || cp == 0x1680 || (cp >= 0x2000 && cp <= 0x200a) || cp == 0x2028
        || cp == 0x2029 || cp == 0x202f || cp == 0x205f || cp == 0x3000;
}

static int rust_control(uint32_t cp) { return cp <= 0x1f || (cp >= 0x7f && cp <= 0x9f); }

static aury_str make_string(const char *data, int64_t len) {
    char *copy = (char *)checked_calloc((size_t)len + 1, 1);
    memcpy(copy, data, (size_t)len);
    aury_str result = (aury_str)checked_calloc(1, sizeof(aury_str_t));
    result->len = len; result->data = copy;
    return result;
}

aury_str aury_str_concat(aury_str a, aury_str b) {
    if (a->len > INT64_MAX - b->len) abort();
    int64_t len = a->len + b->len;
    char *data = (char *)checked_calloc((size_t)len + 1, 1);
    memcpy(data, a->data, (size_t)a->len);
    memcpy(data + a->len, b->data, (size_t)b->len);
    aury_str result = (aury_str)checked_calloc(1, sizeof(aury_str_t));
    result->len = len; result->data = data;
    return result;
}

int64_t aury_str_eq(aury_str a, aury_str b) {
    return a->len == b->len && memcmp(a->data, b->data, (size_t)a->len) == 0;
}

aury_str aury_i64_to_str(int64_t number) {
    char buffer[32];
    int len = snprintf(buffer, sizeof(buffer), "%" PRId64, number);
    return make_string(buffer, len < 0 ? 0 : len);
}

// Canonical f64 -> decimal. MUST stay byte-identical to `interp::format_f64`
// in Rust: finite values use 17 significant digits in normalized scientific
// form (`%.16e`, correctly rounded, same digits Rust's `{:.16e}` emits);
// NaN/inf are spelled explicitly because the two libraries disagree otherwise.
static int format_f64(double x, char *buffer, size_t cap) {
    if (isnan(x)) return snprintf(buffer, cap, "%s", "NaN");
    if (isinf(x)) return snprintf(buffer, cap, "%s", x < 0 ? "-inf" : "inf");
    return snprintf(buffer, cap, "%.16e", x);
}

aury_str aury_f64_to_str(double x) {
    char buffer[64];
    int len = format_f64(x, buffer, sizeof(buffer));
    return make_string(buffer, len < 0 ? 0 : len);
}

// f64 -> i64 with the same saturating semantics as Rust's `x as i64`: NaN maps
// to 0, out-of-range magnitudes clamp to INT64_MIN/MAX, otherwise truncate
// toward zero. LLVM `fptosi` would be undefined on those edges, so casts route
// through here to keep the native backend in lockstep with the interpreter.
int64_t aury_f64_to_i64(double x) {
    if (isnan(x)) return 0;
    if (x >= 9223372036854775808.0) return INT64_MAX;   // >= 2^63
    if (x <= -9223372036854775808.0) return INT64_MIN;  // <= -2^63
    return (int64_t)x;
}

static aury_result_t *parse_i64(aury_str string, int trim) {
    int64_t start = 0, end = string->len;
    if (trim) {
        while (start < end) {
            int width = 1;
            uint32_t cp = decode_utf8((const unsigned char *)string->data + start, end - start, &width);
            if (!rust_whitespace(cp)) break;
            start += width;
        }
        while (end > start) {
            int64_t previous = end - 1;
            while (previous > start && (((unsigned char)string->data[previous] & 0xc0) == 0x80)) previous--;
            int width = 1;
            uint32_t cp = decode_utf8((const unsigned char *)string->data + previous, end - previous, &width);
            if (!rust_whitespace(cp) || previous + width != end) break;
            end = previous;
        }
    }
    int64_t len = end - start;
    char *text = (char *)checked_calloc((size_t)len + 1, 1);
    memcpy(text, string->data + start, (size_t)len);
    errno = 0;
    char *parsed_end = text;
    intmax_t parsed = strtoimax(text, &parsed_end, 10);
    int first_width = 1;
    uint32_t first_cp = len > 0 ? decode_utf8((const unsigned char *)text, len, &first_width) : 0;
    int valid = len > 0 && (trim || !rust_whitespace(first_cp)) && parsed_end == text + len
        && errno != ERANGE && parsed >= INT64_MIN && parsed <= INT64_MAX;
    free(text);

    aury_result_t *result = (aury_result_t *)checked_calloc(1, sizeof(aury_result_t));
    result->tag = valid ? 1 : 0;
    if (valid) {
        result->payload = (int64_t)parsed;
    } else {
        static const char prefix[] = "not an i64: ";
        int64_t error_len = (int64_t)(sizeof(prefix) - 1) + string->len;
        char *error = (char *)checked_calloc((size_t)error_len + 1, 1);
        memcpy(error, prefix, sizeof(prefix) - 1);
        memcpy(error + sizeof(prefix) - 1, string->data, (size_t)string->len);
        result->payload = (int64_t)(intptr_t)make_string(error, error_len);
        free(error);
    }
    return result;
}

aury_result_t *aury_i64_parse(aury_str string) { return parse_i64(string, 1); }
aury_result_t *aury_i64_parse_strict(aury_str string) { return parse_i64(string, 0); }

static void print_string(aury_str string) {
    putchar('"');
    for (int64_t i = 0; i < string->len;) {
        unsigned char byte = (unsigned char)string->data[i];
        if (byte == 0) { fputs("\\0", stdout); i++; }
        else if (byte == '\t') { fputs("\\t", stdout); i++; }
        else if (byte == '\n') { fputs("\\n", stdout); i++; }
        else if (byte == '\r') { fputs("\\r", stdout); i++; }
        else if (byte == '\\') { fputs("\\\\", stdout); i++; }
        else if (byte == '"') { fputs("\\\"", stdout); i++; }
        else {
            int width = 1;
            uint32_t cp = decode_utf8((const unsigned char *)string->data + i, string->len - i, &width);
            if (rust_control(cp)) fprintf(stdout, "\\u{%x}", cp);
            else fwrite(string->data + i, 1, (size_t)width, stdout);
            i += width;
        }
    }
    putchar('"');
}

static uint64_t descriptor_number(const char **descriptor) {
    uint64_t value = 0;
    while (**descriptor >= '0' && **descriptor <= '9') {
        value = value * 10 + (uint64_t)(*(*descriptor)++ - '0');
    }
    if (*(*descriptor)++ != ':') abort();
    return value;
}

static const char *skip_descriptor(const char *descriptor) {
    char kind = *descriptor++;
    if (kind == 'i' || kind == 'b' || kind == 's' || kind == 'u' || kind == 'f') return descriptor;
    if (kind == 'v') return skip_descriptor(descriptor);
    if (kind == 'r') return skip_descriptor(skip_descriptor(descriptor));
    if (kind == 't') {
        uint64_t name_len = descriptor_number(&descriptor);
        descriptor += name_len;
        uint64_t fields = descriptor_number(&descriptor);
        for (uint64_t i = 0; i < fields; i++) {
            uint64_t field_len = descriptor_number(&descriptor);
            descriptor += field_len;
            descriptor = skip_descriptor(descriptor);
        }
        return descriptor;
    }
    abort();
}

static void print_value(int64_t bits, const char **descriptor) {
    char kind = *(*descriptor)++;
    if (kind == 'i') { fprintf(stdout, "%" PRId64, bits); return; }
    if (kind == 'f') {
        // Slots hold the raw IEEE bits; reinterpret and print canonically.
        double value;
        memcpy(&value, &bits, sizeof(value));
        char buffer[64];
        format_f64(value, buffer, sizeof(buffer));
        fputs(buffer, stdout);
        return;
    }
    if (kind == 'b') { fputs(bits ? "true" : "false", stdout); return; }
    if (kind == 's') { print_string((aury_str)(intptr_t)bits); return; }
    if (kind == 'u') { fputs("unit", stdout); return; }
    if (kind == 'v') {
        aury_vec_t *vector = (aury_vec_t *)(intptr_t)bits;
        const char *element = *descriptor;
        const char *after = skip_descriptor(element);
        putchar('[');
        for (int64_t i = 0; i < vector->len; i++) {
            if (i != 0) fputs(", ", stdout);
            const char *cursor = element;
            print_value(vector->slots[i], &cursor);
        }
        putchar(']');
        *descriptor = after;
        return;
    }
    if (kind == 't') {
        uint64_t name_len = descriptor_number(descriptor);
        fwrite(*descriptor, 1, (size_t)name_len, stdout);
        *descriptor += name_len;
        uint64_t fields = descriptor_number(descriptor);
        int64_t *slots = (int64_t *)(intptr_t)bits;
        putchar('{');
        for (uint64_t i = 0; i < fields; i++) {
            if (i != 0) fputs(", ", stdout);
            uint64_t field_len = descriptor_number(descriptor);
            fwrite(*descriptor, 1, (size_t)field_len, stdout);
            *descriptor += field_len;
            fputs(": ", stdout);
            print_value(slots[i], descriptor);
        }
        putchar('}');
        return;
    }
    if (kind == 'r') {
        aury_result_t *result = (aury_result_t *)(intptr_t)bits;
        const char *ok = *descriptor;
        const char *err = skip_descriptor(ok);
        if (result->tag) {
            fputs("ok(", stdout);
            const char *cursor = ok;
            print_value(result->payload, &cursor);
            *descriptor = skip_descriptor(err);
        } else {
            fputs("err(", stdout);
            const char *cursor = err;
            print_value(result->payload, &cursor);
            *descriptor = cursor;
        }
        putchar(')');
        return;
    }
    abort();
}

void aury_value_print(int64_t bits, const char *descriptor) {
    print_value(bits, &descriptor);
    putchar('\n');
}

void aury_str_print(aury_str string) {
    print_string(string);
    putchar('\n');
}
