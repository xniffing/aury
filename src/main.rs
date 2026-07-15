//! `aury` CLI: validate, run, test, and repair-loop Aury programs.

use aury::repair::ValidationOutcome;
use aury::validate::check_module;
use aury::{ast::build_module, interp::Interp, lower_sketch::lower_to_mlir_sketch, sexpr::parse};
use std::process::ExitCode;

const USAGE: &str = "\
aury v0 — an AI-oriented language co-designed with an LLM repair loop

USAGE:
  aury <command> [options] <file.aury>

COMMANDS:
  validate <file>            Run type/effect/region checks; print rejections as JSON.
  run <file> <fn> [args...]  Validate then run function <fn> with i64/bool args.
  test <file> [seed]         Validate then run property tests with shrinking + vacuity check.
  loop <file> [seed]         Run the closed repair loop: validate → apply admissible
                            patches → re-validate, until accept or budget exhaustion.
  lower <file>               Print the MLIR lowering sketch (structural preview).
  json <file>                Like `validate` but print one JSON object per rejection.
  ingest <file.json> [out]   Convert a JSON AST (the AI authoring surface) to the
                            canonical s-expr form, validate it, and write <out>.aury.
  emit-json <file.aury>      Convert an existing .aury to array-form JSON (for round-trip).

EXAMPLES:
  aury validate examples/add.aury
  aury run examples/add.aury add 3 4
  aury test examples/sort.aury 12345
  aury loop examples/broken.aury 12345
  aury ingest examples/gcd.json examples/gcd.aury
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let cmd = &args[1];
    let result = match cmd.as_str() {
        "validate" => cmd_validate(&args[2..]),
        "run" => cmd_run(&args[2..]),
        "test" => cmd_test(&args[2..]),
        "loop" => cmd_loop(&args[2..]),
        "lower" => cmd_lower(&args[2..]),
        "ll" => cmd_ll(&args[2..]),
        "compile" => cmd_compile(&args[2..]),
        "json" => cmd_json(&args[2..]),
        "ingest" => cmd_ingest(&args[2..]),
        "emit-json" => cmd_emit_json(&args[2..]),
        other => {
            eprintln!("unknown command: {}\n\n{}", other, USAGE);
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}

fn read_file(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path, e))
}

fn build(path: &str) -> Result<aury::ast::Module, String> {
    let src = read_file(path)?;
    let xs = parse(&src).map_err(|e| e.to_string())?;
    if xs.len() != 1 {
        return Err("expected exactly one top-level (module ...) form".into());
    }
    build_module(&xs[0])
}

fn cmd_validate(args: &[String]) -> Result<ExitCode, String> {
    let path = args.first().ok_or("validate <file>")?;
    let m = build(path)?;
    match check_module(&m) {
        ValidationOutcome::Accepted => {
            println!("accepted: type/effect/region checks pass");
            Ok(ExitCode::SUCCESS)
        }
        ValidationOutcome::Rejected(rejs) => {
            println!("rejected: {} rejection(s)", rejs.len());
            for r in &rejs {
                println!("----");
                println!("{}", r.to_json());
            }
            Ok(ExitCode::from(1))
        }
    }
}

fn cmd_json(args: &[String]) -> Result<ExitCode, String> {
    let path = args.first().ok_or("json <file>")?;
    let m = build(path)?;
    match check_module(&m) {
        ValidationOutcome::Accepted => {
            println!("[]");
            Ok(ExitCode::SUCCESS)
        }
        ValidationOutcome::Rejected(rejs) => {
            for r in &rejs {
                println!("{}", r.to_json());
            }
            Ok(ExitCode::from(1))
        }
    }
}

