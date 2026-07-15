//! The repair-loop driver. Given a (program, intent) pair, runs the closed
//! loop:
//!
//! The loop is: generate, validate, on rejection apply the lowest-cost
//! admissible repair, re-validate, and accept or regenerate on budget
//! exhaustion.
//!
//! In v0 the "generate" step is *external* (an LLM emits Aury source).
//! This driver operates on already-generated source: it validates, and when
//! rejection occurs it applies the lowest-cost admissible repair automatically
//! and re-validates, up to a per-node and per-program budget. On budget
//! exhaustion it asks the caller to regenerate.
//!
//! This is what makes the loop *automatic* rather than "show the model a type
//! error and hope": the validator proposes checked repairs, and this driver
//! applies them mechanically and re-checks.

use crate::ast::{build_module, Module};
use crate::repair::{Rejection, ValidationOutcome};
use crate::sexpr::{parse, Sexpr};
use crate::spec::{failure_to_rejection, run_property_tests};
use crate::validate::check_module;

/// Per-node repair budget. If a node is rejected with the same kind twice,
/// we escalate to regeneration.
pub const NODE_BUDGET: u32 = 3;
/// Per-program repair budget. Total patches before we ask to regenerate.
pub const PROGRAM_BUDGET: u32 = 20;

pub struct LoopResult {
    /// Final accepted source (if any), patched in place.
    pub source: String,
    pub accepted: bool,
    /// How many patches were applied.
    pub patches_applied: u32,
    /// Final remaining rejections (empty if accepted).
    pub remaining: Vec<Rejection>,
    /// Whether we hit the program budget and recommend regeneration.
    pub recommend_regenerate: bool,
    pub log: Vec<String>,
}

/// Run the repair loop on a source string. If `run_tests` is true, also run
/// property tests as part of acceptance.
pub fn repair_loop(source: &str, run_tests: bool, seed: u64) -> LoopResult {
    let mut log = Vec::new();
    let mut current = source.to_string();
    let mut patches_applied = 0u32;
    let mut per_node: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

    loop {
        if patches_applied >= PROGRAM_BUDGET {
            log.push(format!(
                "program repair budget ({}) exhausted — recommend regeneration",
                PROGRAM_BUDGET
            ));
            let _rem = final_rejections(&current, run_tests, seed);
            return LoopResult {
                source: current,
                accepted: false,
                patches_applied,
                remaining: _rem,
                recommend_regenerate: true,
                log,
            };
        }
        let module = match build(&current) {
            Ok(m) => m,
            Err(e) => {
                // Parse gate: the most common authoring error is an
                // unterminated list (forgot to close nested forms). If the
                // source has unmatched `(`, append the deficit and retry — an
                // admissible mechanical repair that brings parse errors inside
                // the generate→validate→repair loop instead of outside it.
                if e.contains("parse error") || e.contains("unterminated") {
                    let deficit = crate::sexpr::paren_deficit(&current);
                    if deficit > 0 {
                        let fixed = current.to_string() + &")".repeat(deficit);
                        patches_applied += 1;
                        log.push(format!(
                            "parse repair: appended {} closing paren(s)",
                            deficit
                        ));
                        current = fixed;
                        continue;
                    }
                }
                log.push(format!("parse/build error: {}", e));
                return LoopResult {
                    source: current,
                    accepted: false,
                    patches_applied,
                    remaining: vec![],
                    recommend_regenerate: true,
                    log,
                };
            }
        };
        let outcome = check_module(&module);
        match outcome {
            ValidationOutcome::Accepted => {
                if run_tests {
                    let failures = run_property_tests(&module, seed, 64);
                    if failures.is_empty() {
                        log.push("accepted: type/effect/region checks pass; property tests pass".into());
                        return LoopResult {
                            source: current,
                            accepted: true,
                            patches_applied,
                            remaining: vec![],
                            recommend_regenerate: false,
                            log,
                        };
                    } else {
                        let rejs: Vec<Rejection> =
                            failures.iter().map(failure_to_rejection).collect();
                        log.push(format!(
                            "property test failures: {} — feeding back as repair signal",
                            rejs.len()
                        ));
                        // Property-test failures are NOT mechanically patchable
                        // in v0 (they require the model to decide impl-vs-spec).
                        // We surface them and stop.
                        return LoopResult {
                            source: current,
                            accepted: false,
                            patches_applied,
                            remaining: rejs,
                            recommend_regenerate: true,
                            log,
                        };
                    }
                }
                log.push("accepted: type/effect/region checks pass".into());
                return LoopResult {
                    source: current,
                    accepted: true,
                    patches_applied,
                    remaining: vec![],
                    recommend_regenerate: false,
                    log,
                };
            }
            ValidationOutcome::Rejected(rejections) => {
                // Try to apply the lowest-cost admissible repair for the first
                // rejection whose repair menu is non-empty.
                let Some((rej_idx, repair_idx)) =
                    pick_repair(&rejections, &mut per_node)
                else {
                    log.push(
                        "rejections present but none carry a mechanically \
                         applicable repair — recommend regeneration"
                            .to_string(),
                    );
                    return LoopResult {
                        source: current,
                        accepted: false,
                        patches_applied,
                        remaining: rejections,
                        recommend_regenerate: true,
                        log,
                    };
                };
                let rej = &rejections[rej_idx];
                let repair = &rej.repairs[repair_idx];
                // Cycle detection: if this node has already been patched with
                // this kind, escalate.
                let key = format!("{}:{}", rej.node, repair.action);
                let count = per_node.entry(key.clone()).or_insert(0);
                *count += 1;
                if *count > NODE_BUDGET {
                    log.push(format!(
                        "node {} repeatedly patched by `{}` — recommend regeneration",
                        rej.node, repair.action
                    ));
                    return LoopResult {
                        source: current,
                        accepted: false,
                        patches_applied,
                        remaining: rejections,
                        recommend_regenerate: true,
                        log,
                    };
                }
                // Apply the repair to the source. v0 supports a small set of
                // mechanical repairs (wrap, replace_node, insert_copy).
                match apply_repair(&current, rej, repair) {
                    Ok(new_source) => {
                        patches_applied += 1;
                        log.push(format!(
                            "applied repair `{}` (action={}) to node {}; patch #{}, re-validating",
                            repair.id, repair.action, rej.node, patches_applied
                        ));
                        current = new_source;
                    }
                    Err(e) => {
                        log.push(format!(
                            "could not apply repair `{}` automatically: {} — recommend regeneration",
                            repair.action, e
                        ));
                        return LoopResult {
                            source: current,
                            accepted: false,
                            patches_applied,
                            remaining: rejections,
                            recommend_regenerate: true,
                            log,
                        };
                    }
                }
            }
        }
    }
}

