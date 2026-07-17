//! Intent verification: contracts + property tests + vacuity check + shrinking.
//!
//! The validator proves a program is well-formed. It does *not* prove the
//! program does what the user wants. This module is the gate that actually
//! matters for intent: the AI generates specs alongside impl, the harness runs
//! them, and failure is a repair signal just like a type error.
//!
//! Three layers (per the proposal):
//!   1. Contracts (pre/post) — checked by executing them as assertions.
//!   2. Property tests — QuickCheck-style random testing with shrinking.
//!   3. Vacuity check — a spec the program cannot fail is worthless; the
//!      harness detects and rejects trivially-true properties.
//!
//! What this does *not* solve: it does not prove the spec matches user
//! intent. The user is still the final oracle. But it shrinks the gap to
//! "the code passes its own non-vacuous, regression-stable spec."

use crate::ast::*;
use crate::interp::{Interp, Value};
use crate::id::NodeId;
use crate::repair::{Gate, Rejection, Repair};
use crate::types::Type;
use std::collections::HashMap;

/// Run all property tests in a module. Returns a list of failures (each
/// becomes a rejection fed back to the model).
pub fn run_property_tests(
    module: &Module,
    seed: u64,
    cases: usize,
) -> Vec<PropertyFailure> {
    let mut failures = Vec::new();
    for item in &module.items {
        if let ModuleItem::Spec(spec) = item {
            for prop in &spec.properties {
                let r = run_one_property(module, prop, seed, cases);
                if let Some(f) = r {
                    failures.push(f);
                }
            }
        }
    }
    failures
}

pub struct PropertyFailure {
    pub property_name: String,
    pub property_id: NodeId,
    /// The shrunk counterexample inputs, named.
    pub counterexample: Vec<(String, Value)>,
    /// Whether the property was vacuous (trivially true).
    pub vacuous: bool,
    /// The property body source (for the rejection's note).
    pub body_debug: String,
}