fn cmd_run(args: &[String]) -> Result<ExitCode, String> {
    let path = args.first().ok_or("run <file> <fn> [args]")?;
    let fn_name = args.get(1).ok_or("run <file> <fn> [args]")?;
    let arg_strs = &args[2..];
    let m = build(path)?;
    if let ValidationOutcome::Rejected(rejs) = check_module(&m) {
        eprintln!("rejected before run: {} rejection(s)", rejs.len());
        for r in &rejs {
            eprintln!("{}", r.to_json());
        }
        return Ok(ExitCode::from(1));
    }
    let mut interp = Interp::new(&m, 0xC0FFEE);
    let vals = parse_fn_args(&m, fn_name, arg_strs)?;
    match interp.call_fn(fn_name, vals) {
        Ok(v) => {
            println!("{}", show_value(&v));
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("{}", e);
            Ok(ExitCode::from(1))
        }
    }
}

fn cmd_test(args: &[String]) -> Result<ExitCode, String> {
    let path = args.first().ok_or("test <file> [seed]")?;
    let seed: u64 = args
        .get(1)
        .map(|s| {
            s.parse::<u64>()
                .unwrap_or_else(|_| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0))
        })
        .unwrap_or(0xC0FFEE);
    let m = build(path)?;
    if let ValidationOutcome::Rejected(rejs) = check_module(&m) {
        eprintln!("rejected before tests: {} rejection(s)", rejs.len());
        for r in &rejs {
            eprintln!("{}", r.to_json());
        }
        return Ok(ExitCode::from(1));
    }
    let failures = aury::spec::run_property_tests(&m, seed, 128);
    if failures.is_empty() {
        println!("all property tests pass (seed={})", seed);
        Ok(ExitCode::SUCCESS)
    } else {
        println!("{} property test failure(s):", failures.len());
        for f in &failures {
            let rej = aury::spec::failure_to_rejection(f);
            println!("{}", rej.to_json());
        }
        Ok(ExitCode::from(1))
    }
}

fn cmd_loop(args: &[String]) -> Result<ExitCode, String> {
    let path = args.first().ok_or("loop <file> [seed]")?;
    let seed: u64 = args
        .get(1)
        .map(|s| {
            s.parse::<u64>()
                .unwrap_or_else(|_| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0))
        })
        .unwrap_or(0xC0FFEE);
    let src = read_file(path)?;
    let res = aury::repair_loop(&src, true, seed);
    for line in &res.log {
        println!("[loop] {}", line);
    }
    if res.accepted {
        println!("=== ACCEPTED after {} patches ===", res.patches_applied);
        println!("{}", res.source);
        Ok(ExitCode::SUCCESS)
    } else {
        println!("=== NOT ACCEPTED (patches={}, regenerate={}) ===", res.patches_applied, res.recommend_regenerate);
        if !res.remaining.is_empty() {
            println!("remaining rejections:");
            for r in &res.remaining {
                println!("{}", r.to_json());
            }
        }
        Ok(ExitCode::from(1))
    }
}

fn cmd_lower(args: &[String]) -> Result<ExitCode, String> {
    let path = args.first().ok_or("lower <file>")?;
    let m = build(path)?;
    print!("{}", lower_to_mlir_sketch(&m));
    Ok(ExitCode::SUCCESS)
}

/// ll: lower a validated module to LLVM IR text (the real native backend) and
/// print it. This is Aury → LLVM IR; pipe it through `llc`/`clang` for native
/// code. See `compile` for end-to-end.
fn cmd_ll(args: &[String]) -> Result<ExitCode, String> {
    let path = args.first().ok_or("ll <file> [out.ll]")?;
    let m = build(path)?;
    // Validate before lowering — never lower an invalid program.
    if let ValidationOutcome::Rejected(rejs) = check_module(&m) {
        eprintln!("rejected before lowering: {} rejection(s)", rejs.len());
        for r in &rejs {
            eprintln!("{}", r.to_json());
        }
        return Ok(ExitCode::from(1));
    }
    let ir = aury::lower::lower_module(&m)?;
    if let Some(out) = args.get(1) {
        std::fs::write(out, &ir).map_err(|e| format!("write {}: {}", out, e))?;
        println!("lowered {} → {} (LLVM IR)", path, out);
    } else {
        print!("{}", ir);
    }
    Ok(ExitCode::SUCCESS)
}