fn build(source: &str) -> Result<Module, String> {
    let xs = parse(source).map_err(|e| e.to_string())?;
    if xs.len() != 1 {
        return Err("expected exactly one top-level (module ...) form".into());
    }
    build_module(&xs[0])
}

/// Pick the (rejection, repair) pair with the lowest repair cost. Skips
/// repairs that have already exhausted their node budget.
fn pick_repair(
    rejections: &[Rejection],
    per_node: &mut std::collections::HashMap<String, u32>,
) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize, u32)> = None;
    for (ri, rej) in rejections.iter().enumerate() {
        for (si, r) in rej.repairs.iter().enumerate() {
            let key = format!("{}:{}", rej.node, r.action);
            let used = *per_node.get(&key).unwrap_or(&0);
            if used >= NODE_BUDGET {
                continue;
            }
            match best {
                None => best = Some((ri, si, r.cost)),
                Some((_, _, cost)) if r.cost < cost => best = Some((ri, si, r.cost)),
                _ => {}
            }
        }
    }
    best.map(|(ri, si, _)| (ri, si))
}

/// Apply a mechanical repair to the source text. This is the "patching
/// happens on nodes" step. v0 implements the simplest, deterministic patch:
/// we re-parse, find the node by id, and substitute the repair's `with`
/// s-expression in place of the matched node, then re-serialize.
fn apply_repair(
    source: &str,
    rej: &Rejection,
    repair: &crate::repair::Repair,
) -> Result<String, String> {
    let with_template = repair
        .with
        .clone()
        .ok_or_else(|| format!("repair `{}` has no `with`", repair.action))?;
    let xs = parse(source).map_err(|e| e.to_string())?;
    if xs.len() != 1 {
        return Err("expected one top-level form".into());
    }
    let mut root = xs[0].clone();
    let target = rej.node.to_string();
    if let Some(original) = find_node_by_id(&root, &target) {
        // Substitute every `?` placeholder in the repair template with the
        // original node, then replace the original with the result. For
        // `replace_node` repairs (no `?`), this just replaces wholesale.
        let patched = substitute_placeholder(&with_template, original);
        if replace_node_by_id(&mut root, &target, &patched) {
            return Ok(format!("{:?}", root));
        }
    }
    Err(format!("could not locate node {} for patch", rej.node))
}

/// Find the first sub-s-expr whose content-addressed id matches `target_id`.
fn find_node_by_id<'a>(s: &'a Sexpr, target_id: &str) -> Option<&'a Sexpr> {
    if crate::id::sexpr_id(s).hex() == target_id {
        return Some(s);
    }
    if let Sexpr::List(xs) = s {
        for x in xs {
            if let Some(found) = find_node_by_id(x, target_id) {
                return Some(found);
            }
        }
    }
    None
}

/// Replace the first sub-s-expr whose id matches `target_id` with `replacement`.
fn replace_node_by_id(s: &mut Sexpr, target_id: &str, replacement: &Sexpr) -> bool {
    if crate::id::sexpr_id(s).hex() == target_id {
        *s = replacement.clone();
        return true;
    }
    if let Sexpr::List(xs) = s {
        for x in xs.iter_mut() {
            if replace_node_by_id(x, target_id, replacement) {
                return true;
            }
        }
    }
    false
}

/// Substitute every `?` atom placeholder in `template` with a clone of `original`.
fn substitute_placeholder(template: &Sexpr, original: &Sexpr) -> Sexpr {
    match template {
        Sexpr::Atom(a) if a == "?" => original.clone(),
        Sexpr::Atom(a) => Sexpr::Atom(a.clone()),
        Sexpr::List(xs) => Sexpr::List(
            xs.iter()
                .map(|x| substitute_placeholder(x, original))
                .collect(),
        ),
    }
}

fn final_rejections(source: &str, run_tests: bool, seed: u64) -> Vec<Rejection> {
    let module = match build(source) {
        Ok(m) => m,
        Err(_) => return vec![],
    };
    let mut rejs = match check_module(&module) {
        ValidationOutcome::Accepted => vec![],
        ValidationOutcome::Rejected(r) => r,
    };
    if run_tests {
        let failures = run_property_tests(&module, seed, 64);
        rejs.extend(failures.iter().map(failure_to_rejection));
    }
    rejs
}