//! End-to-end integration tests for Aury v0.
//!
//! These exercise the full pipeline the proposal centers on:
//!   - parse + Merkle ids
//!   - validate (type/effect/region) → structured rejections with admissible repairs
//!   - the closed repair loop (validate → apply admissible patch → re-validate → accept)
//!   - intent verification: property tests with shrinking + vacuity check

use aury::{
    ast::build_module,
    interp::Interp,
    repair::{Gate, ValidationOutcome},
    sexpr::parse,
    validate::check_module,
};

fn module(src: &str) -> aury::ast::Module {
    let xs = parse(src).expect("parse");
    assert_eq!(xs.len(), 1);
    build_module(&xs[0]).expect("build")
}

const ADD: &str = r#"
(module m
  (fn add (params (a i64) (b i64)) (ret i64) (body (call i64.add (ref a) (ref b)))))"#;

#[test]
fn parses_and_runs_arithmetic() {
    let m = module(ADD);
    assert!(check_module(&m).is_accepted());
    let mut interp = Interp::new(&m, 0);
    assert_eq!(interp.call_fn("add", vec![i64v(3), i64v(4)]).unwrap(), i64v(7));
}

#[test]
fn merkle_ids_are_stable_and_content_addressed() {
    // The same source yields the same node ids on every parse (content-
    // addressed). Two identical sub-forms share an id.
    let m1 = module(ADD);
    let m2 = module(ADD);
    for (a, b) in m1.items.iter().zip(m2.items.iter()) {
        match (a, b) {
            (aury::ast::ModuleItem::Fn(f1), aury::ast::ModuleItem::Fn(f2)) => {
                assert_eq!(f1.id, f2.id, "fn ids must be stable across parses");
            }
            _ => {}
        }
    }
}

#[test]
fn type_mismatch_emits_ranked_admissible_repairs() {
    // `add` is given a string literal where i64 is expected.
    let src = r#"
(module m
  (fn add (params (a i64) (b i64)) (ret i64)
    (body (call i64.add (ref a) (lit "oops")))))"#;
    let m = module(src);
    let out = check_module(&m);
    let rejs = match out {
        ValidationOutcome::Rejected(r) => r,
        _ => panic!("expected rejection"),
    };
    assert_eq!(rejs.len(), 1);
    let r = &rejs[0];
    assert_eq!(r.gate, Gate::Type);
    assert_eq!(r.kind, "ARG_TYPE_MISMATCH");
    assert!(r.expected.contains("i64"));
    assert!(r.received.contains("str"));
    // Repairs are ranked by cost, ascending.
    let costs: Vec<u32> = r.repairs.iter().map(|x| x.cost).collect();
    let mut sorted = costs.clone();
    sorted.sort();
    assert_eq!(costs, sorted, "repairs must be ranked by cost");
    assert!(!r.repairs.is_empty());
    // Every repair must be admissible by construction: a `wrap` repair's
    // conversion must return the expected type (the bug we fixed: i64.parse
    // returns result, not i64, so it must NOT be offered for str->i64).
    for rep in &r.repairs {
        if rep.action == "wrap" {
            // The only admissible wrap that returns str is i64.to_str; there is
            // no admissible wrap returning i64 from str in v0. So a wrap here
            // would be a bug.
            panic!("no admissible wrap exists for str->i64; found one: {:?}", rep);
        }
    }
}

#[test]
fn repair_loop_closes_on_type_error() {
    // The headline demo: a type error is automatically repaired by applying
    // the lowest-cost admissible repair, and the program is accepted.
    let src = r#"
(module m
  (fn add (params (a i64) (b i64)) (ret i64)
    (body (call i64.add (ref a) (lit "oops")))))"#;
    let res = aury::repair_loop(src, false, 0);
    assert!(res.accepted, "loop should accept: {:?}", res.log);
    assert!(res.patches_applied >= 1);
    // The accepted source must now validate.
    let m = module(&res.source);
    assert!(check_module(&m).is_accepted());
}

#[test]
fn effect_checker_rejects_pure_fn_calling_effectful_op() {
    let src = r#"
(module m
  (fn leak (params (a i64)) (ret i64)
    (body (call i64.add (ref a) (call rng.next)))))"#;
    let m = module(src);
    let rejs = match check_module(&m) {
        ValidationOutcome::Rejected(r) => r,
        _ => panic!("expected effect rejection"),
    };
    assert_eq!(rejs.len(), 1);
    assert_eq!(rejs[0].gate, Gate::Effect);
    assert_eq!(rejs[0].kind, "EFFECT_EXCEEDS_DECLARED");
    // The repair menu proposes widening the effect row, and — unlike v0.1 — it
    // carries a `with` clause so the driver can apply it mechanically.
    let widen = rejs[0]
        .repairs
        .iter()
        .find(|r| r.action == "widen_effect_row")
        .expect("widen_effect_row repair");
    assert!(widen.with.is_some(), "widen repair must carry a `with` clause");
}

#[test]
fn repair_loop_mechanically_widens_effect_row() {
    // Track A anchor: a function that uses an effectful op without declaring the
    // capability is now *mechanically* repaired by the loop (v0.1 could only
    // diagnose this — the effect repair carried no `with` and never converged).
    let src = r#"
(module m
  (fn roll (params) (ret i64)
    (body (call rng.next))))"#;
    let res = aury::repair_loop(src, false, 0);
    assert!(res.accepted, "loop should converge on effect leak: {:?}", res.log);
    assert!(res.patches_applied >= 1);
    // The repaired source now declares the rng capability and validates.
    assert!(res.source.contains("effects"), "repaired source: {}", res.source);
    let m = module(&res.source);
    assert!(check_module(&m).is_accepted());
}

#[test]
fn repair_loop_narrows_over_declared_effect_row() {
    // Least-privilege: a pure body that declares `rng` is over-declared; the
    // loop narrows the row — here, all the way to pure (clause removed).
    let src = r#"
(module m
  (fn f (params (a i64)) (ret i64) (effects rng)
    (body (call i64.add (ref a) (lit 1)))))"#;
    let res = aury::repair_loop(src, false, 0);
    assert!(res.accepted, "loop should narrow and accept: {:?}", res.log);
    assert!(res.patches_applied >= 1);
    // The unused rng effect is gone; the fn is now pure (no effects clause).
    assert!(!res.source.contains("effects"), "repaired source: {}", res.source);
    let m = module(&res.source);
    assert!(check_module(&m).is_accepted());
}

#[test]
fn repair_loop_drops_partially_unused_effect() {
    // Declares two caps but only uses one; the row narrows to just the used cap.
    let src = r#"
(module m
  (fn f (params) (ret i64) (effects rng clock)
    (body (call rng.next))))"#;
    let res = aury::repair_loop(src, false, 0);
    assert!(res.accepted, "loop should narrow and accept: {:?}", res.log);
    assert!(res.source.contains("rng"), "should keep rng: {}", res.source);
    assert!(!res.source.contains("clock"), "should drop clock: {}", res.source);
    let m = module(&res.source);
    assert!(check_module(&m).is_accepted());
}

#[test]
fn effect_checker_rejects_unknown_capability() {
    // A capability outside the vocabulary is rejected with a drop repair; the
    // loop removes it.
    let src = r#"
(module m
  (fn f (params (a i64)) (ret i64) (effects telepathy)
    (body (ref a))))"#;
    let m = module(src);
    match check_module(&m) {
        ValidationOutcome::Rejected(rejs) => {
            assert!(rejs.iter().any(|r| r.kind == "UNKNOWN_CAPABILITY"));
        }
        _ => panic!("expected UNKNOWN_CAPABILITY rejection"),
    }
    let res = aury::repair_loop(src, false, 0);
    assert!(res.accepted, "loop should drop unknown cap and accept: {:?}", res.log);
    assert!(!res.source.contains("telepathy"), "repaired: {}", res.source);
}

#[test]
fn repair_loop_widens_existing_effect_row_in_place() {
    // A fn that already declares one capability but is missing another has its
    // existing (effects ...) clause widened, not duplicated.
    let src = r#"
(module m
  (fn f (params (a i64)) (ret i64) (effects (fs read))
    (body (call i64.add (ref a) (call rng.next)))))"#;
    let res = aury::repair_loop(src, false, 0);
    assert!(res.accepted, "loop should converge: {:?}", res.log);
    // Exactly one effects clause survives (widened, not duplicated).
    assert_eq!(res.source.matches("(effects").count(), 1, "source: {}", res.source);
    let m = module(&res.source);
    assert!(check_module(&m).is_accepted());
}

