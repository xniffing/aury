//! Track 4: the evaluation harness.
//!
//! This is not a language feature — it measures whether the repair-oriented
//! thesis actually holds. Given a *corpus* of (natural-language intent, program)
//! tasks, it runs the same closed loop an agent uses ([`crate::repair_loop`])
//! over each task and records, per task and in aggregate:
//!
//!   - **first-shot**: did the model's initial program parse, type-check, and
//!     pass its own property/contract tests with *zero* repairs?
//!   - **post-repair**: did the closed loop reach an accepted state, and after
//!     how many mechanical patches?
//!   - **oracle**: for tasks that carry concrete input→output checks, does the
//!     accepted program actually compute the right answers?
//!   - **intent gate**: for tasks whose spec is deliberately wrong, does the
//!     loop *correctly refuse* to accept (a true negative, not a failure)?
//!
//! The honest baseline this answers is **first-shot vs post-repair**: how many
//! tasks the repair loop rescues that would otherwise be rejected. A
//! cross-language comparison (Aury vs a mainstream target) is left as future
//! work — we do not fabricate numbers for a toolchain we cannot run here.
//!
//! The report is deterministic (fixed seed) and rendered as both a Markdown
//! table (for the README's Evaluation section) and CSV.

use crate::ast::{build_module, Module, ModuleItem};
use crate::interp::Interp;
use crate::repair::ValidationOutcome;
use crate::spec::{run_contract_tests, run_property_tests};
use crate::validate::check_module;
use crate::value_io::{parse_cli_value, show_value};
use serde_json::Value as Json;
use std::path::{Path, PathBuf};

/// Default deterministic seed (shared with the CLI's `test`/`loop`).
pub const DEFAULT_SEED: u64 = 0xC0FFEE;
/// Property/contract test count per task (matches `aury test`).
const TEST_CASES: usize = 128;

/// A concrete reference-oracle check: call `fn_name` with `args`, and assert the
/// interpreter's `show_value` output equals `expect`.
pub struct OracleCheck {
    pub fn_name: String,
    pub args: Vec<String>,
    pub expect: String,
}

/// One corpus task: an intent, a program to author/repair, and (optionally)
/// oracle checks. `expect_accept` is `false` for tasks whose spec is
/// deliberately unsatisfiable, where the *correct* loop outcome is a rejection.
pub struct Task {
    pub name: String,
    pub intent: String,
    pub program: PathBuf,
    pub expect_accept: bool,
    pub checks: Vec<OracleCheck>,
}

/// Per-task measurement.
pub struct TaskReport {
    pub name: String,
    pub intent: String,
    /// First-shot: the original program built into an AST with no repairs.
    pub parsed_first_shot: bool,
    /// First-shot: type/effect/region checks passed with no repairs.
    pub validated_first_shot: bool,
    /// The earliest gate that rejected the first-shot program: one of `parse`,
    /// `type`, `effect`, `region`, `contract`, `intent`, or `""` (fully valid
    /// first-shot). Drives the per-gate convergence breakdown.
    pub first_shot_gate: String,
    /// First-shot: properties + contracts passed (only meaningful if validated).
    pub intent_first_shot: bool,
    /// Post-repair: the closed loop reached an accepted state.
    pub accepted: bool,
    pub patches: u32,
    pub recommend_regenerate: bool,
    pub remaining: usize,
    pub expect_accept: bool,
    /// The loop's accept/reject outcome matched the task's expectation.
    pub outcome_as_expected: bool,
    pub checks_total: usize,
    pub checks_passed: usize,
    /// Short human-readable note (first failure reason, or a status).
    pub note: String,
}

impl TaskReport {
    /// A task counts as a full pass when the loop outcome matched expectation
    /// *and* every oracle check passed.
    pub fn passed(&self) -> bool {
        self.outcome_as_expected && self.checks_passed == self.checks_total
    }
}

/// The whole-corpus report.
pub struct CorpusReport {
    pub seed: u64,
    pub tasks: Vec<TaskReport>,
}

/// Read a program file into canonical s-expression *source text* — the form the
/// repair loop operates on. `.json` (the AI authoring surface) is converted
/// through the same path `aury ingest` uses; `.aury` is read verbatim, so a
/// paren-deficit program stays broken for the loop's parse-repair to fix.
fn program_source(path: &Path) -> Result<String, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    if path.extension().and_then(|e| e.to_str()) == Some("json") {
        let sexpr = crate::json::parse_json_sexpr(&text)?;
        Ok(format!("{:?}", sexpr))
    } else {
        Ok(text)
    }
}

