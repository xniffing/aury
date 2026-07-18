//! `aury` CLI: validate, run, test, and repair-loop Aury programs.

use aury::repair::ValidationOutcome;
use aury::validate::check_module;
use aury::{ast::build_module, interp::Interp, lower_sketch::lower_to_mlir_sketch, sexpr::parse};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

const USAGE: &str = "\
aury v0 — an AI-oriented language co-designed with an LLM repair loop

USAGE:
  aury <command> [options] <file.aury>

COMMANDS:
  validate <file>            Run type/effect/region checks; print rejections as JSON.
  run <file> <fn> [args...]  Validate then run <fn>; composites use typed JSON args.
  test <file> [seed]         Validate then run property + contract tests with shrinking
                            and vacuity checks.
  loop <file> [seed]         Run the closed repair loop: validate → apply admissible
                            patches → re-validate, until accept or budget exhaustion.
  lower <file>               Print the MLIR lowering sketch (structural preview).
  ll <file> [out.ll]         Emit validated LLVM IR text.
  compile <file> <fn> [args...] [-o out]
                             Build with clang, run, and print the native result.
  wasm <file> <fn> [args...] [-o out.wasm] [--no-run]
                             Build to a wasm32-wasi module with clang; run it with
                             wasmtime/wasmer if present and print the result.
  wasm-lib <file> --export <fn>[,<fn>...] [-o out.wasm]
                             Build a reusable wasm32-wasi reactor module that
                             exports the named functions (callable from a host
                             such as a browser). Scalar (i64/bool) signatures
                             cross the boundary directly.
  json <file>                Like `validate` but print one JSON object per rejection.
  ingest <file.json> [out]   Convert a JSON AST (the AI authoring surface) to the
                            canonical s-expr form, validate it, and write <out>.aury.
  emit-json <file.aury>      Convert an existing .aury to array-form JSON (for round-trip).
  eval <corpus.json> [--seed N] [--md out.md] [--csv out.csv]
                             Run the closed loop over a corpus of (intent, program)
                             tasks and print a repair-convergence table.
  diagram <file> [--kind call|types] [-o out.md]
                             Render the module's design as a Mermaid diagram:
                             `call` (call graph with effect badges, the default)
                             or `types` (struct data-model class diagram).

EXAMPLES:
  aury validate examples/add.aury
  aury run examples/add.aury add 3 4
  aury test examples/sort.aury 12345
  aury loop examples/broken.aury 12345
  aury ingest examples/gcd.json examples/gcd.aury
  aury diagram examples/agent/vec-pipeline.aury
";