// ---- Track C1b: region arena ----

#[test]
fn arena_frees_region_allocations() {
    // Compile the standalone accounting harness against the runtime and run it;
    // exit code 0 means the arena freed every region-scoped allocation and the
    // live count returned to baseline (nested regions included).
    if std::process::Command::new("clang").arg("--version").output().is_err() {
        return; // hermetic: skip when clang is unavailable
    }
    let manifest = env!("CARGO_MANIFEST_DIR");
    let runtime = format!("{}/runtime/aury_rt.c", manifest);
    let harness = format!("{}/tests/arena_accounting.c", manifest);
    let exe = std::env::temp_dir().join("aury_arena_accounting.exe");
    let build = std::process::Command::new("clang")
        .args(["-O2", &harness, &runtime, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "clang failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = std::process::Command::new(&exe).output().unwrap();
    assert!(
        run.status.success(),
        "arena accounting failed at assertion #{:?}",
        run.status.code()
    );
}

// ---- Track C: region aliasing ----

#[test]
fn alias_conflict_two_mut_refs_in_one_region() {
    let src = r#"
(module m
  (fn f (params (a (ref r mut i64)) (b (ref r mut i64))) (ret i64)
    (body (lit 0))))"#;
    let m = module(src);
    match check_module(&m) {
        ValidationOutcome::Rejected(rejs) => {
            assert!(rejs.iter().any(|r| r.kind == "ALIAS_CONFLICT" && r.gate == Gate::Region), "{:?}", rejs);
        }
        _ => panic!("expected ALIAS_CONFLICT"),
    }
}

#[test]
fn alias_mut_plus_shared_conflicts() {
    let src = r#"
(module m
  (fn f (params (a (ref r mut i64)) (b (ref r ref i64))) (ret i64)
    (body (lit 0))))"#;
    let m = module(src);
    assert!(matches!(check_module(&m), ValidationOutcome::Rejected(_)));
}

#[test]
fn distinct_regions_and_shared_borrows_are_ok() {
    // two muts in *different* regions: provably disjoint, accepted
    let a = module(r#"
(module m
  (fn f (params (a (ref r mut i64)) (b (ref s mut i64))) (ret i64)
    (body (lit 0))))"#);
    assert!(check_module(&a).is_accepted(), "{:?}", check_module(&a));
    // two shared refs in one region: shared aliasing is allowed
    let b = module(r#"
(module m
  (fn f (params (a (ref r ref i64)) (b (ref r ref i64))) (ret i64)
    (body (lit 0))))"#);
    assert!(check_module(&b).is_accepted(), "{:?}", check_module(&b));
}

#[test]
fn repair_loop_splits_conflicting_region() {
    // The loop mechanically renames one reference's region to a fresh disjoint
    // one, resolving the aliasing conflict.
    let src = r#"
(module m
  (fn f (params (a (ref r mut i64)) (b (ref r mut i64))) (ret i64)
    (body (lit 0))))"#;
    let res = aury::repair_loop(src, false, 0);
    assert!(res.accepted, "loop should split the region: {:?}", res.log);
    assert!(res.source.contains("r_s1"), "repaired source: {}", res.source);
    let m = module(&res.source);
    assert!(check_module(&m).is_accepted());
}

// ---- Track B: growable aggregates + affine move-tracking ----

const VEC_BUILD: &str = r#"
(module m
  (fn build (params (n i64)) (ret (vec i64))
    (body
      (let acc (vec i64) (vec-new (vec i64))
        (let i i64 0
          (loop
            (if (call i64.ge (ref i) (ref n))
                (break (ref acc))
                (block
                  (set acc (vec-push (ref acc) (ref i)))
                  (set i (call i64.add (ref i) (lit 1)))
                  unit)))))))
  (fn build-len (params (n i64)) (ret i64)
    (body (len (build (ref n)))))
  (fn build-at (params (n i64) (k i64)) (ret i64)
    (body (idx (build (ref n)) (ref k)))))"#;

#[test]
fn vec_push_builds_vec_in_a_loop() {
    // The accumulator loop pattern `(set acc (vec-push acc i))` type-checks
    // (push moves the vec, set revives it) and computes correctly in interp.
    let m = module(VEC_BUILD);
    assert!(check_module(&m).is_accepted(), "{:?}", check_module(&m));
    let mut interp = Interp::new(&m, 0);
    // build(4) = [0,1,2,3]
    assert_eq!(interp.call_fn("build-len", vec![i64v(4)]).unwrap(), i64v(4));
    assert_eq!(interp.call_fn("build-at", vec![i64v(4), i64v(2)]).unwrap(), i64v(2));
    assert_eq!(interp.call_fn("build-len", vec![i64v(0)]).unwrap(), i64v(0));
}

#[test]
fn vec_push_move_tracking_rejects_use_after_move() {
    // Pushing `acc` moves it; using `acc` again is USE_AFTER_MOVE.
    let src = r#"
(module m
  (fn dup (params) (ret (vec i64))
    (body
      (let acc (vec i64) (vec-new (vec i64) (lit 1))
        (let a (vec i64) (vec-push (ref acc) (lit 2))
          (vec-push (ref acc) (lit 3)))))))"#;
    let m = module(src);
    match check_module(&m) {
        ValidationOutcome::Rejected(rejs) => {
            assert!(rejs.iter().any(|r| r.kind == "USE_AFTER_MOVE"), "{:?}", rejs);
        }
        _ => panic!("expected USE_AFTER_MOVE"),
    }
}

#[test]
fn repair_loop_inserts_copy_for_use_after_move() {
    // The loop mechanically applies the `insert_copy` repair: the second use of
    // the moved vec becomes `(copy acc)`, which revives a fresh value.
    let src = r#"
(module m
  (fn dup (params) (ret (vec i64))
    (body
      (let acc (vec i64) (vec-new (vec i64) (lit 1))
        (let a (vec i64) (vec-push (ref acc) (lit 2))
          (vec-push (ref acc) (lit 3)))))))"#;
    let res = aury::repair_loop(src, false, 0);
    assert!(res.accepted, "loop should insert copy and accept: {:?}", res.log);
    assert!(res.source.contains("copy"), "repaired source: {}", res.source);
    let m = module(&res.source);
    assert!(check_module(&m).is_accepted());
}

#[test]
fn property_test_drives_vec_push_over_generated_vecs() {
    // The intent gate generates `(vec i64)` inputs (existing generator) and runs
    // them through a vec-push map: doubling then summing equals twice the sum.
    // Confirms growable vecs are exercised by property testing with no new
    // generator/shrinker work.
    let src = r#"
(module m
  (spec
    (property double-sum-is-twice
      (forall ((xs (vec i64)))
        (call i64.eq
          (call vp-reduce-sum (vp-map-double (ref xs)))
          (call i64.mul (call vp-reduce-sum (ref xs)) (lit 2))))))
  (fn vp-map-double (params (xs (vec i64))) (ret (vec i64))
    (body
      (let out (vec i64) (vec-new (vec i64))
        (let i i64 0
          (loop
            (if (call i64.ge (ref i) (len (ref xs)))
                (break (ref out))
                (block
                  (set out (vec-push (ref out) (call i64.mul (idx (ref xs) (ref i)) (lit 2))))
                  (set i (call i64.add (ref i) (lit 1)))
                  unit)))))))
  (fn vp-reduce-sum (params (xs (vec i64))) (ret i64)
    (body
      (let acc i64 0
        (let i i64 0
          (loop
            (if (call i64.ge (ref i) (len (ref xs)))
                (break (ref acc))
                (block
                  (set acc (call i64.add (ref acc) (idx (ref xs) (ref i))))
                  (set i (call i64.add (ref i) (lit 1)))
                  unit))))))))"#;
    let m = module(src);
    assert!(check_module(&m).is_accepted(), "{:?}", check_module(&m));
    let failures = aury::spec::run_property_tests(&m, 12345, 128);
    assert!(
        failures.is_empty(),
        "property should hold; got {} counterexample(s)",
        failures.len()
    );
}

#[test]
fn property_test_catches_bug_and_shrinks() {
    // bad-max returns the smaller; the property max >= a is falsified and
    // shrunk to a minimal counterexample.
    let src = r#"
(module m
  (spec
    (property max-at-least-a
      (forall ((a i64) (b i64))
        (call i64.ge (call bad-max (ref a) (ref b)) (ref a)))))
  (fn bad-max (params (a i64) (b i64)) (ret i64)
    (body (if (call i64.gt (ref a) (ref b)) (then (ref b)) (else (ref a))))))"#;
    let m = module(src);
    assert!(check_module(&m).is_accepted(), "structurally valid");
    let failures = aury::spec::run_property_tests(&m, 12345, 128);
    assert_eq!(failures.len(), 1, "the property should be falsified");
    let f = &failures[0];
    assert!(!f.vacuous);
    assert!(!f.counterexample.is_empty(), "must produce a counterexample");
}

#[test]
fn vacuity_check_flags_property_that_does_not_exercise_impl() {
    // A property whose body is a tautology that never calls any function:
    // vacuous.
    let src = r#"
(module m
  (spec
    (property tautology
      (forall ((a i64))
        (call i64.ge (ref a) (lit 0)))))
  (fn f (params (a i64)) (ret i64) (body (ref a))))"#;
    let m = module(src);
    let failures = aury::spec::run_property_tests(&m, 1, 64);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].vacuous, "tautological property must be flagged vacuous");
}

#[test]
fn correct_impl_is_not_flagged_vacuous() {
    // add-commutes never fails (because add IS commutative) but it DOES
    // exercise `add`, so it must NOT be flagged vacuous.
    let src = r#"
(module m
  (spec
    (property add-commutes
      (forall ((a i64) (b i64))
        (call i64.eq (call add (ref a) (ref b)) (call add (ref b) (ref a))))))
  (fn add (params (a i64) (b i64)) (ret i64) (body (call i64.add (ref a) (ref b)))))"#;
    let m = module(src);
    let failures = aury::spec::run_property_tests(&m, 12345, 128);
    assert!(failures.is_empty(), "correct + exercised property should pass");
}

#[test]
fn structs_typecheck_and_run() {
    let src = r#"
(module m
  (struct Point (x i64) (y i64))
  (fn make (params (x i64) (y i64)) (ret (struct Point))
    (body (new-struct Point (x (ref x)) (y (ref y)))))
  (fn sum (params (p (struct Point))) (ret i64)
    (body (call i64.add (get (ref p) x) (get (ref p) y)))))"#;
    let m = module(src);
    assert!(check_module(&m).is_accepted());
    let mut interp = Interp::new(&m, 0);
    let p = interp.call_fn("make", vec![i64v(3), i64v(4)]).unwrap();
    assert_eq!(interp.call_fn("sum", vec![p]).unwrap(), i64v(7));
}

fn i64v(n: i64) -> aury::interp::Value {
    aury::interp::Value::I64(n)
}

// ============================================================
// JSON authoring surface (the AI generation interface)
// ============================================================

#[test]
fn json_ingest_produces_valid_canonical_program() {
    // The typed-object JSON form is what a model emits via a tool-call.
    let json = std::fs::read_to_string("tests/fixtures/gcd.json").expect("read gcd.json");
    let module = aury::json::build_module_from_json(&json).expect("ingest");
    assert!(check_module(&module).is_accepted());
    let mut interp = Interp::new(&module, 0);
    assert_eq!(interp.call_fn("gcd", vec![i64v(48), i64v(36)]).unwrap(), i64v(12));
}

#[test]
fn json_and_sexpr_paths_produce_identical_ir() {
    // The headline proof: authoring as JSON vs authoring as s-expressions must
    // yield byte-identical IR — the same Merkle node ids — so the JSON path is
    // a true authoring surface for the canonical form, not a parallel format.
    let json = std::fs::read_to_string("tests/fixtures/gcd.json").unwrap();
    let json_sexpr = aury::json::parse_json_sexpr(&json).unwrap();
    let json_module = build_module(&json_sexpr).unwrap();
    let aury_src = format!("{:?}", json_sexpr);
    let aury_sexpr = parse(&aury_src).unwrap();
    // The s-expr trees are equal (JSON -> Sexpr -> text -> Sexpr round-trips).
    assert_eq!(json_sexpr, aury_sexpr[0]);
    // And the typed ASTs (with Merkle ids) are equal.
    let aury_module = build_module(&aury_sexpr[0]).unwrap();
    assert_eq!(json_module.id, aury_module.id);
    for (a, b) in json_module.items.iter().zip(aury_module.items.iter()) {
        match (a, b) {
            (aury::ast::ModuleItem::Fn(fa), aury::ast::ModuleItem::Fn(fb)) => {
                assert_eq!(fa.id, fb.id, "fn {} id differs", fa.name);
            }
            (aury::ast::ModuleItem::Spec(sa), aury::ast::ModuleItem::Spec(sb)) => {
                assert_eq!(sa.id, sb.id);
            }
            _ => {}
        }
    }
}

#[test]
fn emit_json_then_ingest_round_trips() {
    // emit-json (.aury -> array-form JSON) then ingest (JSON -> .aury) must
    // produce a program that validates and runs identically to the original.
    let src = std::fs::read_to_string("tests/fixtures/calculator.aury").unwrap();
    let xs = parse(&src).unwrap();
    let json = aury::json::sexpr_to_json(&xs[0]);
    let json_text = serde_json::to_string(&json).unwrap();
    let back = aury::json::parse_json_sexpr(&json_text).unwrap();
    // Lossless at the s-expr level.
    assert_eq!(xs[0], back);
    let m = build_module(&back).unwrap();
    assert!(check_module(&m).is_accepted());
    let mut interp = Interp::new(&m, 0);
    assert_eq!(interp.call_fn("add", vec![i64v(3), i64v(4)]).unwrap(), i64v(7));
}

#[test]
fn parse_gate_repair_closes_unterminated_lists() {
    // The class of error that ate the hand-authoring thread: forgot to close
    // nested forms. The repair loop must bring parse errors inside the loop by
    // appending the missing closing parens — an admissible mechanical repair.
    let unterm = "(module m (fn add (params (a i64) (b i64)) (ret i64) (body (call i64.add (ref a) (ref b)";
    let res = aury::repair_loop(unterm, false, 0);
    assert!(res.accepted, "parse repair should close the loop: {:?}", res.log);
    assert!(res.log.iter().any(|l| l.contains("parse repair")));
    assert!(res.patches_applied >= 1);
}

// ============================================================
// Native lowering (Aury -> LLVM IR -> native executable)
// ============================================================

#[test]
fn native_lowering_matches_interpreter_for_numeric_core() {
    // The correctness contract for the lowering: a program compiled to native
    // code via clang must produce the SAME result as the interpreter. This is
    // the strong equivalence check — it catches any lowering bug (SSA, control
    // flow, divergence, builtin mapping) by running both and comparing.
    let src = std::fs::read_to_string("tests/fixtures/math.aury").unwrap();
    let xs = parse(&src).unwrap();
    let m = build_module(&xs[0]).unwrap();
    // lower the reachable set from gcd (i64/bool core only)
    let args = vec!["1071".to_string(), "462".to_string()];
    let ir = aury::lower::lower_program_with_main(&m, "gcd", &args).unwrap();
    assert!(ir.contains("define i64 @aury__gcd"));
    assert!(ir.contains("define i32 @main"));
    // vec/struct fns in math must NOT appear (they're outside the reachable set)
    assert!(!ir.contains("@aury__poly-eval"), "unreachable vec fn should not be lowered");
    // bring your own clang: this test verifies the IR is well-formed by the
    // equivalence it implies; if clang is absent we still assert the IR shape.
    if std::process::Command::new("clang").arg("--version").output().is_ok() {
        let ll = std::env::temp_dir().join("aury_test_gcd.ll");
        std::fs::write(&ll, &ir).unwrap();
        let exe = std::env::temp_dir().join("aury_test_gcd.exe");
        let runtime = format!("{}/runtime/aury_rt.c", env!("CARGO_MANIFEST_DIR"));
        let st = std::process::Command::new("clang")
            .args(["-O2", ll.to_str().unwrap(), &runtime, "-o", exe.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(st.status.success(), "clang failed:\n{}", String::from_utf8_lossy(&st.stderr));
        let out = std::process::Command::new(&exe).output().unwrap();
        let native = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // gcd(1071,462) = 21
        assert_eq!(native, "21", "native output must equal interpreter (21)");
    }
}

#[test]
fn native_lowering_supports_str_result_and_typed_control_flow() {
    // Authored through Aury's typed-object JSON surface. This exercises boxed
    // string params/results, result.is_ok, bool params, string-valued if, and
    // a string match whose bind must retain the scrutinee type.
    let json = r#"{
      "kind":"module",
      "name":"native_str",
      "items":[
        {"kind":"fn","name":"describe","params":[{"name":"n","type":"i64"}],"ret":"str",
         "body":{"kind":"if",
           "cond":{"kind":"call","op":"i64.gt","args":[{"kind":"ref","name":"n"},{"kind":"lit","value":0}]},
           "then":{"kind":"call","op":"str.concat","args":[{"kind":"lit","value":"pos:"},{"kind":"call","op":"i64.to_str","args":[{"kind":"ref","name":"n"}]}]},
           "else":{"kind":"lit","value":"nonpos"}}},
        {"kind":"fn","name":"is-int","params":[{"name":"s","type":"str"}],"ret":"bool",
         "body":{"kind":"call","op":"result.is_ok","args":[{"kind":"call","op":"i64.parse","args":[{"kind":"ref","name":"s"}]}]}},
        {"kind":"fn","name":"invert","params":[{"name":"b","type":"bool"}],"ret":"bool",
         "body":{"kind":"call","op":"bool.not","args":[{"kind":"ref","name":"b"}]}},
        {"kind":"fn","name":"early","params":[{"name":"n","type":"i64"}],"ret":"str",
         "body":{"kind":"if",
           "cond":{"kind":"call","op":"i64.gt","args":[{"kind":"ref","name":"n"},{"kind":"lit","value":0}]},
           "then":{"kind":"return","value":{"kind":"lit","value":"early"}},
           "else":{"kind":"lit","value":"late"}}},
        {"kind":"fn","name":"echo-match","params":[{"name":"s","type":"str"}],"ret":"str",
         "body":{"kind":"match","scrut":{"kind":"ref","name":"s"},"arms":[
           {"pattern":{"kind":"lit","value":"yes"},"body":{"kind":"lit","value":"Y"}},
           {"pattern":{"kind":"bind","name":"x"},"body":{"kind":"call","op":"str.concat","args":[{"kind":"ref","name":"x"},{"kind":"lit","value":"!"}]}}
         ]}}
      ]
    }"#;
    let m = aury::json::build_module_from_json(json).expect("build JSON module");
    assert!(check_module(&m).is_accepted());

    let cases: Vec<(&str, Vec<String>, String)> = vec![
        ("describe", vec!["5".into()], "\"pos:5\"\n".into()),
        ("describe", vec!["-3".into()], "\"nonpos\"\n".into()),
        ("is-int", vec!["12x".into()], "false\n".into()),
        ("is-int", vec![" 42 ".into()], "true\n".into()),
        ("is-int", vec!["\u{2003}42\u{2003}".into()], "true\n".into()),
        ("is-int", vec!["9223372036854775808".into()], "false\n".into()),
        ("invert", vec!["true".into()], "false\n".into()),
        ("early", vec!["1".into()], "\"early\"\n".into()),
        ("early", vec!["0".into()], "\"late\"\n".into()),
        ("echo-match", vec!["yes".into()], "\"Y\"\n".into()),
        (
            "echo-match",
            vec!["a\"\\\n".into()],
            format!("{:?}\n", "a\"\\\n!"),
        ),
        ("echo-match", vec!["--help".into()], "\"--help!\"\n".into()),
        (
            "echo-match",
            vec!["\u{85}".into()],
            format!("{:?}\n", "\u{85}!"),
        ),
    ];

    for (index, (entry, args, expected)) in cases.into_iter().enumerate() {
        let ir = aury::lower::lower_program_with_main(&m, entry, &args).unwrap();
        assert!(ir.contains("private constant"));
        if std::process::Command::new("clang").arg("--version").output().is_err() {
            continue;
        }
        let ll = std::env::temp_dir().join(format!("aury_native_str_{}.ll", index));
        let exe = std::env::temp_dir().join(format!("aury_native_str_{}.exe", index));
        std::fs::write(&ll, &ir).unwrap();
        let runtime = format!("{}/runtime/aury_rt.c", env!("CARGO_MANIFEST_DIR"));
        let clang = std::process::Command::new("clang")
            .args(["-O2", ll.to_str().unwrap(), &runtime, "-o", exe.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            clang.status.success(),
            "clang failed for {}:\n{}\n{}",
            entry,
            String::from_utf8_lossy(&clang.stderr),
            ir
        );
        let output = std::process::Command::new(&exe).output().unwrap();
        assert!(output.status.success(), "native {} failed", entry);
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected, "{}", entry);
    }
}

#[test]
fn native_aggregate_rng_and_edge_parity_matrix() {
    let src = std::fs::read_to_string("tests/native_parity.aury").unwrap();
    let m = module(&src);
    assert!(check_module(&m).is_accepted());
    if std::process::Command::new("clang").arg("--version").output().is_err() {
        return;
    }
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("vec-id", vec!["[1,2,-3]".into()]),
        ("vec-at", vec![r#"["a","b"]"#.into(), "1".into()]),
        ("vec-size", vec!["[[1,2],[]]".into()]),
        ("vec-sum-from", vec!["[1,2,3,4]".into(), "0".into()]),
        ("empty", vec![]),
        ("packet-id", vec![r#"{"name":"x","values":[1,-2],"nested":[[true,false],[]]}"#.into()]),
        ("packet-name", vec![r#"{"name":"x","values":[1,-2],"nested":[[true,false],[]]}"#.into()]),
        ("branch-vector", vec!["true".into()]),
        ("branch-vector", vec!["false".into()]),
        ("branch-packet", vec!["true".into()]),
        ("branch-packet", vec!["false".into()]),
        ("made", vec![]),
        ("copied", vec![]),
        ("random-pair", vec![]),
        ("result-id", vec![r#"{"ok":[5,6]}"#.into()]),
        ("result-id", vec![r#"{"err":{"name":"bad","values":[],"nested":[]}}"#.into()]),
        ("unit-value", vec![]),
        ("parse-value", vec!["42".into()]),
        ("parse-value", vec!["nope".into()]),
        ("edge-div", vec![]),
        ("edge-mod", vec![]),
        ("edge-neg", vec![]),
        ("edge-abs", vec![]),
        ("nested-return", vec![]),
        // Track 2: mutable loops must match the interpreter across backends.
        ("loop-fact", vec!["5".into()]),
        ("loop-fact", vec!["10".into()]),
        ("loop-fact", vec!["0".into()]),
        ("loop-sum", vec!["10".into()]),
        ("loop-sum", vec!["0".into()]),
        ("loop-table", vec!["3".into()]),
        ("loop-table", vec!["5".into()]),
        ("loop-empty", vec!["7".into()]),
        // Track 3: f64 arithmetic, edges, casts, and float-carrying aggregates
        // must be byte-identical across interp and native (format included).
        ("f-poly", vec!["3.14".into()]),
        ("f-poly", vec!["-2.5".into()]),
        ("f-poly", vec!["0.0".into()]),
        ("f-abs", vec!["-7.5".into()]),
        ("f-inf", vec![]),
        ("f-ninf", vec![]),
        ("f-nan", vec![]),
        ("f-nan-self-eq", vec![]),
        ("f-cmp", vec!["1.5".into(), "2.5".into()]),
        ("f-cmp", vec!["2.5".into(), "1.5".into()]),
        ("f-to-str", vec!["3.14".into()]),
        ("f-to-str", vec!["0.1".into()]),
        ("f-to-str", vec!["-0.0".into()]),
        ("f-of-i", vec!["42".into()]),
        ("i-of-f", vec!["3.9".into()]),
        ("i-of-f", vec!["-3.9".into()]),
        ("i-of-f", vec!["1e30".into()]),
        ("f-mean", vec!["[1.0,2.0,3.0,4.0]".into()]),
        ("f-mean", vec!["[0.5,-0.5]".into()]),
        ("f-point-x", vec![r#"{"x":1.25,"y":-9.5}"#.into()]),
        ("f-make-point", vec!["1.25".into(), "-9.5".into()]),
        // Track B: growable vecs (vec-push) — build/map/filter/reduce over i64
        // and f64 must be byte-identical across interp and native.
        ("vp-build", vec!["5".into()]),
        ("vp-build", vec!["0".into()]),
        ("vp-map-double", vec!["[1,2,3,-4]".into()]),
        ("vp-filter-even", vec!["[1,2,3,4,5,6]".into()]),
        ("vp-filter-even", vec!["[1,3,5]".into()]),
        ("vp-reduce-sum", vec!["[10,20,30]".into()]),
        ("vp-build-sum", vec!["6".into()]),
        ("vp-fscale", vec!["[1.0,2.0,-0.5]".into()]),
        ("vp-fscale", vec!["[]".into()]),
        ("vp-copy-branch", vec!["3".into()]),
        // Track C1b: an arena-managed region (frees scratch) is observably equal.
        ("region-scalar", vec!["5".into()]),
        ("region-scalar", vec!["0".into()]),
    ];
    let runtime = format!("{}/runtime/aury_rt.c", env!("CARGO_MANIFEST_DIR"));
    for (index, (entry, args)) in cases.into_iter().enumerate() {
        let function = m.items.iter().find_map(|item| match item {
            aury::ast::ModuleItem::Fn(function) if function.name == entry => Some(function),
            _ => None,
        }).unwrap();
        let values = function.params.iter().zip(&args)
            .map(|(parameter, text)| aury::value_io::parse_cli_value(&m, &parameter.ty, text).unwrap())
            .collect();
        let mut interp = Interp::new(&m, 0xC0FFEE);
        let expected = format!("{}\n", aury::value_io::show_value(&interp.call_fn(entry, values).unwrap()));
        let ir = aury::lower::lower_program_with_main(&m, entry, &args).unwrap();
        let ll = std::env::temp_dir().join(format!("aury_native_parity_{}.ll", index));
        let exe = std::env::temp_dir().join(format!("aury_native_parity_{}.exe", index));
        std::fs::write(&ll, ir).unwrap();
        let clang = std::process::Command::new("clang")
            .args(["-O2", ll.to_str().unwrap(), &runtime, "-o", exe.to_str().unwrap()])
            .output().unwrap();
        assert!(clang.status.success(), "clang failed for {}:\n{}", entry, String::from_utf8_lossy(&clang.stderr));
        let output = std::process::Command::new(&exe).output().unwrap();
        assert!(output.status.success(), "native {} failed", entry);
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected, "{}", entry);
    }
}

#[test]
fn native_vector_bounds_trap_matches_interpreter_error() {
    let src = std::fs::read_to_string("tests/native_parity.aury").unwrap();
    let m = module(&src);
    if std::process::Command::new("clang").arg("--version").output().is_err() {
        return;
    }
    let runtime = format!("{}/runtime/aury_rt.c", env!("CARGO_MANIFEST_DIR"));
    for (index, args) in [
        vec![r#"["only"]"#.to_string(), "-1".to_string()],
        vec![r#"["only"]"#.to_string(), "1".to_string()],
    ].into_iter().enumerate() {
        let values = vec![
            aury::value_io::parse_cli_value(&m, &aury::types::Type::Vec(Box::new(aury::types::Type::Str)), &args[0]).unwrap(),
            aury::interp::Value::I64(args[1].parse().unwrap()),
        ];
        assert!(Interp::new(&m, 0xC0FFEE).call_fn("vec-at", values).is_err());
        let ir = aury::lower::lower_program_with_main(&m, "vec-at", &args).unwrap();
        let ll = std::env::temp_dir().join(format!("aury_bounds_{}.ll", index));
        let exe = std::env::temp_dir().join(format!("aury_bounds_{}.exe", index));
        std::fs::write(&ll, ir).unwrap();
        let clang = std::process::Command::new("clang")
            .args(["-O2", ll.to_str().unwrap(), &runtime, "-o", exe.to_str().unwrap()])
            .output().unwrap();
        assert!(clang.status.success(), "{}", String::from_utf8_lossy(&clang.stderr));
        assert!(!std::process::Command::new(exe).output().unwrap().status.success());
    }
}

#[test]
fn cli_compile_runs_bare_output_name_with_embedded_runtime() {
    if std::process::Command::new("clang").arg("--version").output().is_err() {
        return;
    }
    let directory = std::env::temp_dir().join(format!("aury_cli_bare_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let fixture = std::fs::canonicalize("tests/native_parity.aury").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aury"))
        .current_dir(&directory)
        .args([
            "compile",
            fixture.to_str().unwrap(),
            "packet-name",
            r#"{"name":"bare","values":[1],"nested":[]}"#,
            "-o",
            "bare_native",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "\"bare\"\n");
    assert!(directory.join("bare_native").is_file());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn validator_rejects_duplicate_fields_and_accepts_i64_neq() {
    let duplicate = module(r#"
(module m
  (struct P (x i64))
  (fn f (params) (ret (struct P))
    (body (new-struct P (x (lit 1)) (x (lit 2))))))"#);
    let rejections = match check_module(&duplicate) {
        ValidationOutcome::Rejected(rejections) => rejections,
        _ => panic!("duplicate field must be rejected"),
    };
    assert!(rejections.iter().any(|rejection| rejection.kind == "DUPLICATE_FIELD"));

    let neq = module(r#"
(module m
  (fn f (params (a i64) (b i64)) (ret bool)
    (body (call i64.neq (ref a) (ref b)))))"#);
    assert!(check_module(&neq).is_accepted());

    let bad_rng = module(r#"
(module m
  (fn f (params) (ret i64) (effects (rng))
    (body (call rng.next (lit 1)))))"#);
    assert!(matches!(check_module(&bad_rng), ValidationOutcome::Rejected(_)));
    assert!(Interp::new(&bad_rng, 0xC0FFEE).call_fn("f", vec![]).is_err());
}

#[test]
fn validator_checks_function_and_explicit_return_types() {
    for src in [
        r#"(module m (fn f (params) (ret i64) (body (lit true))))"#,
        r#"(module m (fn f (params) (ret i64) (body (return (lit true)))))"#,
    ] {
        let rejected = match check_module(&module(src)) {
            ValidationOutcome::Rejected(rejections) => rejections,
            _ => panic!("return mismatch must be rejected"),
        };
        assert!(rejected.iter().any(|rejection| rejection.kind == "RETURN_TYPE_MISMATCH"));
    }
}

#[test]
fn nested_let_shadowing_restores_the_previous_binding() {
    let m = module(r#"
(module m
  (fn f (params (x i64)) (ret i64)
    (body (block
      (let x bool (lit true) (ref x))
      (ref x)))))"#);
    assert!(check_module(&m).is_accepted());
    assert_eq!(
        Interp::new(&m, 0).call_fn("f", vec![i64v(17)]).unwrap(),
        i64v(17)
    );
}

#[test]
fn duplicate_struct_and_function_names_are_rejected() {
    let duplicate_struct = module(r#"
(module m
  (struct Same (first i64))
  (struct Same (second bool))
  (fn f (params) (ret i64) (body (lit 0))))"#);
    let rejected = match check_module(&duplicate_struct) {
        ValidationOutcome::Rejected(rejections) => rejections,
        _ => panic!("duplicate struct names must be rejected"),
    };
    assert!(rejected.iter().any(|rejection| rejection.kind == "DUPLICATE_STRUCT"));

    let duplicate_function = module(r#"
(module m
  (fn same (params) (ret i64) (body (lit 1)))
  (fn same (params) (ret i64) (body (lit 2))))"#);
    let rejected = match check_module(&duplicate_function) {
        ValidationOutcome::Rejected(rejections) => rejections,
        _ => panic!("duplicate function names must be rejected"),
    };
    assert!(rejected
        .iter()
        .any(|rejection| rejection.kind == "DUPLICATE_FUNCTION"));
}

#[test]
fn native_binary_builtins_propagate_returns_from_operands() {
    if std::process::Command::new("clang").arg("--version").output().is_err() {
        return;
    }
    let m = module(r#"
(module m
  (fn arithmetic (params) (ret i64)
    (body (call i64.add (return (lit 7)) (lit 1))))
  (fn comparison (params) (ret i64)
    (body (call i64.eq (lit 1) (return (lit 8)))))
  (fn boolean (params) (ret bool)
    (body (call bool.and (return (lit true)) (lit false)))))"#);
    assert!(check_module(&m).is_accepted());
    let runtime = format!("{}/runtime/aury_rt.c", env!("CARGO_MANIFEST_DIR"));
    for (index, (entry, expected)) in [
        ("arithmetic", "7\n"),
        ("comparison", "8\n"),
        ("boolean", "true\n"),
    ]
    .into_iter()
    .enumerate()
    {
        let ir = aury::lower::lower_program_with_main(&m, entry, &[]).unwrap();
        let ll = std::env::temp_dir().join(format!("aury_return_operand_{}.ll", index));
        let exe = std::env::temp_dir().join(format!("aury_return_operand_{}.exe", index));
        std::fs::write(&ll, ir).unwrap();
        let clang = std::process::Command::new("clang")
            .args(["-O2", ll.to_str().unwrap(), &runtime, "-o", exe.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(clang.status.success(), "{}", String::from_utf8_lossy(&clang.stderr));
        let output = std::process::Command::new(exe).output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
    }
}

#[test]
fn recursive_native_entry_descriptor_is_a_compile_error() {
    let m = module(r#"
(module m
  (struct Node (next (struct Node)))
  (fn never (params) (ret (struct Node)) (body (loop unit))))"#);
    assert!(check_module(&m).is_accepted());
    let error = aury::lower::lower_program_with_main(&m, "never", &[]).unwrap_err();
    assert!(error.contains("recursive struct `Node`"), "{}", error);
}

#[test]
fn aggregate_value_io_rejects_malformed_values() {
    use aury::types::Type;

    let m = module(&std::fs::read_to_string("tests/native_parity.aury").unwrap());
    let nested_bools = Type::Vec(Box::new(Type::Vec(Box::new(Type::Bool))));
    let packet = Type::Struct("Packet".into());
    let result = Type::Result(
        Box::new(Type::Vec(Box::new(Type::I64))),
        Box::new(packet.clone()),
    );
    for (ty, text) in [
        (Type::Vec(Box::new(Type::I64)), "["),
        (nested_bools, "[[1]]"),
        (packet.clone(), r#"{"name":"x","values":[]}"#),
        (
            packet.clone(),
            r#"{"name":"x","values":[],"nested":[],"extra":0}"#,
        ),
        (result.clone(), "{}"),
        (result.clone(), r#"{"ok":[],"err":{"name":"x","values":[],"nested":[]}}"#),
        (result.clone(), r#"{"ok":[1],"ok":[2]}"#),
        (packet, r#"{"name":"first","name":"last","values":[],"nested":[]}"#),
        (result, r#"{"other":0}"#),
    ] {
        assert!(
            aury::value_io::parse_cli_value(&m, &ty, text).is_err(),
            "{:?} unexpectedly accepted {}",
            ty,
            text
        );
    }
}

// ===========================================================================
// Track 1: executable contracts (requires / ensures).
// ===========================================================================

/// Find a function definition by name in a module.
fn fn_def<'a>(m: &'a aury::ast::Module, name: &str) -> &'a aury::ast::FnDef {
    m.items
        .iter()
        .find_map(|it| match it {
            aury::ast::ModuleItem::Fn(f) if f.name == name => Some(f),
            _ => None,
        })
        .expect("function present")
}

#[test]
fn ensures_is_enforced_at_runtime_and_caught_by_intent_gate() {
    // `abs` claims (ensures result >= 0) but returns x unchanged — a bug that
    // is invisible to the type checker and only the contract catches.
    let buggy = r#"
(module m
  (fn abs (params (x i64)) (ret i64)
    (ensures (call i64.ge (ref result) (lit 0)))
    (body (ref x))))"#;
    let m = module(buggy);
    // Structurally valid: the contract gate is about intent, not types.
    assert!(check_module(&m).is_accepted(), "buggy abs still type-checks");
    assert_eq!(fn_def(&m, "abs").ensures.len(), 1, "ensures clause parsed");

    // Runtime enforcement: a negative input trips the postcondition.
    let mut interp = Interp::new(&m, 0);
    assert!(
        interp.call_fn("abs", vec![i64v(-5)]).is_err(),
        "postcondition must trap at runtime for x < 0"
    );

    // Intent gate: run_contract_tests finds and shrinks the counterexample.
    let failures = aury::spec::run_contract_tests(&m, 12345, 128);
    assert_eq!(failures.len(), 1, "postcondition must be falsified");
    assert!(!failures[0].vacuous);
    // The bug fails for every x < 0, so the minimal counterexample is exactly
    // x = -1 — shrinking must reach it and not drift to a larger magnitude.
    assert_eq!(failures[0].counterexample, vec![("x".to_string(), i64v(-1))]);
    let rej = aury::spec::contract_failure_to_rejection(&failures[0]);
    assert_eq!(rej.gate, Gate::Contract);
    assert_eq!(rej.kind, "POSTCONDITION_FALSIFIED");
}

#[test]
fn correct_impl_satisfies_its_contract() {
    let good = r#"
(module m
  (fn abs (params (x i64)) (ret i64)
    (ensures (call i64.ge (ref result) (lit 0)))
    (body (if (call i64.lt (ref x) (lit 0)) (call i64.neg (ref x)) (ref x)))))"#;
    let m = module(good);
    assert!(check_module(&m).is_accepted());
    // Runtime: contract holds for both signs.
    let mut interp = Interp::new(&m, 0);
    assert_eq!(interp.call_fn("abs", vec![i64v(-5)]).unwrap(), i64v(5));
    assert_eq!(interp.call_fn("abs", vec![i64v(7)]).unwrap(), i64v(7));
    // Intent gate: no failures.
    assert!(aury::spec::run_contract_tests(&m, 12345, 128).is_empty());
}

#[test]
fn requires_filters_out_of_domain_inputs() {
    // safe_div traps on b == 0, but its precondition excludes that case. The
    // contract tester must SKIP out-of-domain inputs, so the function passes.
    let src = r#"
(module m
  (fn safe_div (params (a i64) (b i64)) (ret i64)
    (requires (call i64.neq (ref b) (lit 0)))
    (ensures (call bool.or (call i64.ge (ref result) (lit 0)) (call i64.lt (ref result) (lit 0))))
    (body (call i64.div (ref a) (ref b)))))"#;
    let m = module(src);
    assert!(check_module(&m).is_accepted());
    assert_eq!(fn_def(&m, "safe_div").requires.len(), 1);

    // Passes despite div-by-zero being reachable: b == 0 is out of domain.
    assert!(
        aury::spec::run_contract_tests(&m, 999, 256).is_empty(),
        "precondition must exclude b == 0 from testing"
    );

    // Runtime: a precondition violation traps; an in-domain call succeeds.
    let mut interp = Interp::new(&m, 0);
    assert!(interp.call_fn("safe_div", vec![i64v(4), i64v(0)]).is_err());
    assert_eq!(interp.call_fn("safe_div", vec![i64v(6), i64v(2)]).unwrap(), i64v(3));
}

#[test]
fn ensures_must_be_a_boolean_predicate() {
    // An `ensures` clause of type i64 is a Contract-gate rejection with an
    // admissible replace_node repair.
    let src = r#"
(module m
  (fn f (params (x i64)) (ret i64)
    (ensures (ref x))
    (body (ref x))))"#;
    let m = module(src);
    let rejs = match check_module(&m) {
        ValidationOutcome::Rejected(r) => r,
        _ => panic!("expected a contract rejection"),
    };
    assert_eq!(rejs.len(), 1);
    assert_eq!(rejs[0].gate, Gate::Contract);
    assert_eq!(rejs[0].kind, "CONTRACT_NOT_BOOL");
    assert!(rejs[0].repairs.iter().any(|r| r.action == "replace_node"));
}

#[test]
fn requires_using_result_is_an_unbound_reference() {
    // `result` is only in scope for `ensures`. Using it in `requires` must be
    // an ordinary unbound-reference rejection (scope enforced for free).
    let src = r#"
(module m
  (fn f (params (x i64)) (ret i64)
    (requires (call i64.ge (ref result) (lit 0)))
    (body (ref x))))"#;
    let m = module(src);
    let rejs = match check_module(&m) {
        ValidationOutcome::Rejected(r) => r,
        _ => panic!("expected an unbound-ref rejection"),
    };
    assert!(rejs.iter().any(|r| r.kind == "UNBOUND_REF" && r.path == "result"));
}

#[test]
fn postcondition_that_ignores_result_is_vacuous() {
    // An `ensures` that never mentions `result` cannot constrain the output.
    let src = r#"
(module m
  (fn id (params (x i64)) (ret i64)
    (ensures (call i64.ge (ref x) (lit -1000)))
    (body (ref x))))"#;
    let m = module(src);
    assert!(check_module(&m).is_accepted());
    let failures = aury::spec::run_contract_tests(&m, 1, 64);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].vacuous, "ensures ignoring result must be flagged vacuous");
    assert_eq!(
        aury::spec::contract_failure_to_rejection(&failures[0]).kind,
        "VACUOUS_CONTRACT"
    );
}

#[test]
fn contracts_round_trip_through_json_authoring_surface() {
    // A model can author contracts via typed-object JSON; the result is
    // identical canonical IR and validates.
    let json = r#"{"kind":"module","name":"m","items":[
      {"kind":"fn","name":"abs","params":[{"name":"x","type":"i64"}],"ret":"i64",
       "ensures":[{"kind":"call","op":"i64.ge","args":[
         {"kind":"ref","name":"result"},{"kind":"lit","value":0}]}],
       "body":{"kind":"if",
         "cond":{"kind":"call","op":"i64.lt","args":[{"kind":"ref","name":"x"},{"kind":"lit","value":0}]},
         "then":{"kind":"call","op":"i64.neg","args":[{"kind":"ref","name":"x"}]},
         "else":{"kind":"ref","name":"x"}}}
    ]}"#;
    let m = aury::json::build_module_from_json(json).expect("ingest contracts");
    assert!(check_module(&m).is_accepted());
    assert_eq!(fn_def(&m, "abs").ensures.len(), 1);

    // Array-form round trip: sexpr -> JSON -> sexpr is lossless, so contracts
    // survive emit-json / ingest.
    let s = aury::json::parse_json_sexpr(json).unwrap();
    let back = aury::json::sexpr_to_json(&s);
    let round = aury::json::json_to_sexpr(&back).unwrap();
    assert_eq!(s, round, "contracts must survive the JSON round trip");
}

// ============================================================
// Track 2 — mutable loops / accumulators (set + loop + break)
// ============================================================

/// Load the loop fixtures from the shared native-parity module.
fn loops_module() -> aury::ast::Module {
    let src = std::fs::read_to_string("tests/native_parity.aury").unwrap();
    module(&src)
}

#[test]
fn interpreter_runs_iterative_accumulator_loops() {
    let m = loops_module();
    assert!(check_module(&m).is_accepted());
    let mut interp = Interp::new(&m, 0);
    // iterative factorial
    assert_eq!(interp.call_fn("loop-fact", vec![i64v(5)]).unwrap(), i64v(120));
    assert_eq!(interp.call_fn("loop-fact", vec![i64v(10)]).unwrap(), i64v(3628800));
    // n = 0: the loop breaks on the first test, accumulator stays at 1
    assert_eq!(interp.call_fn("loop-fact", vec![i64v(0)]).unwrap(), i64v(1));
    // countdown sum 1..n
    assert_eq!(interp.call_fn("loop-sum", vec![i64v(10)]).unwrap(), i64v(55));
    assert_eq!(interp.call_fn("loop-sum", vec![i64v(0)]).unwrap(), i64v(0));
    // nested loops over the multiplication table: (sum 1..n)^2
    assert_eq!(interp.call_fn("loop-table", vec![i64v(3)]).unwrap(), i64v(36));
    assert_eq!(interp.call_fn("loop-table", vec![i64v(5)]).unwrap(), i64v(225));
    // a loop that breaks immediately yields its seed untouched
    assert_eq!(interp.call_fn("loop-empty", vec![i64v(7)]).unwrap(), i64v(42));
}

#[test]
fn loop_with_break_is_a_value_agreeing_with_return_type() {
    // A loop whose only exit is `break acc` has type i64 and satisfies the
    // function's declared return type — no `return` needed.
    let src = r#"
(module m
  (fn count-down (params (n i64)) (ret i64)
    (body
      (let i i64 (ref n)
        (loop
          (if (call i64.le (ref i) (lit 0))
              (break (ref i))
              (set i (call i64.sub (ref i) (lit 1)))))))))"#;
    let m = module(src);
    assert!(check_module(&m).is_accepted(), "loop-as-value must type-check");
    let mut interp = Interp::new(&m, 0);
    assert_eq!(interp.call_fn("count-down", vec![i64v(4)]).unwrap(), i64v(0));
}

#[test]
fn set_of_a_parameter_is_rejected() {
    let src = r#"
(module m
  (fn f (params (n i64)) (ret i64)
    (body (block (set n (lit 5)) (ref n)))))"#;
    let rejs = match check_module(&module(src)) {
        ValidationOutcome::Rejected(r) => r,
        _ => panic!("expected rejection"),
    };
    assert_eq!(rejs[0].gate, Gate::Type);
    assert_eq!(rejs[0].kind, "SET_OF_PARAM");
}

#[test]
fn set_of_unbound_name_is_rejected() {
    let src = r#"
(module m
  (fn f (params (n i64)) (ret i64)
    (body (block (set zzz (lit 1)) (ref n)))))"#;
    let rejs = match check_module(&module(src)) {
        ValidationOutcome::Rejected(r) => r,
        _ => panic!("expected rejection"),
    };
    assert!(rejs.iter().any(|r| r.kind == "SET_UNBOUND"));
}

#[test]
fn set_type_mismatch_is_rejected_with_repairs() {
    let src = r#"
(module m
  (fn f (params (n i64)) (ret i64)
    (body (let acc i64 0 (block (set acc true) (ref acc))))))"#;
    let rejs = match check_module(&module(src)) {
        ValidationOutcome::Rejected(r) => r,
        _ => panic!("expected rejection"),
    };
    let r = rejs.iter().find(|r| r.kind == "SET_TYPE_MISMATCH").expect("SET_TYPE_MISMATCH");
    assert_eq!(r.gate, Gate::Type);
    assert!(r.expected.contains("i64"), "expected shows the binding type, got {:?}", r.expected);
    assert!(r.received.contains("bool"), "received shows the value type, got {:?}", r.received);
}

#[test]
fn break_outside_loop_is_rejected() {
    let src = r#"
(module m
  (fn f (params (n i64)) (ret i64)
    (body (break (lit 5)))))"#;
    let rejs = match check_module(&module(src)) {
        ValidationOutcome::Rejected(r) => r,
        _ => panic!("expected rejection"),
    };
    assert!(rejs.iter().any(|r| r.kind == "BREAK_OUTSIDE_LOOP"));
}

#[test]
fn break_value_types_must_agree() {
    let src = r#"
(module m
  (fn f (params (n i64)) (ret i64)
    (body (loop (if (call i64.gt (ref n) (lit 0)) (break (lit 1)) (break true))))))"#;
    let rejs = match check_module(&module(src)) {
        ValidationOutcome::Rejected(r) => r,
        _ => panic!("expected rejection"),
    };
    assert!(rejs.iter().any(|r| r.kind == "BREAK_TYPE_MISMATCH"));
}

#[test]
fn set_inside_match_arm_mutates_enclosing_binding() {
    // Regression guard: match arms bind patterns without cloning the scope, so
    // a `set` inside an arm updates the loop-carried accumulator — matching
    // native lowering, where all arms share the same slot.
    let src = r#"
(module m
  (fn f (params (n i64)) (ret i64)
    (body
      (let acc i64 0
        (let i i64 0
          (loop
            (if (call i64.ge (ref i) (ref n))
                (break (ref acc))
                (block
                  (match (call i64.mod (ref i) (lit 2))
                    (0 (set acc (call i64.add (ref acc) (lit 10))))
                    (_ (set acc (call i64.add (ref acc) (lit 1)))))
                  (set i (call i64.add (ref i) (lit 1)))
                  unit))))))))"#;
    let m = module(src);
    assert!(check_module(&m).is_accepted());
    let mut interp = Interp::new(&m, 0);
    // i = 0,1,2,3 -> 10 + 1 + 10 + 1 = 22
    assert_eq!(interp.call_fn("f", vec![i64v(4)]).unwrap(), i64v(22));
}

#[test]
fn set_and_break_survive_the_json_round_trip() {
    // The typed JSON authoring surface must emit and re-ingest set/break.
    let json = r#"{"kind":"loop","body":
      {"kind":"block","stmts":[
        {"kind":"set","name":"acc","value":{"kind":"lit","value":1}}],
       "tail":{"kind":"break","value":{"kind":"ref","name":"acc"}}}}"#;
    let s = aury::json::parse_json_sexpr(json).unwrap();
    let back = aury::json::sexpr_to_json(&s);
    let round = aury::json::json_to_sexpr(&back).unwrap();
    assert_eq!(s, round, "set/break must survive the JSON round trip");
}

// ---------------------------------------------------------------------------
// Track 3: f64 floats
// ---------------------------------------------------------------------------

use aury::interp::Value;

fn f64v(x: f64) -> Value {
    Value::F64(x)
}

const FLOATS: &str = r#"
(module m
  (fn poly (params (x f64)) (ret f64)
    (body (call f64.add (call f64.mul (ref x) (ref x)) 1.0)))
  (fn div (params (a f64) (b f64)) (ret f64) (body (call f64.div (ref a) (ref b))))
  (fn abs (params (x f64)) (ret f64) (body (call f64.abs (ref x))))
  (fn eq (params (a f64) (b f64)) (ret bool) (body (call f64.eq (ref a) (ref b))))
  (fn neq (params (a f64) (b f64)) (ret bool) (body (call f64.neq (ref a) (ref b))))
  (fn to-str (params (x f64)) (ret str) (body (call f64.to_str (ref x))))
  (fn of-i (params (n i64)) (ret f64) (body (cast f64 (ref n))))
  (fn to-i (params (x f64)) (ret i64) (body (cast i64 (ref x)))))"#;

#[test]
fn f64_arithmetic_and_ieee_edges_in_interp() {
    let m = module(FLOATS);
    assert!(check_module(&m).is_accepted());
    let mut interp = Interp::new(&m, 0);
    assert_eq!(interp.call_fn("poly", vec![f64v(3.0)]).unwrap(), f64v(10.0));
    assert_eq!(interp.call_fn("abs", vec![f64v(-7.5)]).unwrap(), f64v(7.5));
    // Division by zero is IEEE and never traps.
    assert_eq!(interp.call_fn("div", vec![f64v(1.0), f64v(0.0)]).unwrap(), f64v(f64::INFINITY));
    assert_eq!(interp.call_fn("div", vec![f64v(-1.0), f64v(0.0)]).unwrap(), f64v(f64::NEG_INFINITY));
    match interp.call_fn("div", vec![f64v(0.0), f64v(0.0)]).unwrap() {
        Value::F64(x) => assert!(x.is_nan(), "0/0 must be NaN"),
        other => panic!("expected NaN, got {:?}", other),
    }
    // NaN compares unequal to everything, including itself.
    let nan = interp.call_fn("div", vec![f64v(0.0), f64v(0.0)]).unwrap();
    let (n1, n2) = (nan.clone(), nan);
    assert_eq!(interp.call_fn("eq", vec![n1.clone(), n2.clone()]).unwrap(), Value::Bool(false));
    assert_eq!(interp.call_fn("neq", vec![n1, n2]).unwrap(), Value::Bool(true));
}

#[test]
fn f64_casts_round_trip_and_saturate() {
    let m = module(FLOATS);
    let mut interp = Interp::new(&m, 0);
    assert_eq!(interp.call_fn("of-i", vec![i64v(42)]).unwrap(), f64v(42.0));
    // Truncation toward zero.
    assert_eq!(interp.call_fn("to-i", vec![f64v(3.9)]).unwrap(), i64v(3));
    assert_eq!(interp.call_fn("to-i", vec![f64v(-3.9)]).unwrap(), i64v(-3));
    // Saturation on overflow, and NaN maps to 0 (matching Rust `as`).
    assert_eq!(interp.call_fn("to-i", vec![f64v(1e30)]).unwrap(), i64v(i64::MAX));
    assert_eq!(interp.call_fn("to-i", vec![f64v(-1e30)]).unwrap(), i64v(i64::MIN));
    assert_eq!(interp.call_fn("to-i", vec![f64v(f64::NAN)]).unwrap(), i64v(0));
}

#[test]
fn f64_to_str_uses_the_canonical_deterministic_format() {
    let m = module(FLOATS);
    let s = |x: f64| match Interp::new(&m, 0).call_fn("to-str", vec![f64v(x)]).unwrap() {
        Value::Str(s) => s,
        other => panic!("expected str, got {:?}", other),
    };
    assert_eq!(s(1.5), "1.5000000000000000e+00");
    assert_eq!(s(0.0), "0.0000000000000000e+00");
    assert_eq!(s(-0.25), "-2.5000000000000000e-01");
    assert_eq!(s(f64::INFINITY), "inf");
    assert_eq!(s(f64::NEG_INFINITY), "-inf");
    assert_eq!(s(f64::NAN), "NaN");
    // The interp helper and the display path agree.
    assert_eq!(aury::interp::format_f64(1.5), "1.5000000000000000e+00");
}

#[test]
fn f64_literals_require_a_decimal_point() {
    // `1.0` is a float; `1` stays an i64; a bare identifier is unaffected.
    assert_eq!(aury::ast::parse_f64_literal("1.0"), Some(1.0f64.to_bits()));
    assert_eq!(aury::ast::parse_f64_literal("-2.5"), Some((-2.5f64).to_bits()));
    assert_eq!(aury::ast::parse_f64_literal("1"), None);
    assert_eq!(aury::ast::parse_f64_literal("inf"), None);
    assert_eq!(aury::ast::parse_f64_literal("hello"), None);
}

#[test]
fn passing_i64_where_f64_expected_is_rejected_with_a_float_default_repair() {
    // The literal `1` is an i64; `f64.add` wants f64. The type gate rejects it
    // and offers the `0.0` default-literal repair.
    let src = r#"
(module m
  (fn f (params (x f64)) (ret f64) (body (call f64.add (ref x) (lit 1)))))"#;
    let m = module(src);
    let rejs = match check_module(&m) {
        ValidationOutcome::Rejected(r) => r,
        _ => panic!("expected rejection"),
    };
    let r = &rejs[0];
    assert_eq!(r.gate, Gate::Type);
    assert_eq!(r.kind, "ARG_TYPE_MISMATCH");
    assert!(r.expected.contains("f64"));
    assert!(r.received.contains("i64"));
    assert!(
        r.repairs.iter().any(|repair| repair.note.contains("0.0")),
        "a default f64 literal (0.0) repair should be offered, got {:?}",
        r.repairs.iter().map(|repair| &repair.note).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Track 4: evaluation corpus (repair convergence)
// ---------------------------------------------------------------------------

#[test]
fn evaluation_corpus_converges_as_expected() {
    // The committed corpus is a regression gate: every task's loop outcome must
    // match its declared expectation, every oracle check must pass, and the
    // repair loop must genuinely rescue at least one otherwise-broken task
    // while the intent gate correctly refuses at least one wrong spec.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/corpus.json");
    let report = aury::eval::run_corpus(&manifest, None).expect("run corpus");

    assert!(report.all_passed(), "corpus regressed:\n{}", report.to_markdown());
    assert!(report.tasks.len() >= 6, "corpus should be non-trivial");

    // First-shot vs post-repair is the honest baseline: at least one task must
    // fail first-shot yet be rescued by the loop's mechanical repair.
    let rescued = report
        .tasks
        .iter()
        .filter(|t| t.accepted && t.patches > 0 && !t.validated_first_shot)
        .count();
    assert!(rescued >= 1, "expected at least one task rescued by repair");

    // At least one deliberately-wrong spec must be a true negative (not accepted).
    let true_negatives = report
        .tasks
        .iter()
        .filter(|t| !t.expect_accept && !t.accepted && t.outcome_as_expected)
        .count();
    assert!(true_negatives >= 1, "expected the intent gate to reject a wrong spec");

    // v0.2 headline: the loop now mechanically converges *structural* gates, not
    // just parse. Assert at least one converged task per structural gate the
    // corpus exercises (effect, region), plus the parse case.
    let converged_at = |gate: &str| {
        report
            .tasks
            .iter()
            .filter(|t| t.first_shot_gate == gate && t.accepted && t.expect_accept && t.patches > 0)
            .count()
    };
    assert!(converged_at("parse") >= 1, "parse-gate convergence expected");
    assert!(converged_at("effect") >= 1, "effect-gate convergence expected (Track A)");
    assert!(converged_at("region") >= 1, "region-gate convergence expected (Tracks B/C)");

    // Cross-implementation agreement: where a Python reference is available, it
    // must reproduce every oracle output (hermetic — skipped if python3 absent).
    for t in &report.tasks {
        if t.baseline_available {
            assert_eq!(
                t.baseline_passed, t.baseline_total,
                "reference impl disagreed with Aury on task `{}`",
                t.name
            );
        }
    }

    // Determinism: a second run with the same seed is identical.
    let again = aury::eval::run_corpus(&manifest, None).expect("run corpus again");
    assert_eq!(report.summary(), again.summary(), "eval must be deterministic");
}