fn build_source(source: &str) -> Result<Module, String> {
    let xs = crate::sexpr::parse(source).map_err(|e| e.to_string())?;
    if xs.len() != 1 {
        return Err("expected exactly one top-level (module ...) form".into());
    }
    build_module(&xs[0])
}

/// Run every oracle check against an accepted module. Returns (passed, total,
/// note-of-first-failure).
fn run_checks(module: &Module, checks: &[OracleCheck], seed: u64) -> (usize, usize, String) {
    let mut passed = 0usize;
    let mut note = String::new();
    for check in checks {
        let function = module.items.iter().find_map(|item| match item {
            ModuleItem::Fn(f) if f.name == check.fn_name => Some(f),
            _ => None,
        });
        let Some(function) = function else {
            if note.is_empty() {
                note = format!("oracle fn `{}` not found", check.fn_name);
            }
            continue;
        };
        let values: Result<Vec<_>, _> = function
            .params
            .iter()
            .zip(&check.args)
            .map(|(p, a)| parse_cli_value(module, &p.ty, a))
            .collect();
        let values = match values {
            Ok(v) => v,
            Err(e) => {
                if note.is_empty() {
                    note = format!("{}: bad oracle args: {}", check.fn_name, e);
                }
                continue;
            }
        };
        let got = match Interp::new(module, seed).call_fn(&check.fn_name, values) {
            Ok(v) => show_value(&v),
            Err(e) => {
                if note.is_empty() {
                    note = format!("{}: trapped: {}", check.fn_name, e);
                }
                continue;
            }
        };
        if got == check.expect {
            passed += 1;
        } else if note.is_empty() {
            note = format!(
                "{}({}) = {} (expected {})",
                check.fn_name,
                check.args.join(","),
                got,
                check.expect
            );
        }
    }
    (passed, checks.len(), note)
}

/// Run one task end to end.
pub fn run_task(task: &Task, seed: u64) -> TaskReport {
    let mut report = TaskReport {
        name: task.name.clone(),
        intent: task.intent.clone(),
        parsed_first_shot: false,
        validated_first_shot: false,
        first_shot_gate: String::new(),
        intent_first_shot: false,
        accepted: false,
        patches: 0,
        recommend_regenerate: false,
        remaining: 0,
        expect_accept: task.expect_accept,
        outcome_as_expected: false,
        checks_total: task.checks.len(),
        checks_passed: 0,
        note: String::new(),
    };

    let source = match program_source(&task.program) {
        Ok(s) => s,
        Err(e) => {
            report.note = e;
            report.outcome_as_expected = !task.expect_accept; // a load failure is never an accept
            return report;
        }
    };

    // First-shot: no repairs at all. Record the *earliest* gate that rejects it.
    match build_source(&source) {
        Err(_) => report.first_shot_gate = "parse".into(),
        Ok(module) => {
            report.parsed_first_shot = true;
            match check_module(&module) {
                ValidationOutcome::Accepted => {
                    report.validated_first_shot = true;
                    let props = run_property_tests(&module, seed, TEST_CASES);
                    let contracts = run_contract_tests(&module, seed, TEST_CASES);
                    report.intent_first_shot = props.is_empty() && contracts.is_empty();
                    if !report.intent_first_shot {
                        report.first_shot_gate = "intent".into();
                    }
                }
                ValidationOutcome::Rejected(rejs) => {
                    report.first_shot_gate = rejs
                        .first()
                        .map(|r| r.gate.as_str().to_string())
                        .unwrap_or_default();
                }
            }
        }
    }

    // Post-repair: the closed loop (validate → patch → re-validate → intent gate).
    let result = crate::repair_loop(&source, true, seed);
    report.accepted = result.accepted;
    report.patches = result.patches_applied;
    report.recommend_regenerate = result.recommend_regenerate;
    report.remaining = result.remaining.len();
    report.outcome_as_expected = result.accepted == task.expect_accept;

    if result.accepted {
        // Oracle checks run against the accepted (repaired) program.
        match build_source(&result.source) {
            Ok(module) => {
                let (passed, total, note) = run_checks(&module, &task.checks, seed);
                report.checks_passed = passed;
                report.checks_total = total;
                if report.note.is_empty() {
                    report.note = note;
                }
            }
            Err(e) => report.note = format!("accepted but re-build failed: {}", e),
        }
    } else {
        // A correctly-rejected task (expect_accept:false) is a true negative.
        if !task.expect_accept {
            report.checks_passed = report.checks_total; // vacuously satisfied
            if report.note.is_empty() {
                report.note = "correctly rejected (intent gate)".into();
            }
        } else if report.note.is_empty() {
            report.note = if result.recommend_regenerate {
                format!("not accepted; {} rejection(s), regenerate", report.remaining)
            } else {
                format!("not accepted; {} rejection(s)", report.remaining)
            };
        }
    }
    report
}