/// compile: lower a module, add a C `main` that calls <fn> with <args>, assemble
/// with clang to a native executable, run it, and print the result. The native
/// result must match `aury run` — that equivalence is the correctness check for
/// the lowering.
fn cmd_compile(args: &[String]) -> Result<ExitCode, String> {
    // aury compile <file> <fn> [args...] [-o out]
    let o_idx = args.iter().position(|a| a == "-o");
    let excluded: std::collections::HashSet<usize> = o_idx
        .map(|i| [i, i + 1].into_iter().collect())
        .unwrap_or_default();
    let positional: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !excluded.contains(i) && a.as_str() != "-o")
        .map(|(_, a)| a)
        .collect();
    let out = match o_idx {
        Some(i) => Some(
            args.get(i + 1)
                .cloned()
                .ok_or("compile: -o requires an output path")?,
        ),
        None => None,
    };
    let path = positional.first().ok_or("compile <file> <fn> [args...] [-o out]")?;
    let entry = positional.get(1).ok_or("compile <file> <fn> [args...]")?;
    let arg_strs = if positional.len() >= 3 {
        &positional[2..]
    } else {
        &[]
    };
    let m = build(path)?;
    if let ValidationOutcome::Rejected(rejs) = check_module(&m) {
        eprintln!("rejected before compile: {} rejection(s)", rejs.len());
        for r in &rejs {
            eprintln!("{}", r.to_json());
        }
        return Ok(ExitCode::from(1));
    }
    let raw_args: Vec<String> = arg_strs.iter().map(|arg| (*arg).clone()).collect();
    let ir = aury::lower::lower_program_with_main(&m, entry, &raw_args)?;
    let ll = std::env::temp_dir().join(format!("aury_{}.ll", entry));
    std::fs::write(&ll, &ir).map_err(|e| format!("write ll: {}", e))?;
    let exe = out.unwrap_or_else(|| {
        let p = path.trim_end_matches(".aury");
        format!("{}.{}.exe", p, entry)
    });
    // Assemble + link with clang (also runs mem2reg + optimization). Link the
    // Aury runtime (str/result ops) alongside the generated IR.
    let rt = format!("{}/runtime/aury_rt.c", env!("CARGO_MANIFEST_DIR"));
    let status = std::process::Command::new("clang")
        .arg("-O2")
        .arg(&ll)
        .arg(&rt)
        .arg("-o")
        .arg(&exe)
        .output()
        .map_err(|e| format!("clang: {} (is clang installed?)", e))?;
    if !status.status.success() {
        eprintln!("clang failed:\n{}", String::from_utf8_lossy(&status.stderr));
        eprintln!("IR written to {:?}", ll);
        return Ok(ExitCode::from(1));
    }
    // Run the native binary.
    let status = std::process::Command::new(&exe)
        .output()
        .map_err(|e| format!("run {}: {}", exe, e))?;
    print!("{}", String::from_utf8_lossy(&status.stdout));
    if !status.status.success() {
        eprintln!("native exit code: {:?}", status.status.code());
    }
    Ok(if status.status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// ingest: the AI authoring surface. Read a JSON AST, convert to the canonical
/// s-expr IR (assigning Merkle ids via the existing build path), and write the
/// canonical `.aury`. By default it validates first and refuses to write an
/// invalid program; pass `--force` to write anyway so the repair loop can fix
/// it (this is what the skill's `dev.sh` does).
fn cmd_ingest(args: &[String]) -> Result<ExitCode, String> {
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let force = args.iter().any(|a| a == "--force");
    let in_path = positional.first().ok_or("ingest <file.json> [--force] [out.aury]")?;
    let json_text = read_file(in_path)?;
    let sexpr = aury::json::parse_json_sexpr(&json_text)?;
    let module = aury::ast::build_module(&sexpr)?;
    // Validate before emitting — unless --force (the AI workflow converts
    // first, then lets `aury loop` repair).
    let outcome = check_module(&module);
    if !force {
        if let ValidationOutcome::Rejected(rejs) = &outcome {
            eprintln!("rejected before ingest: {} rejection(s)", rejs.len());
            for r in rejs {
                eprintln!("{}", r.to_json());
            }
            eprintln!("(pass --force to write anyway, then `aury loop` to repair)");
            return Ok(ExitCode::from(1));
        }
    } else if let ValidationOutcome::Rejected(rejs) = &outcome {
        eprintln!("ingest --force: {} rejection(s) present; writing for repair", rejs.len());
    }
    let out_path = if positional.len() >= 2 {
        positional[1].clone()
    } else {
        let p = in_path.trim_end_matches(".json");
        format!("{}.aury", p)
    };
    std::fs::write(&out_path, format!("{:?}", sexpr))
        .map_err(|e| format!("write {}: {}", out_path, e))?;
    let tag = if force { "written (force)" } else { "validated" };
    println!("ingested {} → {} ({})", in_path, out_path, tag);
    Ok(ExitCode::SUCCESS)
}

/// emit-json: convert an existing .aury to array-form JSON, so it can be
/// round-tripped through `ingest`.
fn cmd_emit_json(args: &[String]) -> Result<ExitCode, String> {
    let path = args.first().ok_or("emit-json <file.aury>")?;
    let src = read_file(path)?;
    let xs = parse(&src).map_err(|e| e.to_string())?;
    if xs.len() != 1 {
        return Err("expected one top-level form".into());
    }
    let v = aury::json::sexpr_to_json(&xs[0]);
    println!("{}", serde_json::to_string_pretty(&v).unwrap());
    Ok(ExitCode::SUCCESS)
}

fn parse_fn_args(
    module: &aury::ast::Module,
    fn_name: &str,
    args: &[String],
) -> Result<Vec<aury::interp::Value>, String> {
    use aury::ast::ModuleItem;
    use aury::interp::Value;
    use aury::types::Type;

    let function = module
        .items
        .iter()
        .find_map(|item| match item {
            ModuleItem::Fn(function) if function.name == fn_name => Some(function),
            _ => None,
        })
        .ok_or_else(|| format!("fn not found: {}", fn_name))?;
    if function.params.len() != args.len() {
        return Err(format!(
            "fn {} takes {} args, got {}",
            fn_name,
            function.params.len(),
            args.len()
        ));
    }

    function
        .params
        .iter()
        .zip(args)
        .map(|(param, arg)| match param.ty {
            Type::I64 => arg
                .parse::<i64>()
                .map(Value::I64)
                .map_err(|_| format!("arg `{}` for `{}` is not an i64", arg, param.name)),
            Type::Bool => match arg.as_str() {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err(format!("arg `{}` for `{}` is not a bool", arg, param.name)),
            },
            Type::Str => Ok(Value::Str(arg.clone())),
            _ => Err(format!(
                "CLI arguments of type {:?} are not supported for `{}`",
                param.ty, param.name
            )),
        })
        .collect()
}

fn show_value(v: &aury::interp::Value) -> String {
    use aury::interp::Value;
    match v {
        Value::I64(n) => format!("{}", n),
        Value::Bool(b) => format!("{}", b),
        Value::Str(s) => format!("{:?}", s),
        Value::Unit => "unit".into(),
        Value::Vec(vs) => format!(
            "[{}]",
            vs.iter().map(show_value).collect::<Vec<_>>().join(", ")
        ),
        Value::Struct(name, fs) => format!(
            "{}{{{}}}",
            name,
            fs.iter()
                .map(|(n, v)| format!("{}: {}", n, show_value(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Region(_) => "region".into(),
        Value::ResultOk(v) => format!("ok({})", show_value(v)),
        Value::ResultErr(v) => format!("err({})", show_value(v)),
    }
}