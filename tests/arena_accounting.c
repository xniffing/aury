/* Standalone accounting check for the region arena in runtime/aury_rt.c.
 * Compiled and run by the `arena_frees_region_allocations` integration test.
 * Exercises enter/alloc/exit and asserts the live-allocation count returns to
 * baseline (and tracks correctly across nested regions). Exit code 0 = pass;
 * a nonzero code identifies the failing assertion. */
#include <stdint.h>

extern void aury_region_enter(void);
extern void aury_region_exit(void);
extern int64_t aury_region_exit_keep(int64_t result, const char *descriptor);
extern int64_t aury_live_allocations(void);
extern void *aury_vec_new(int64_t len);
extern int64_t *aury_box_new(int64_t slots);

/* aury_vec_t layout must match the runtime for the relocation check below. */
typedef struct { int64_t len; int64_t *slots; } arena_vec_t;

int main(void) {
    if (aury_live_allocations() != 0) return 1; /* baseline */

    /* A single region: a vec (vec struct + slots = 2) and a box (1) = 3 live. */
    aury_region_enter();
    aury_vec_new(3);
    aury_box_new(4);
    if (aury_live_allocations() != 3) return 2;
    aury_region_exit();
    if (aury_live_allocations() != 0) return 3; /* all scratch freed */

    /* Nested regions free their own frame only. */
    aury_region_enter();
    aury_vec_new(1); /* +2 */
    aury_region_enter();
    aury_vec_new(1); /* +2 */
    if (aury_live_allocations() != 4) return 4;
    aury_region_exit();
    if (aury_live_allocations() != 2) return 5; /* inner freed */
    aury_region_exit();
    if (aury_live_allocations() != 0) return 6; /* outer freed */

    /* Allocations outside any region are process-lifetime, never tracked. */
    aury_vec_new(2);
    if (aury_live_allocations() != 0) return 7;

    /* exit_keep relocates an aggregate result out of the (top-level) region, so
     * the returned vector survives while the region's own allocations are freed
     * back to baseline. */
    aury_region_enter();
    arena_vec_t *scratch = (arena_vec_t *)aury_vec_new(3); /* +2 */
    scratch->slots[0] = 11;
    scratch->slots[1] = 22;
    scratch->slots[2] = 33;
    if (aury_live_allocations() != 2) return 8;
    arena_vec_t *kept = (arena_vec_t *)(intptr_t)aury_region_exit_keep(
        (int64_t)(intptr_t)scratch, "vi");
    if (aury_live_allocations() != 0) return 9;   /* scratch frame freed */
    if (kept->len != 3) return 10;                /* result survived */
    if (kept->slots[0] != 11 || kept->slots[2] != 33) return 11;

    return 0;
}