/// Parse the corpus manifest and resolve program paths relative to it.
pub fn load_corpus(manifest_path: &Path) -> Result<(u64, Vec<Task>), String> {
    let text = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("read {}: {}", manifest_path.display(), e))?;
    let json: Json = serde_json::from_str(&text).map_err(|e| format!("manifest JSON: {}", e))?;
    let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));

    let seed = match json.get("seed") {
        None => DEFAULT_SEED,
        Some(Json::Number(n)) => n.as_u64().ok_or("seed must be a u64")?,
        Some(Json::String(s)) => s
            .parse::<u64>()
            .or_else(|_| u64::from_str_radix(s.trim_start_matches("0x"), 16))
            .map_err(|_| format!("bad seed `{}`", s))?,
        Some(_) => return Err("seed must be a number or string".into()),
    };

    let tasks_json = json
        .get("tasks")
        .and_then(|t| t.as_array())
        .ok_or("manifest needs a `tasks` array")?;
    let mut tasks = Vec::new();
    for t in tasks_json {
        let name = t.get("name").and_then(|v| v.as_str()).ok_or("task.name")?;
        let intent = t.get("intent").and_then(|v| v.as_str()).unwrap_or("");
        let program = t
            .get("program")
            .and_then(|v| v.as_str())
            .ok_or("task.program")?;
        let expect_accept = t
            .get("expect_accept")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let mut checks = Vec::new();
        if let Some(cs) = t.get("checks").and_then(|v| v.as_array()) {
            for c in cs {
                let fn_name = c.get("fn").and_then(|v| v.as_str()).ok_or("check.fn")?;
                let args = c
                    .get("args")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().map(json_scalar_to_string).collect())
                    .unwrap_or_default();
                let expect = c
                    .get("expect")
                    .map(json_scalar_to_string)
                    .ok_or("check.expect")?;
                checks.push(OracleCheck {
                    fn_name: fn_name.to_string(),
                    args,
                    expect,
                });
            }
        }
        tasks.push(Task {
            name: name.to_string(),
            intent: intent.to_string(),
            program: base.join(program),
            expect_accept,
            checks,
        });
    }
    Ok((seed, tasks))
}

