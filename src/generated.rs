//! Track D — replay + scoring for the generation-reliability baseline.
//!
//! `eval/record/generate.py` records, OFFLINE, `k` model generations per task in
//! Aury and in Python (raw outputs, failures included). This module *replays*
//! those committed fixtures deterministically: each Aury generation runs through
//! the closed repair loop (parse → validate → repair → accept) and then the
//! task's oracle checks; each Python generation runs first-shot in a hermetic
//! `python3` subprocess against the same oracle inputs. It emits an Aury-vs-Python
//! comparison so the number is reproducible even though generation was not.
//!
//! The thesis lives in the final row: does Aury's structured rejection + mechanical
//! repair (first-shot *plus* repaired-to-accept, then oracle-correct) beat Python's
//! first-shot oracle-correctness on a matched, language-neutral task set?

use crate::interp::Interp;
use serde_json::Value;
use std::path::Path;

/// One task's matched signature + oracle checks (from `eval/record/tasks.json`).
struct Task {
    name: String,
    checks: Vec<Check>,
}

struct Check {
    args: Vec<String>,
    expect: String,
}

/// Aggregate score for one language side, summed over all generations.
#[derive(Default, Clone)]
pub struct LangScore {
    pub total: usize,
    /// Compiled/checked with zero repairs (Aury); for Python this equals `final_correct`.
    pub first_shot_valid: usize,
    /// Aury only: accepted after ≥1 mechanical repair.
    pub converged: usize,
    /// Passed every oracle check (Aury: after the loop; Python: first-shot).
    pub final_correct: usize,
}

pub struct GenReport {
    pub model: String,
    pub date: String,
    pub k: usize,
    pub aury: LangScore,
    pub python: LangScore,
    pub python_available: bool,
    pub per_task: Vec<(String, LangScore, LangScore)>,
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    serde_json::from_str(&text).map_err(|e| format!("parse {}: {}", path.display(), e))
}

fn load_tasks(manifest_dir: &Path) -> Result<Vec<Task>, String> {
    // tasks.json lives next to generate.py; the run dir is eval/generated/<...>,
    // so tasks.json is at ../../record/tasks.json relative to the run dir.
    let tasks_path = manifest_dir
        .join("..")
        .join("..")
        .join("record")
        .join("tasks.json");
    let v = read_json(&tasks_path)?;
    let arr = v["tasks"].as_array().ok_or("tasks.json: no `tasks` array")?;
    let mut tasks = Vec::new();
    for t in arr {
        let name = t["name"].as_str().ok_or("task without name")?.to_string();
        let mut checks = Vec::new();
        for c in t["checks"].as_array().ok_or("task without checks")? {
            let args = c["args"]
                .as_array()
                .ok_or("check without args")?
                .iter()
                .map(|a| a.as_str().unwrap_or("").to_string())
                .collect();
            let expect = c["expect"].as_str().ok_or("check without expect")?.to_string();
            checks.push(Check { args, expect });
        }
        tasks.push(Task { name, checks });
    }
    Ok(tasks)
}

/// Does `python3` exist? Scoring stays hermetic — the Python side is simply
/// skipped (and reported as such) when no interpreter is present.
fn python_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run the Aury oracle checks against an accepted program source. Returns true
/// only if every check produces the expected value.
fn aury_oracle_ok(source: &str, task: &Task, seed: u64) -> bool {
    let xs = match crate::sexpr::parse(source) {
        Ok(xs) if xs.len() == 1 => xs,
        _ => return false,
    };
    let module = match crate::ast::build_module(&xs[0]) {
        Ok(m) => m,
        Err(_) => return false,
    };
    for check in &task.checks {
        let function = module.items.iter().find_map(|item| match item {
            crate::ast::ModuleItem::Fn(f) if f.name == task.name => Some(f),
            _ => None,
        });
        let Some(function) = function else { return false };
        if function.params.len() != check.args.len() {
            return false;
        }
        let values: Result<Vec<_>, _> = function
            .params
            .iter()
            .zip(&check.args)
            .map(|(p, a)| crate::value_io::parse_cli_value(&module, &p.ty, a))
            .collect();
        let Ok(values) = values else { return false };
        match Interp::new(&module, seed).call_fn(&task.name, values) {
            Ok(v) if crate::value_io::show_value(&v) == check.expect => {}
            _ => return false,
        }
    }
    true
}

/// Score one recorded Aury generation: (first_shot_valid, converged, final_correct).
fn score_aury(source: &str, task: &Task, seed: u64) -> (bool, bool, bool) {
    let res = crate::repair_loop(source, false, seed);
    let first_shot = res.accepted && res.patches_applied == 0;
    let converged = res.accepted && res.patches_applied > 0;
    let correct = res.accepted && aury_oracle_ok(&res.source, task, seed);
    (first_shot, converged, correct)
}