/// Run a single property. Returns None if it passes (and is non-vacuous).
fn run_one_property(
    module: &Module,
    prop: &Property,
    seed: u64,
    cases: usize,
) -> Option<PropertyFailure> {
    let mut interp = Interp::new(module, seed);
    let mut failing: Option<Vec<Value>> = None;
    let mut rng = Rng::new(seed ^ 0xC0FFEE);

    // Vacuity check (sound, no false positives on correct implementations):
    // a property is vacuous iff it does not actually *exercise* any of the
    // module's functions — i.e., its body references no user-defined fn. A
    // property that calls the function under test is meaningful even when the
    // implementation happens to satisfy it for all inputs; that is
    // correctness, not vacuity. The random "can it fail" probe can't tell
    // those apart, so we don't use it.
    let user_fns: std::collections::HashSet<String> = module
        .items
        .iter()
        .filter_map(|it| match it {
            ModuleItem::Fn(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect();
    let vacuous = !body_references_user_fn(&prop.body, &user_fns);
    if vacuous {
        return Some(PropertyFailure {
            property_name: prop.name.clone(),
            property_id: prop.id,
            counterexample: vec![],
            vacuous: true,
            body_debug: format!("{:?}", prop.body),
        });
    }

    for _ in 0..cases {
        let vals: Vec<Value> = prop
            .forall
            .iter()
            .map(|(_, t)| rng.gen_value(t))
            .collect();
        let result = eval_property_body(&mut interp, &prop.body, &prop.forall, &vals);
        match result {
            Ok(true) => {}
            Ok(false) => {
                let shrunk = shrink(&mut interp, &prop.body, &prop.forall, &vals);
                failing = Some(shrunk);
                break;
            }
            Err(_) => {
                // A runtime error during property testing counts as a failure.
                let shrunk = shrink(&mut interp, &prop.body, &prop.forall, &vals);
                failing = Some(shrunk);
                break;
            }
        }
    }
    if let Some(vals) = failing {
        let names: Vec<String> = prop.forall.iter().map(|(n, _)| n.clone()).collect();
        let cx = names.into_iter().zip(vals.into_iter()).collect();
        return Some(PropertyFailure {
            property_name: prop.name.clone(),
            property_id: prop.id,
            counterexample: cx,
            vacuous: false,
            body_debug: format!("{:?}", prop.body),
        });
    }
    None
}

// ===========================================================================
// Contract testing: preconditions/postconditions as an executable intent gate.
//
// Contracts are enforced at runtime by the interpreter (see
// `Interp::invoke_fn`), so they hold on every concrete execution. This harness
// *actively* exercises them: for each contracted function it generates inputs,
// keeps those satisfying the preconditions, runs the function, and reports any
// postcondition violation (or trap) as a shrunk counterexample — exactly like a
// property failure, so it re-enters the repair loop the same way.
// ===========================================================================

pub struct ContractFailure {
    pub fn_name: String,
    pub fn_id: NodeId,
    /// The shrunk counterexample inputs, named. Empty for a vacuity failure or
    /// a zero-argument function.
    pub counterexample: Vec<(String, Value)>,
    /// True when the postcondition is vacuous (never mentions `result`).
    pub vacuous: bool,
    /// Human-readable detail (the trap message, or the vacuity explanation).
    pub detail: String,
}

/// Run contract tests for every function that carries an `ensures` clause.
pub fn run_contract_tests(module: &Module, seed: u64, cases: usize) -> Vec<ContractFailure> {
    let mut failures = Vec::new();
    for item in &module.items {
        let ModuleItem::Fn(f) = item else { continue };
        if f.ensures.is_empty() {
            continue;
        }
        if let Some(fail) = run_one_contract(module, f, seed, cases) {
            failures.push(fail);
        }
    }
    failures
}

fn run_one_contract(
    module: &Module,
    f: &FnDef,
    seed: u64,
    cases: usize,
) -> Option<ContractFailure> {
    // Vacuity: a postcondition that never inspects `result` is asserting
    // something about the inputs alone — it cannot constrain the function's
    // output, so it is worthless as a spec. (Mirrors the property vacuity gate.)
    if !f.ensures.iter().any(|e| expr_mentions(e, RESULT_BINDING)) {
        return Some(ContractFailure {
            fn_name: f.name.clone(),
            fn_id: f.id,
            counterexample: vec![],
            vacuous: true,
            detail: "no `ensures` clause references `result`".into(),
        });
    }
    // We can only generate inputs for functions whose parameters are all
    // generatable scalars/vectors. Others are skipped (not a failure).
    if !f.params.iter().all(|p| is_generatable(&p.ty)) {
        return None;
    }
    let forall: Vec<(String, Type)> = f.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect();

    let mut interp = Interp::new(module, seed);
    let mut rng = Rng::new(seed ^ 0xC02417AC7);

    let iters = if f.params.is_empty() { 1 } else { cases };
    for _ in 0..iters {
        let vals: Vec<Value> = f.params.iter().map(|p| rng.gen_value(&p.ty)).collect();
        if !preconditions_hold(&mut interp, f, &forall, &vals) {
            continue; // input is out of the function's domain
        }
        if interp.call_fn(&f.name, vals.clone()).is_err() {
            let shrunk = shrink_contract(&mut interp, f, &forall, &vals);
            let detail = match interp.call_fn(&f.name, shrunk.clone()) {
                Err(e) => e.0,
                Ok(_) => "contract violated".into(),
            };
            let cx = forall
                .iter()
                .map(|(n, _)| n.clone())
                .zip(shrunk.into_iter())
                .collect();
            return Some(ContractFailure {
                fn_name: f.name.clone(),
                fn_id: f.id,
                counterexample: cx,
                vacuous: false,
                detail,
            });
        }
    }
    None
}

/// True iff every precondition holds for these inputs (input is in-domain).
fn preconditions_hold(interp: &mut Interp, f: &FnDef, forall: &[(String, Type)], vals: &[Value]) -> bool {
    f.requires
        .iter()
        .all(|r| matches!(eval_property_body(interp, r, forall, vals), Ok(true)))
}

/// A contract "fails" for an in-domain input iff the preconditions hold and the
/// call traps (postcondition violation is a trap in the interpreter).
fn contract_fails(interp: &mut Interp, f: &FnDef, forall: &[(String, Type)], vals: &[Value]) -> bool {
    preconditions_hold(interp, f, forall, vals) && interp.call_fn(&f.name, vals.to_vec()).is_err()
}

/// Shrink a failing contract counterexample, keeping it in-domain and failing.
fn shrink_contract(interp: &mut Interp, f: &FnDef, forall: &[(String, Type)], vals: &[Value]) -> Vec<Value> {
    let mut current = vals.to_vec();
    for _ in 0..256 {
        let mut changed = false;
        for i in 0..current.len() {
            for c in shrink_one(&current[i]) {
                if value_eq(&c, &current[i]) {
                    continue;
                }
                let mut trial = current.clone();
                trial[i] = c;
                if contract_fails(interp, f, forall, &trial) {
                    current = trial;
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            break;
        }
    }
    current
}

fn is_generatable(ty: &Type) -> bool {
    match ty {
        Type::I64 | Type::Bool | Type::Str => true,
        Type::Vec(inner) => is_generatable(inner),
        _ => false,
    }
}

/// Convert a contract failure into a structured rejection for the model.
pub fn contract_failure_to_rejection(f: &ContractFailure) -> Rejection {
    let cx_repr = if f.counterexample.is_empty() {
        "(none)".into()
    } else {
        f.counterexample
            .iter()
            .map(|(n, v)| format!("{} = {}", n, show_value(v)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Rejection {
        gate: Gate::Contract,
        kind: if f.vacuous {
            "VACUOUS_CONTRACT".into()
        } else {
            "POSTCONDITION_FALSIFIED".into()
        },
        node: f.fn_id,
        path: f.fn_name.clone(),
        expected: if f.vacuous {
            "a postcondition that constrains `result`".into()
        } else {
            "the postcondition holds whenever the precondition does".into()
        },
        received: if f.vacuous {
            f.detail.clone()
        } else {
            format!("falsified for: {} ({})", cx_repr, f.detail)
        },
        context: {
            let mut m = HashMap::new();
            for (n, v) in &f.counterexample {
                m.insert(n.clone(), show_value(v));
            }
            m
        },
        repairs: vec![Repair {
            id: "cr1".into(),
            action: if f.vacuous {
                "strengthen_contract".into()
            } else {
                "fix_impl_or_contract".into()
            },
            with: None,
            cost: 4,
            preserves_effects: true,
            preserves_contracts: false,
            propagates: vec![],
            note: if f.vacuous {
                "The `ensures` clause never references `result`, so it says \
                 nothing about the function's output. Rewrite it as a predicate \
                 over `result` (and the parameters)."
                    .into()
            } else {
                format!(
                    "`{}` violates its postcondition for: {}. Either the \
                     implementation is wrong (fix the body) or the contract is \
                     wrong (fix the `ensures` clause). The counterexample is \
                     shrunk and in-domain (it satisfies every `requires`).",
                    f.fn_name, cx_repr
                )
            },
        }],
    }
}

/// Does an expression reference a variable named `name` anywhere?
fn expr_mentions(e: &Expr, name: &str) -> bool {
    match e {
        Expr::Ref { name: n, .. } => n == name,
        Expr::Lit { .. } => false,
        Expr::Let { init, body, .. } => expr_mentions(init, name) || expr_mentions(body, name),
        Expr::Call { args, .. } => args.iter().any(|a| expr_mentions(a, name)),
        Expr::If { cond, then, els, .. } => {
            expr_mentions(cond, name) || expr_mentions(then, name) || expr_mentions(els, name)
        }
        Expr::Match { scrut, arms, .. } => {
            expr_mentions(scrut, name) || arms.iter().any(|a| expr_mentions(&a.body, name))
        }
        Expr::Loop { body, .. } => expr_mentions(body, name),
        Expr::Break { value, .. } => expr_mentions(value, name),
        Expr::Set { name: n, value, .. } => n == name || expr_mentions(value, name),
        Expr::Return { value, .. } => expr_mentions(value, name),
        Expr::Block { stmts, tail, .. } => {
            stmts.iter().any(|s| expr_mentions(s, name)) || expr_mentions(tail, name)
        }
        Expr::Region { body, .. } => expr_mentions(body, name),
        Expr::Copy { value, .. } => expr_mentions(value, name),
        Expr::VecNew { elems, .. } => elems.iter().any(|e| expr_mentions(e, name)),
        Expr::Index { target, index, .. } => expr_mentions(target, name) || expr_mentions(index, name),
        Expr::Len { target, .. } => expr_mentions(target, name),
        Expr::StructNew { fields, .. } => fields.iter().any(|(_, v)| expr_mentions(v, name)),
        Expr::Field { target, .. } => expr_mentions(target, name),
        Expr::Cast { value, .. } => expr_mentions(value, name),
    }
}

/// Does the property body call any user-defined function? If not, the
/// property doesn't exercise any implementation and is vacuous.
fn body_references_user_fn(e: &Expr, fns: &std::collections::HashSet<String>) -> bool {
    match e {
        Expr::Call { op, args, .. } => {
            if fns.contains(op) {
                return true;
            }
            args.iter().any(|a| body_references_user_fn(a, fns))
        }
        Expr::Let { init, body, .. } => {
            body_references_user_fn(init, fns) || body_references_user_fn(body, fns)
        }
        Expr::If { cond, then, els, .. } => {
            body_references_user_fn(cond, fns)
                || body_references_user_fn(then, fns)
                || body_references_user_fn(els, fns)
        }
        Expr::Match { scrut, arms, .. } => {
            body_references_user_fn(scrut, fns)
                || arms.iter().any(|a| body_references_user_fn(&a.body, fns))
        }
        Expr::Loop { body, .. } => body_references_user_fn(body, fns),
        Expr::Break { value, .. } => body_references_user_fn(value, fns),
        Expr::Set { value, .. } => body_references_user_fn(value, fns),
        Expr::Return { value, .. } => body_references_user_fn(value, fns),
        Expr::Block { stmts, tail, .. } => {
            stmts.iter().any(|s| body_references_user_fn(s, fns))
                || body_references_user_fn(tail, fns)
        }
        Expr::Region { body, .. } => body_references_user_fn(body, fns),
        Expr::Copy { value, .. } => body_references_user_fn(value, fns),
        Expr::VecNew { elems, .. } => elems.iter().any(|e| body_references_user_fn(e, fns)),
        Expr::Index { target, index, .. } => {
            body_references_user_fn(target, fns) || body_references_user_fn(index, fns)
        }
        Expr::Len { target, .. } => body_references_user_fn(target, fns),
        Expr::StructNew { fields, .. } => fields.iter().any(|(_, v)| body_references_user_fn(v, fns)),
        Expr::Field { target, .. } => body_references_user_fn(target, fns),
        Expr::Cast { value, .. } => body_references_user_fn(value, fns),
        Expr::Lit { .. } | Expr::Ref { .. } => false,
    }
}

/// Evaluate a property body that should be a boolean expression, given the
/// forall variables bound to values.
fn eval_property_body(
    interp: &mut Interp,
    body: &Expr,
    forall: &[(String, Type)],
    vals: &[Value],
) -> Result<bool, String> {
    // Evaluate the property body with the forall variables bound to `vals`.
    // We synthesize a wrapper fn whose *parameters* are exactly the forall
    // variables, so calling it with `vals` binds them into scope (the
    // interpreter's call_fn builds the scope from params+args).
    if vals.len() != forall.len() {
        return Err(format!("arity: {} vals vs {} forall", vals.len(), forall.len()));
    }
    let wrapper = FnDef {
        id: prop_id(forall),
        name: "__property".into(),
        params: forall
            .iter()
            .map(|(n, t)| Param {
                id: crate::id::IdBuilder::new("p").str(n).finish(),
                name: n.clone(),
                ty: t.clone(),
                is_cap: false,
            })
            .collect(),
        ret: Type::Bool,
        effects: crate::types::EffectRow::pure_row(),
        requires: vec![],
        ensures: vec![],
        body: body.clone(),
    };
    interp.fns.insert("__property".to_string(), wrapper);
    let r = interp.call_fn("__property", vals.to_vec());
    interp.fns.remove("__property");
    match r {
        Ok(Value::Bool(b)) => Ok(b),
        Ok(v) => Err(format!("property body not bool: {:?}", v)),
        Err(e) => Err(e.0),
    }
}

fn prop_id(forall: &[(String, Type)]) -> NodeId {
    crate::id::IdBuilder::new("property-wrapper")
        .str(&forall.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>().join(","))
        .finish()
}

/// QuickCheck-style shrinking: try to reduce each input toward a minimal
/// failing case. We try shrinking numbers toward zero, bools to false,
/// strings to "", vectors to empty.
fn shrink(
    interp: &mut Interp,
    body: &Expr,
    forall: &[(String, Type)],
    vals: &[Value],
) -> Vec<Value> {
    let mut current: Vec<Value> = vals.to_vec();
    // Hard cap so a pathological property can't loop forever.
    for _ in 0..256 {
        let mut changed = false;
        for i in 0..current.len() {
            let candidates = shrink_one(&current[i]);
            for c in candidates {
                // Skip no-op candidates (would loop forever).
                if value_eq(&c, &current[i]) {
                    continue;
                }
                let mut trial = current.clone();
                trial[i] = c.clone();
                let r = eval_property_body(interp, body, forall, &trial);
                if r.unwrap_or(false) == false {
                    current = trial;
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            break;
        }
    }
    current
}

fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::I64(x), Value::I64(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Unit, Value::Unit) => true,
        (Value::Vec(x), Value::Vec(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| value_eq(a, b)),
        _ => false,
    }
}

fn shrink_one(v: &Value) -> Vec<Value> {
    match v {
        Value::I64(n) => {
            let mut cs = vec![Value::I64(0)];
            if *n != 0 {
                // Every candidate must be strictly closer to zero, so shrinking
                // decreases magnitude monotonically and cannot oscillate. A
                // step of `n.signum()` moves one toward zero regardless of sign
                // (the old `n - 1` moved *away* from zero for negatives, which
                // left counterexamples non-minimal, e.g. -2 instead of -1).
                cs.push(Value::I64(n / 2));
                cs.push(Value::I64(n - n.signum()));
            }
            cs
        }
        Value::Bool(_) => vec![Value::Bool(false), Value::Bool(true)],
        Value::Str(s) => {
            let mut cs = vec![Value::Str(String::new())];
            if s.len() > 1 {
                cs.push(Value::Str(s[..s.len() / 2].to_string()));
            }
            cs
        }
        Value::Vec(vs) => {
            let mut cs = vec![Value::Vec(vec![])];
            if vs.len() > 1 {
                cs.push(Value::Vec(vs[..vs.len() - 1].to_vec()));
            }
            cs
        }
        _ => vec![],
    }
}

/// Adversarial probe: try extreme values to see if the property can fail at
/// all. Used for the vacuity check.






/// Convert a property failure into a structured rejection for the model.
pub fn failure_to_rejection(f: &PropertyFailure) -> Rejection {
    let cx_repr = if f.counterexample.is_empty() {
        "(none)".into()
    } else {
        f.counterexample
            .iter()
            .map(|(n, v)| format!("{} = {}", n, show_value(v)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Rejection {
        gate: Gate::PropertyTest,
        kind: if f.vacuous {
            "VACUOUS_PROPERTY".into()
        } else {
            "PROPERTY_FALSIFIED".into()
        },
        node: f.property_id,
        path: f.property_name.clone(),
        expected: "property holds for all inputs".into(),
        received: if f.vacuous {
            "property is trivially true (never fails)".into()
        } else {
            format!("falsified for: {}", cx_repr)
        },
        context: {
            let mut m = HashMap::new();
            for (n, v) in &f.counterexample {
                m.insert(n.clone(), show_value(v));
            }
            m
        },
        repairs: vec![Repair {
            id: "pr1".into(),
            action: if f.vacuous {
                "strengthen_property".into()
            } else {
                "fix_impl_or_spec".into()
            },
            with: None,
            cost: 4,
            preserves_effects: true,
            preserves_contracts: false,
            propagates: vec![],
            note: if f.vacuous {
                "The property never fails for any tested input — it may be \
                 trivially true. Strengthen it to actually exercise the \
                 implementation."
                    .into()
            } else {
                format!(
                    "The implementation fails the property for: {}. Either \
                     the implementation is wrong (fix the impl) or the spec \
                     is wrong (fix the property). The shrunk counterexample is \
                     minimal.",
                    cx_repr
                )
            },
        }],
    }
}

fn show_value(v: &Value) -> String {
    match v {
        Value::I64(n) => format!("{}i64", n),
        Value::Bool(b) => format!("{}", b),
        Value::Str(s) => format!("\"{}\"", s),
        Value::Unit => "unit".into(),
        Value::Vec(vs) => format!(
            "[{}]",
            vs.iter()
                .map(show_value)
                .collect::<Vec<_>>()
                .join(", ")
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

// ---- a small deterministic RNG for property-test input generation ----
struct Rng {
    state: u64,
}
impl Rng {
    fn new(seed: u64) -> Self {
        Rng {
            state: seed.wrapping_add(0x9E3779B97F4A7C15),
        }
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn gen_value(&mut self, t: &Type) -> Value {
        match t {
            Type::I64 => Value::I64((self.next_u64() as i64) % 200 - 100),
            Type::Bool => Value::Bool(self.next_u64() % 2 == 0),
            Type::Str => Value::Str(self.next_u64().to_string()),
            Type::Vec(inner) => {
                let n = (self.next_u64() % 4) as usize;
                Value::Vec((0..n).map(|_| self.gen_value(inner)).collect())
            }
            _ => Value::Unit,
        }
    }
    #[allow(dead_code)]
    fn gen_extreme(&mut self, t: &Type) -> Value {
        match t {
            Type::I64 => match self.next_u64() % 5 {
                0 => Value::I64(0),
                1 => Value::I64(-1),
                2 => Value::I64(i64::MAX),
                3 => Value::I64(i64::MIN),
                _ => Value::I64(1),
            },
            Type::Bool => Value::Bool(self.next_u64() % 2 == 0),
            Type::Str => Value::Str("!".into()),
            _ => self.gen_value(t),
        }
    }
}