/// Oracle args/expects may be written as JSON scalars for convenience; render
/// them the way the CLI would (strings verbatim, numbers/bools stringified).
fn json_scalar_to_string(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        Json::Number(n) => n.to_string(),
        Json::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Run the whole corpus.
pub fn run_corpus(manifest_path: &Path, seed_override: Option<u64>) -> Result<CorpusReport, String> {
    let (manifest_seed, tasks) = load_corpus(manifest_path)?;
    let seed = seed_override.unwrap_or(manifest_seed);
    let tasks = tasks.iter().map(|t| run_task(t, seed)).collect();
    Ok(CorpusReport { seed, tasks })
}

impl CorpusReport {
    fn count(&self, f: impl Fn(&TaskReport) -> bool) -> usize {
        self.tasks.iter().filter(|t| f(t)).count()
    }

    /// Every task's loop outcome matched expectation and every oracle passed.
    pub fn all_passed(&self) -> bool {
        self.tasks.iter().all(|t| t.passed())
    }

    /// Machine-checkable one-line summary.
    pub fn summary(&self) -> String {
        let n = self.tasks.len();
        let first_shot = self.count(|t| t.validated_first_shot);
        let accepted = self.count(|t| t.accepted);
        let expected = self.count(|t| t.outcome_as_expected);
        let repaired = self.count(|t| t.accepted && t.patches > 0);
        let checks_total: usize = self.tasks.iter().map(|t| t.checks_total).sum();
        let checks_passed: usize = self.tasks.iter().map(|t| t.checks_passed).sum();
        format!(
            "{}/{} tasks: outcome-as-expected={}, first-shot-valid={}, accepted={}, \
             rescued-by-repair={}, oracle-checks={}/{} (seed=0x{:X})",
            expected, n, expected, first_shot, accepted, repaired, checks_passed, checks_total,
            self.seed
        )
    }

    /// The repair-convergence table, as GitHub-flavored Markdown.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "## Evaluation — repair convergence over the corpus\n\n\
             Deterministic (seed `0x{:X}`), reproduced by `aury eval eval/corpus.json`.\n\n\
             Columns: **First-shot** = the model's initial program passed type + intent \
             checks with zero repairs; **Loop** = accepted (`✓`) / correctly rejected \
             (`ø`, a deliberately-wrong spec) / failed (`✗`) after the closed loop; \
             **Patches** = mechanical repairs the loop applied; **Oracle** = concrete \
             input→output checks that passed.\n\n",
            self.seed
        ));
        out.push_str("| Task | First-shot | Loop | Patches | Oracle | Notes |\n");
        out.push_str("|------|:----------:|:----:|:-------:|:------:|-------|\n");
        for t in &self.tasks {
            // Precise gate label (parse✗ / type✗ / effect✗ / region✗ / intent✗),
            // matching the per-gate breakdown below.
            let first = if t.first_shot_gate.is_empty() {
                "✓".to_string()
            } else {
                format!("{}✗", t.first_shot_gate)
            };
            let loop_col = if !t.expect_accept {
                if t.accepted {
                    "✗"
                } else {
                    "ø"
                }
            } else if t.accepted {
                "✓"
            } else {
                "✗"
            };
            let oracle = if t.checks_total == 0 {
                "—".to_string()
            } else {
                format!("{}/{}", t.checks_passed, t.checks_total)
            };
            let note = t.note.replace('|', "\\|");
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                t.name, first, loop_col, t.patches, oracle, note
            ));
        }
        let n = self.tasks.len();
        out.push_str(&format!(
            "\n**{}/{} outcomes as expected** · first-shot-valid {} · rescued by repair {} · \
             oracle checks {}/{}.\n",
            self.count(|t| t.outcome_as_expected),
            n,
            self.count(|t| t.validated_first_shot),
            self.count(|t| t.accepted && t.patches > 0),
            self.tasks.iter().map(|t| t.checks_passed).sum::<usize>(),
            self.tasks.iter().map(|t| t.checks_total).sum::<usize>(),
        ));
        // Per-gate breakdown: which gate first rejected each program, and whether
        // the closed loop mechanically converged it. This is the v0.2 headline —
        // the loop now converges structural gates (effect, region), not just
        // parse — so it is reported explicitly.
        out.push_str(
            "\n### First-shot failures by gate\n\n\
             **Converged** = the loop mechanically repaired the program to \
             acceptance; **rejected✓** = a deliberately-wrong spec the loop \
             correctly refused (true negative).\n\n\
             | Gate | first-shot fails | converged | rejected✓ |\n\
             |------|:----------------:|:---------:|:---------:|\n",
        );
        for gate in ["parse", "type", "effect", "region", "contract", "intent"] {
            let fails = self.count(|t| t.first_shot_gate == gate);
            if fails == 0 {
                continue;
            }
            let converged =
                self.count(|t| t.first_shot_gate == gate && t.accepted && t.expect_accept && t.patches > 0);
            let rejected_ok =
                self.count(|t| t.first_shot_gate == gate && !t.expect_accept && !t.accepted);
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                gate, fails, converged, rejected_ok
            ));
        }
        out
    }

    /// Same data as CSV for downstream analysis.
    pub fn to_csv(&self) -> String {
        let mut out = String::from(
            "task,parsed_first_shot,validated_first_shot,first_shot_gate,intent_first_shot,\
             accepted,patches,expect_accept,outcome_as_expected,checks_passed,checks_total\n",
        );
        for t in &self.tasks {
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{}\n",
                t.name,
                t.parsed_first_shot,
                t.validated_first_shot,
                if t.first_shot_gate.is_empty() { "-" } else { &t.first_shot_gate },
                t.intent_first_shot,
                t.accepted,
                t.patches,
                t.expect_accept,
                t.outcome_as_expected,
                t.checks_passed,
                t.checks_total,
            ));
        }
        out
    }
}
