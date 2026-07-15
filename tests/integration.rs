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
    // The repair menu proposes adding the missing capability.
    assert!(rejs[0].repairs.iter().any(|r| r.action == "add_capability"));
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
    let json = std::fs::read_to_string("examples/gcd.json").expect("read gcd.json");
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
    let json = std::fs::read_to_string("examples/gcd.json").unwrap();
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
    let src = std::fs::read_to_string("examples/calculator.aury").unwrap();
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
    let src = std::fs::read_to_string("examples/math.aury").unwrap();
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