const NATIVE_RUNTIME_SOURCE: &str = include_str!("../runtime/aury_rt.c");
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn write_unique_temp_file(stem: &str, extension: &str, contents: &str) -> Result<PathBuf, String> {
    let safe_stem: String = stem
        .chars()
        .map(|character| if character.is_ascii_alphanumeric() { character } else { '_' })
        .collect();
    for _ in 0..100 {
        let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aury_{}_{}_{}.{}",
            std::process::id(),
            sequence,
            safe_stem,
            extension
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(contents.as_bytes())
                    .map_err(|error| format!("write {}: {}", path.display(), error))?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("create {}: {}", path.display(), error)),
        }
    }
    Err("could not create a unique native compilation temporary file".into())
}

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
        "wasm" => cmd_wasm(&args[2..]),
        "wasm-lib" => cmd_wasm_lib(&args[2..]),
        "json" => cmd_json(&args[2..]),
        "ingest" => cmd_ingest(&args[2..]),
        "emit-json" => cmd_emit_json(&args[2..]),
        "eval" => cmd_eval(&args[2..]),
        "diagram" => cmd_diagram(&args[2..]),
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
            println!("{}", aury::value_io::show_value(&v));
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
    let contract_failures = aury::spec::run_contract_tests(&m, seed, 128);
    if failures.is_empty() && contract_failures.is_empty() {
        println!("all property and contract tests pass (seed={})", seed);
        Ok(ExitCode::SUCCESS)
    } else {
        println!(
            "{} property + {} contract test failure(s):",
            failures.len(),
            contract_failures.len()
        );
        for f in &failures {
            println!("{}", aury::spec::failure_to_rejection(f).to_json());
        }
        for f in &contract_failures {
            println!("{}", aury::spec::contract_failure_to_rejection(f).to_json());
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

fn cmd_eval(args: &[String]) -> Result<ExitCode, String> {
    let manifest = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or("eval <corpus.json> [--seed N] [--md out.md] [--csv out.csv]")?;
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let seed = flag("--seed").map(|s| {
        s.parse::<u64>()
            .unwrap_or_else(|_| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(aury::eval::DEFAULT_SEED))
    });
    let report = aury::eval::run_corpus(std::path::Path::new(manifest), seed)?;
    let markdown = report.to_markdown();
    print!("{}", markdown);
    println!("\n{}", report.summary());
    if let Some(path) = flag("--md") {
        std::fs::write(&path, &markdown).map_err(|e| format!("write {}: {}", path, e))?;
        eprintln!("wrote {}", path);
    }
    if let Some(path) = flag("--csv") {
        std::fs::write(&path, report.to_csv()).map_err(|e| format!("write {}: {}", path, e))?;
        eprintln!("wrote {}", path);
    }
    // Nonzero exit if any task's outcome diverged from expectation or an oracle
    // check failed — so `aury eval` doubles as a regression gate in CI.
    if report.all_passed() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn cmd_lower(args: &[String]) -> Result<ExitCode, String> {
    let path = args.first().ok_or("lower <file>")?;
    let m = build(path)?;
    print!("{}", lower_to_mlir_sketch(&m));
    Ok(ExitCode::SUCCESS)
}

/// diagram: render the module's design as Mermaid. A read-only AST walk — it
/// does not validate first (a diagram of an in-progress module is still useful),
/// and it stays hermetic (emits text; no `dot`/renderer dependency).
fn cmd_diagram(args: &[String]) -> Result<ExitCode, String> {
    let path = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or("diagram <file> [--kind call|types] [-o out.md]")?;
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let kind = match flag("--kind") {
        Some(k) => aury::diagram::Kind::parse(&k)?,
        None => aury::diagram::Kind::Call,
    };
    let m = build(path)?;
    let mermaid = aury::diagram::render(&m, kind);
    // Wrap in a fenced ```mermaid block so the output drops straight into
    // Markdown (GitHub, the README, artifacts) and renders.
    let fenced = format!("```mermaid\n{}```\n", mermaid);
    if let Some(out) = flag("-o").or_else(|| flag("--out")) {
        std::fs::write(&out, &fenced).map_err(|e| format!("write {}: {}", out, e))?;
        eprintln!("wrote {}", out);
    } else {
        print!("{}", fenced);
    }
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
    // `--` ends option parsing, allowing a literal string argument of `-o`.
    let option_end = args.iter().position(|arg| arg == "--").unwrap_or(args.len());
    let o_idx = args[..option_end].iter().position(|arg| arg == "-o");
    let excluded: std::collections::HashSet<usize> = o_idx
        .map(|i| [i, i + 1].into_iter().collect())
        .unwrap_or_default();
    let positional: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != option_end && !excluded.contains(i))
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
    let ll = write_unique_temp_file(entry, "ll", &ir)?;
    let exe = out.unwrap_or_else(|| {
        let p = path.trim_end_matches(".aury");
        format!("{}.{}.exe", p, entry)
    });
    // The runtime is embedded in the CLI binary so installed `aury` binaries
    // do not depend on the source checkout remaining at CARGO_MANIFEST_DIR.
    let runtime_c = write_unique_temp_file(&format!("rt_{}", entry), "c", NATIVE_RUNTIME_SOURCE)?;
    let status = std::process::Command::new("clang")
        .arg("-O2")
        .arg(&ll)
        .arg(&runtime_c)
        .arg("-o")
        .arg(&exe)
        .output()
        .map_err(|e| format!("clang: {} (is clang installed?)", e))?;
    let _ = std::fs::remove_file(&runtime_c);
    if !status.status.success() {
        eprintln!("clang failed:\n{}", String::from_utf8_lossy(&status.stderr));
        eprintln!("IR written to {:?}", ll);
        return Ok(ExitCode::from(1));
    }
    let _ = std::fs::remove_file(&ll);
    // Command lookup does not search the current directory. Prefix a bare
    // relative output name with `./`, while preserving absolute/path outputs.
    let executable_path = std::path::Path::new(&exe);
    let run_path = if executable_path.components().count() == 1 {
        std::path::Path::new(".").join(executable_path)
    } else {
        executable_path.to_path_buf()
    };
    let status = std::process::Command::new(&run_path)
        .output()
        .map_err(|e| format!("run {}: {}", run_path.display(), e))?;
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

/// Resolve the clang used for wasm builds. Apple's system clang usually ships
/// without the WebAssembly backend, and wasi-sdk's clang bundles the sysroot and
/// `wasm-ld`, so prefer an explicit override. Honour `AURY_WASM_CLANG`, then a
/// wasi-sdk layout (`$WASI_SDK_PATH/bin/clang`, `/opt/wasi-sdk/bin/clang`), then
/// fall back to a bare `clang` on PATH.
fn resolve_wasm_clang() -> String {
    if let Ok(explicit) = std::env::var("AURY_WASM_CLANG") {
        if !explicit.is_empty() {
            return explicit;
        }
    }
    let sdk_candidates = std::env::var("WASI_SDK_PATH")
        .ok()
        .into_iter()
        .map(|p| format!("{}/bin/clang", p.trim_end_matches('/')))
        .chain(std::iter::once("/opt/wasi-sdk/bin/clang".to_string()));
    for candidate in sdk_candidates {
        if std::path::Path::new(&candidate).exists() {
            return candidate;
        }
    }
    "clang".to_string()
}

/// Resolve the wasi-libc sysroot for a generic (non-wasi-sdk) clang. wasi-sdk's
/// own clang defaults its sysroot, so returning `None` is fine there. Honour
/// `WASI_SYSROOT`, then a wasi-sdk share layout.
fn resolve_wasi_sysroot() -> Option<String> {
    if let Ok(explicit) = std::env::var("WASI_SYSROOT") {
        if !explicit.is_empty() {
            return Some(explicit);
        }
    }
    let candidates = std::env::var("WASI_SDK_PATH")
        .ok()
        .into_iter()
        .map(|p| format!("{}/share/wasi-sysroot", p.trim_end_matches('/')))
        .chain(std::iter::once("/opt/wasi-sdk/share/wasi-sysroot".to_string()));
    candidates.into_iter().find(|c| std::path::Path::new(c).exists())
}

/// Find an installed wasm runtime (wasmtime, then wasmer) on PATH so a built
/// module can be executed for the same result the interpreter and native backend
/// produce. Returns the program name; both accept `<runtime> <module.wasm>`.
fn resolve_wasm_runtime() -> Option<&'static str> {
    for runtime in ["wasmtime", "wasmer"] {
        let found = std::process::Command::new(runtime)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if found {
            return Some(runtime);
        }
    }
    None
}

/// wasm: like `compile`, but assemble the validated LLVM IR plus the C runtime
/// into a `wasm32-wasi` module. The generated `main` is target-neutral, so WASI's
/// `_start` invokes it and the libc-only runtime (`aury_rt.c`) links against
/// wasi-libc unchanged. When a wasm runtime is present the module is executed so
/// its output can be checked against `aury run` / `aury compile`.
fn cmd_wasm(args: &[String]) -> Result<ExitCode, String> {
    // aury wasm <file> <fn> [args...] [-o out.wasm] [--no-run]
    let no_run = args.iter().any(|arg| arg == "--no-run");
    let option_end = args.iter().position(|arg| arg == "--").unwrap_or(args.len());
    let o_idx = args[..option_end].iter().position(|arg| arg == "-o");
    let excluded: std::collections::HashSet<usize> = o_idx
        .map(|i| [i, i + 1].into_iter().collect())
        .unwrap_or_default();
    let positional: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, arg)| {
            *i != option_end && !excluded.contains(i) && arg.as_str() != "--no-run"
        })
        .map(|(_, a)| a)
        .collect();
    let out = match o_idx {
        Some(i) => Some(
            args.get(i + 1)
                .cloned()
                .ok_or("wasm: -o requires an output path")?,
        ),
        None => None,
    };
    let path = positional.first().ok_or("wasm <file> <fn> [args...] [-o out.wasm] [--no-run]")?;
    let entry = positional.get(1).ok_or("wasm <file> <fn> [args...]")?;
    let arg_strs = if positional.len() >= 3 {
        &positional[2..]
    } else {
        &[]
    };
    let m = build(path)?;
    if let ValidationOutcome::Rejected(rejs) = check_module(&m) {
        eprintln!("rejected before wasm build: {} rejection(s)", rejs.len());
        for r in &rejs {
            eprintln!("{}", r.to_json());
        }
        return Ok(ExitCode::from(1));
    }
    let raw_args: Vec<String> = arg_strs.iter().map(|arg| (*arg).clone()).collect();
    // Name the entry `__main_void` (not `main`): raw IR skips clang's C frontend,
    // so wasi-libc's `_start` finds the entry only under its own entry symbol.
    let ir = aury::lower::lower_program_with_entry(&m, entry, &raw_args, "__main_void")?;
    let ll = write_unique_temp_file(entry, "ll", &ir)?;
    let module_path = out.unwrap_or_else(|| {
        let p = path.trim_end_matches(".aury");
        format!("{}.{}.wasm", p, entry)
    });
    let runtime_c = write_unique_temp_file(&format!("rt_{}", entry), "c", NATIVE_RUNTIME_SOURCE)?;
    let clang = resolve_wasm_clang();
    let mut command = std::process::Command::new(&clang);
    command
        .arg("--target=wasm32-wasip1")
        .arg("-O2")
        .arg(&ll)
        .arg(&runtime_c)
        .arg("-o")
        .arg(&module_path);
    if let Some(sysroot) = resolve_wasi_sysroot() {
        command.arg(format!("--sysroot={}", sysroot));
    }
    let status = command.output().map_err(|e| {
        format!(
            "{}: {} (need a clang with the WebAssembly target; install wasi-sdk \
             and set WASI_SDK_PATH, or set AURY_WASM_CLANG)",
            clang, e
        )
    })?;
    let _ = std::fs::remove_file(&runtime_c);
    if !status.status.success() {
        eprintln!("wasm build failed:\n{}", String::from_utf8_lossy(&status.stderr));
        eprintln!("IR written to {:?}", ll);
        return Ok(ExitCode::from(1));
    }
    let _ = std::fs::remove_file(&ll);
    // Status goes to stderr so stdout carries only the program's output, exactly
    // like `compile` — callers can parse the result without stripping a banner.
    eprintln!("built {} → {} (wasm32-wasi)", path, module_path);
    if no_run {
        return Ok(ExitCode::SUCCESS);
    }
    let Some(runtime) = resolve_wasm_runtime() else {
        eprintln!("(no wasm runtime found; run with `wasmtime {}` or `wasmer {}`)", module_path, module_path);
        return Ok(ExitCode::SUCCESS);
    };
    let status = std::process::Command::new(runtime)
        .arg(&module_path)
        .output()
        .map_err(|e| format!("run {} {}: {}", runtime, module_path, e))?;
    print!("{}", String::from_utf8_lossy(&status.stdout));
    if !status.status.success() {
        eprintln!("{} stderr:\n{}", runtime, String::from_utf8_lossy(&status.stderr));
        eprintln!("wasm exit code: {:?}", status.status.code());
    }
    Ok(if status.status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// wasm-lib: build a reusable wasm32-wasi *reactor* module (no `main`) that
/// exports the named Aury functions for a host to call — e.g. a browser calling
/// the module with `WebAssembly.instantiate`. Reactor modules export
/// `_initialize` (call once before use) plus each requested function under the
/// symbol `aury__<name>`. Scalar Aury types (`i64`, `bool`) map to wasm `i64`
/// and cross the JS boundary as `BigInt` directly; aggregate returns (`str`,
/// `vec`, `struct`, `result`) are pointers into the module's linear memory and
/// need host-side marshaling, so they are reported rather than silently exported.
fn cmd_wasm_lib(args: &[String]) -> Result<ExitCode, String> {
    // aury wasm-lib <file> --export <fn>[,<fn>...] [-o out.wasm]
    let export_idx = args.iter().position(|arg| arg == "--export");
    let export_list = match export_idx {
        Some(i) => args.get(i + 1).ok_or("wasm-lib: --export requires a comma-separated function list")?,
        None => return Err("wasm-lib <file> --export <fn>[,<fn>...] [-o out.wasm]".into()),
    };
    let exports: Vec<String> = export_list
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if exports.is_empty() {
        return Err("wasm-lib: --export list is empty".into());
    }
    let o_idx = args.iter().position(|arg| arg == "-o");
    let excluded: std::collections::HashSet<usize> = o_idx
        .map(|i| [i, i + 1].into_iter().collect())
        .unwrap_or_default();
    let export_excluded: std::collections::HashSet<usize> =
        export_idx.map(|i| [i, i + 1].into_iter().collect()).unwrap_or_default();
    let positional: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, _)| !excluded.contains(i) && !export_excluded.contains(i))
        .map(|(_, a)| a)
        .collect();
    let out = match o_idx {
        Some(i) => Some(args.get(i + 1).cloned().ok_or("wasm-lib: -o requires an output path")?),
        None => None,
    };
    let path = positional.first().ok_or("wasm-lib <file> --export <fn>[,<fn>...] [-o out.wasm]")?;
    let m = build(path)?;
    if let ValidationOutcome::Rejected(rejs) = check_module(&m) {
        eprintln!("rejected before wasm-lib build: {} rejection(s)", rejs.len());
        for r in &rejs {
            eprintln!("{}", r.to_json());
        }
        return Ok(ExitCode::from(1));
    }
    // Each export must name a defined function; warn when its signature crosses
    // the boundary as a pointer rather than a scalar.
    use aury::ast::ModuleItem;
    for name in &exports {
        let function = m.items.iter().find_map(|item| match item {
            ModuleItem::Fn(function) if &function.name == name => Some(function),
            _ => None,
        });
        let Some(function) = function else {
            return Err(format!("wasm-lib: --export names unknown function `{}`", name));
        };
        let pointer_type = |t: &aury::types::Type| {
            !matches!(t, aury::types::Type::I64 | aury::types::Type::Bool | aury::types::Type::Unit)
        };
        if pointer_type(&function.ret) || function.params.iter().any(|p| pointer_type(&p.ty)) {
            eprintln!(
                "wasm-lib: note: `{}` has non-scalar params/return; those cross as linear-memory pointers and need host marshaling",
                name
            );
        }
    }
    let ir = aury::lower::lower_module(&m)?;
    let stem = std::path::Path::new(path.as_str())
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module");
    let ll = write_unique_temp_file(stem, "ll", &ir)?;
    let module_path = out.unwrap_or_else(|| format!("{}.wasm", path.trim_end_matches(".aury")));
    let runtime_c = write_unique_temp_file(&format!("rt_{}", stem), "c", NATIVE_RUNTIME_SOURCE)?;
    let clang = resolve_wasm_clang();
    let mut command = std::process::Command::new(&clang);
    command
        .arg("--target=wasm32-wasip1")
        .arg("-mexec-model=reactor")
        .arg("-O2")
        .arg(&ll)
        .arg(&runtime_c);
    for name in &exports {
        command.arg(format!("-Wl,--export=aury__{}", name));
    }
    command.arg("-o").arg(&module_path);
    if let Some(sysroot) = resolve_wasi_sysroot() {
        command.arg(format!("--sysroot={}", sysroot));
    }
    let status = command.output().map_err(|e| {
        format!(
            "{}: {} (need a clang with the WebAssembly target; install wasi-sdk \
             and set WASI_SDK_PATH, or set AURY_WASM_CLANG)",
            clang, e
        )
    })?;
    let _ = std::fs::remove_file(&runtime_c);
    if !status.status.success() {
        eprintln!("wasm-lib build failed:\n{}", String::from_utf8_lossy(&status.stderr));
        eprintln!("IR written to {:?}", ll);
        return Ok(ExitCode::from(1));
    }
    let _ = std::fs::remove_file(&ll);
    eprintln!("built {} → {} (wasm32-wasi reactor)", path, module_path);
    eprintln!("  exports: _initialize, {}", exports.iter().map(|n| format!("aury__{}", n)).collect::<Vec<_>>().join(", "));
    Ok(ExitCode::SUCCESS)
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
        .map(|(param, arg)| {
            aury::value_io::parse_cli_value(module, &param.ty, arg)
                .map_err(|error| format!("arg for `{}`: {}", param.name, error))
        })
        .collect()
}