/// Score one recorded Python generation first-shot: does it pass every oracle
/// check in a fresh `python3` subprocess? No repair round (first-shot only).
fn score_python(source: &str, task: &Task) -> bool {
    for check in &task.checks {
        // exec the generated source, then call the target function with the
        // oracle args and print the result — captured and compared verbatim.
        let call_args = check.args.join(", ");
        let script = format!(
            "import sys\nns = {{}}\nexec(_SRC, ns)\nprint(ns['{}']({}))",
            task.name, call_args
        );
        // Pass the source via env to avoid quoting issues; read it in the driver.
        let full = format!("_SRC = __import__('os').environ['AURY_GEN_SRC']\n{}", script);
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(&full)
            .env("AURY_GEN_SRC", source)
            .output();
        let ok = match output {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).trim() == check.expect
            }
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

fn read_gen(dir: &Path, task: &str, lang: &str, i: usize, ext: &str) -> Option<String> {
    let path = dir.join(task).join(lang).join(format!("{}.{}", i, ext));
    std::fs::read_to_string(path).ok()
}

pub fn run(dir: &Path, seed: u64) -> Result<GenReport, String> {
    let manifest = read_json(&dir.join("manifest.json"))?;
    let model = manifest["model"].as_str().unwrap_or("unknown").to_string();
    let date = manifest["date"].as_str().unwrap_or("unknown").to_string();
    let k = manifest["k"].as_u64().unwrap_or(0) as usize;
    let tasks = load_tasks(dir)?;
    let py_ok = python_available();

    let mut aury = LangScore::default();
    let mut python = LangScore::default();
    let mut per_task = Vec::new();

    for task in &tasks {
        let mut a = LangScore::default();
        let mut p = LangScore::default();
        for i in 0..k {
            if let Some(src) = read_gen(dir, &task.name, "aury", i, "aury") {
                a.total += 1;
                let (fs, conv, correct) = score_aury(&src, task, seed);
                if fs {
                    a.first_shot_valid += 1;
                }
                if conv {
                    a.converged += 1;
                }
                if correct {
                    a.final_correct += 1;
                }
            }
            if let Some(src) = read_gen(dir, &task.name, "python", i, "py") {
                p.total += 1;
                if py_ok && score_python(&src, task) {
                    p.first_shot_valid += 1;
                    p.final_correct += 1;
                }
            }
        }
        aury.total += a.total;
        aury.first_shot_valid += a.first_shot_valid;
        aury.converged += a.converged;
        aury.final_correct += a.final_correct;
        python.total += p.total;
        python.first_shot_valid += p.first_shot_valid;
        python.final_correct += p.final_correct;
        per_task.push((task.name.clone(), a, p));
    }

    Ok(GenReport {
        model,
        date,
        k,
        aury,
        python,
        python_available: py_ok,
        per_task,
    })
}

impl GenReport {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("## Generation-reliability baseline — Aury vs Python\n\n");
        out.push_str(&format!(
            "Recorded model **{}** on **{}**, {} generation(s) per task per language. \
             Generation is non-hermetic (a one-time offline model run); **scoring below \
             is deterministic** — a replay of the committed fixtures through the gates.\n\n",
            self.model, self.date, self.k
        ));
        let n = self.aury.total;
        let pn = self.python.total;
        out.push_str("| Metric | Aury | Python |\n|------|:----:|:----:|\n");
        out.push_str(&format!(
            "| First-shot valid (compiles/checks, 0 fixes) | {}/{} | {} |\n",
            self.aury.first_shot_valid,
            n,
            if self.python_available {
                format!("{}/{}", self.python.first_shot_valid, pn)
            } else {
                "— (no python3)".into()
            }
        ));
        out.push_str(&format!(
            "| Converged (accepted after ≥1 mechanical repair) | {}/{} | — |\n",
            self.aury.converged, n
        ));
        out.push_str(&format!(
            "| **Final oracle-correct** | **{}/{}** | **{}** |\n",
            self.aury.final_correct,
            n,
            if self.python_available {
                format!("{}/{}", self.python.final_correct, pn)
            } else {
                "— (no python3)".into()
            }
        ));
        out.push_str("\n### Per task (Aury first-shot / converged / correct · Python correct)\n\n");
        out.push_str("| Task | Aury fs | Aury conv | Aury ok | Python ok |\n");
        out.push_str("|------|:------:|:--------:|:------:|:--------:|\n");
        for (name, a, p) in &self.per_task {
            if a.total == 0 && p.total == 0 {
                continue; // task had no recorded generations in this run
            }
            out.push_str(&format!(
                "| {} | {}/{} | {}/{} | {}/{} | {} |\n",
                name,
                a.first_shot_valid,
                a.total,
                a.converged,
                a.total,
                a.final_correct,
                a.total,
                if self.python_available {
                    format!("{}/{}", p.final_correct, p.total)
                } else {
                    "—".into()
                }
            ));
        }
        out.push_str("\n### Threats to validity\n\n");
        out.push_str(&format!(
            "- **Small sample, single model/date.** n = {} Aury and {} Python generations, \
             model `{}`, {}. Read as evidence with provenance, not a universal rate.\n",
            n, pn, self.model, self.date
        ));
        out.push_str(
            "- **Familiarity bias favors Python.** Models have seen vast amounts of Python and \
             almost no Aury, so this is a *conservative* test for Aury — a win despite the bias is \
             a strong signal; a loss is honest.\n",
        );
        out.push_str(
            "- **The repair loop is Aury's treatment.** Python is scored first-shot only (no \
             feedback round), isolating structured mechanical repair as the intervention.\n",
        );
        if !self.python_available {
            out.push_str(
                "- **Python side skipped:** no `python3` on PATH at scoring time; re-run where it is present.\n",
            );
        }
        out
    }

    /// Deterministic one-line summary (Aury side, always reproducible).
    pub fn summary(&self) -> String {
        format!(
            "aury: first-shot {}/{}, converged {}, final-correct {} · python final-correct {} (available={})",
            self.aury.first_shot_valid,
            self.aury.total,
            self.aury.converged,
            self.aury.final_correct,
            self.python.final_correct,
            self.python_available
        )
    }
}
