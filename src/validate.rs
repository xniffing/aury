//! The validator. Checks types, effects, and regions with **no inference**,
//! emitting structured [`Rejection`]s with ranked admissible [`Repair`]es.
//!
//! Key property: every [`Repair`] the validator proposes names a replacement
//! it has already checked is locally valid (or it is a structural change
//! that is valid by construction). The model picks from a menu of known-good
//! fixes.

use crate::ast::*;
use crate::id::{sexpr_id, NodeId};
use crate::repair::*;
use crate::sexpr::Sexpr;
use crate::types::{EffectRow, Type};
use std::collections::{HashMap, HashSet};

/// A binding in scope during checking: its type, whether it's affine (owned),
/// whether it has been consumed (moved), and the region it lives in (if any).
#[derive(Clone, Debug)]
struct Binding {
    ty: Type,
    affine: bool,
    moved: bool,
    /// Region tracking is exercised in v1 (shared regions); reserved here.
    #[allow(dead_code)]
    region: Option<String>,
    is_cap: bool,
    /// True for function parameters (and `result`): values, not mutable
    /// locals, so `set` on them is rejected. `let` bindings are `false`.
    is_param: bool,
}

struct Checker {
    fns: HashMap<String, FnSig>,
    structs: HashMap<String, StructDef>,
    rejections: Vec<Rejection>,
    /// Track applied repairs per node so we can detect cycling.
    applied: HashMap<NodeId, u32>,
    /// The function's declared effect row (for effect checking against the
    /// body's actual effects).
    declared_effects: EffectRow,
    /// The node id of the enclosing `(fn ...)` form. Effect-row repairs target
    /// the whole function so the driver can widen/insert its `(effects ...)`
    /// clause mechanically.
    fn_id: NodeId,
    /// The function's parameter names → regions (for region checking).
    regions_in_scope: Vec<String>,
    /// A counter for repair ids.
    repair_counter: u32,
    /// Stack of enclosing loops' break value types (innermost last). `None`
    /// means no `break` seen yet for that loop; `Some(t)` records the agreed
    /// break type. A `break` outside any loop finds an empty stack.
    break_tys: Vec<Option<Type>>,
}

#[derive(Clone, Debug)]
struct FnSig {
    name: String,
    params: Vec<(String, Type, bool)>, // name, type, is_cap
    ret: Type,
    effects: EffectRow,
}

pub fn check_module(module: &Module) -> ValidationOutcome {
    let mut fns = HashMap::new();
    let mut structs = HashMap::new();
    let mut total = Vec::new();
    // First pass: collect signatures.
    for item in &module.items {
        match item {
            ModuleItem::Fn(f) => {
                if fns.contains_key(&f.name) {
                    total.push(Rejection {
                        gate: Gate::Type,
                        kind: "DUPLICATE_FUNCTION".into(),
                        node: f.id,
                        path: f.name.clone(),
                        expected: "a unique function name".into(),
                        received: format!("duplicate function `{}`", f.name),
                        context: HashMap::new(),
                        repairs: vec![],
                    });
                    continue;
                }
                fns.insert(
                    f.name.clone(),
                    FnSig {
                        name: f.name.clone(),
                        params: f.params.iter().map(|p| (p.name.clone(), p.ty.clone(), p.is_cap)).collect(),
                        ret: f.ret.clone(),
                        effects: f.effects.clone(),
                    },
                );
            }
            ModuleItem::Struct(s) => {
                if structs.contains_key(&s.name) {
                    total.push(Rejection {
                        gate: Gate::Type,
                        kind: "DUPLICATE_STRUCT".into(),
                        node: s.id,
                        path: s.name.clone(),
                        expected: "a unique struct type name".into(),
                        received: format!("duplicate struct `{}`", s.name),
                        context: HashMap::new(),
                        repairs: vec![],
                    });
                    continue;
                }
                let mut seen = HashSet::new();
                for (field, _) in &s.fields {
                    if !seen.insert(field) {
                        total.push(Rejection {
                            gate: Gate::Type,
                            kind: "DUPLICATE_FIELD".into(),
                            node: s.id,
                            path: format!("{}.{}", s.name, field),
                            expected: "each struct field exactly once".into(),
                            received: format!("duplicate field `{}`", field),
                            context: HashMap::new(),
                            repairs: vec![],
                        });
                    }
                }
                structs.insert(s.name.clone(), s.clone());
            }
            _ => {}
        }
    }
    // Second pass: check each function body.
    for item in &module.items {
        if let ModuleItem::Fn(f) = item {
            let mut c = Checker {
                fns: fns.clone(),
                structs: structs.clone(),
                rejections: Vec::new(),
                applied: HashMap::new(),
                declared_effects: f.effects.clone(),
                fn_id: f.id,
                regions_in_scope: Vec::new(),
                repair_counter: 0,
                break_tys: Vec::new(),
            };
            let mut scope: HashMap<String, Binding> = HashMap::new();
            for p in &f.params {
                let region = region_of(&p.ty).map(|s| s.to_string());
                if let Some(r) = &region {
                    c.regions_in_scope.push(r.clone());
                }
                scope.insert(
                    p.name.clone(),
                    Binding {
                        ty: p.ty.clone(),
                        affine: is_affine(&p.ty),
                        moved: false,
                        region,
                        is_cap: p.is_cap,
                        is_param: true,
                    },
                );
            }
            let body_ty = c.check_expr(&f.body, &mut scope, Some(&f.ret));
            if !c.diverges(&f.body) && !c.types_agree(&body_ty, &f.ret) {
                c.reject_return_type(f.body.id(), &f.ret, &body_ty, &f.body);
            }
            // Effect check: collect the body's effects and compare against the
            // declared row. Three directions, all carrying mechanical repairs:
            //   under-declared → widen; unknown capability → drop it;
            //   over-declared (least-privilege) → narrow to what the body uses.
            let body_effects = c.collect_effects(&f.body);
            let unknown: Vec<String> = f
                .effects
                .caps
                .iter()
                .filter(|cap| !is_known_capability(cap))
                .cloned()
                .collect();
            if !unknown.is_empty() {
                c.reject_unknown_capability(&f.effects, &unknown);
            }
            if !f.effects.admits(&body_effects) {
                c.reject_effect(&f.effects, &body_effects);
            } else if unknown.is_empty() && effect_over_declared(&f.effects, &body_effects) {
                c.reject_effect_over_declared(&f.effects, &body_effects);
            }
            // Contract gate: requires/ensures must be pure boolean predicates
            // over the parameters (and `result`, for ensures). Names outside
            // that scope surface as ordinary UNBOUND_REF rejections, so using
            // `result` in a `requires` clause is rejected automatically.
            for req in &f.requires {
                let mut cscope = contract_scope(f, false);
                c.check_contract(req, &mut cscope, "requires");
            }
            for ens in &f.ensures {
                let mut cscope = contract_scope(f, true);
                c.check_contract(ens, &mut cscope, "ensures");
            }
            // Region aliasing: two `mut` references sharing a region may overlap,
            // so they are statically rejected (the region is the aliasing domain;
            // distinct regions are provably disjoint).
            c.check_region_aliasing(f);
            total.extend(c.rejections);
        }
    }
    if total.is_empty() {
        ValidationOutcome::Accepted
    } else {
        ValidationOutcome::Rejected(total)
    }
}

fn region_of(ty: &Type) -> Option<&str> {
    match ty {
        Type::Ref { region, .. } => Some(region),
        _ => None,
    }
}

/// Serialize a `Type` back to its canonical s-expression, byte-identical to what
/// `Type::parse` accepts, so a region-rename repair targets the exact type node
/// by its content-addressed id.
fn type_to_sexpr(ty: &Type) -> Sexpr {
    fn atom(s: &str) -> Sexpr { Sexpr::Atom(s.into()) }
    match ty {
        Type::I64 => atom("i64"),
        Type::F64 => atom("f64"),
        Type::Bool => atom("bool"),
        Type::Str => atom("str"),
        Type::Unit => atom("unit"),
        Type::Region => atom("region"),
        Type::Vec(t) => Sexpr::List(vec![atom("vec"), type_to_sexpr(t)]),
        Type::Struct(n) => Sexpr::List(vec![atom("struct"), atom(n)]),
        Type::Ref { region, mutable, ty } => Sexpr::List(vec![
            atom("ref"),
            atom(region),
            atom(if *mutable { "mut" } else { "ref" }),
            type_to_sexpr(ty),
        ]),
        Type::Result(ok, err) => {
            Sexpr::List(vec![atom("result"), type_to_sexpr(ok), type_to_sexpr(err)])
        }
    }
}

/// Pick a region name disjoint from every name in `used`, derived from `base`
/// (`r` -> `r_s1`, `r_s2`, ...). Deterministic so repairs are reproducible.
fn fresh_region(base: &str, used: &HashSet<String>) -> String {
    let mut k = 1;
    loop {
        let candidate = format!("{}_s{}", base, k);
        if !used.contains(&candidate) {
            return candidate;
        }
        k += 1;
    }
}

/// Collect every region name mentioned anywhere in a type (for freshness).
fn regions_in_type(ty: &Type, out: &mut HashSet<String>) {
    match ty {
        Type::Ref { region, ty, .. } => {
            out.insert(region.clone());
            regions_in_type(ty, out);
        }
        Type::Vec(t) => regions_in_type(t, out),
        Type::Result(a, b) => {
            regions_in_type(a, out);
            regions_in_type(b, out);
        }
        _ => {}
    }
}

/// The fixed capability vocabulary. Effect rows may only name these; anything
/// else is an `UNKNOWN_CAPABILITY` rejection. Capabilities with no OS shim yet
/// (everything but `rng` in v0) are still declarable and checked structurally —
/// they are deterministically stubbed at runtime when their ops arrive.
pub const KNOWN_CAPABILITIES: &[&str] = &[
    "rng", "clock", "log", "net", "state", "fs read", "fs write",
];

/// True if `cap` is a member of the capability vocabulary.
pub fn is_known_capability(cap: &str) -> bool {
    KNOWN_CAPABILITIES.contains(&cap)
}

/// A row over-declares (violates least-privilege) when it is not pure and names
/// a capability the body never exercises. A pure body over any non-pure row is
/// the fully-narrowable case.
fn effect_over_declared(declared: &EffectRow, used: &EffectRow) -> bool {
    if declared.pure {
        return false;
    }
    declared.caps.iter().any(|c| !used.caps.contains(c))
}

/// Serialize an effect row back to its `(effects ...)` s-expression form, so a
/// `widen_effect_row` repair can be applied to source mechanically. A multi-word
/// capability (e.g. `"fs read"`) becomes a nested list `(fs read)`; a bare
/// capability becomes an atom. A pure row serializes to `(effects)` — the driver
/// treats an empty `(effects)` as "remove the clause" (i.e. make the fn pure).
fn effect_row_to_sexpr(row: &EffectRow) -> Sexpr {
    let mut items = vec![Sexpr::Atom("effects".into())];
    for cap in &row.caps {
        let words: Vec<&str> = cap.split_whitespace().collect();
        if words.len() <= 1 {
            items.push(Sexpr::Atom(cap.clone()));
        } else {
            items.push(Sexpr::List(
                words.into_iter().map(|w| Sexpr::Atom(w.into())).collect(),
            ));
        }
    }
    Sexpr::List(items)
}

/// Build the checking scope for a contract clause: the function's parameters,
/// plus the `result` binding (bound to the return type) when `with_result` is
/// set (i.e. for `ensures`).
fn contract_scope(f: &FnDef, with_result: bool) -> HashMap<String, Binding> {
    let mut scope = HashMap::new();
    for p in &f.params {
        scope.insert(
            p.name.clone(),
            Binding {
                ty: p.ty.clone(),
                affine: is_affine(&p.ty),
                moved: false,
                region: region_of(&p.ty).map(|s| s.to_string()),
                is_cap: p.is_cap,
                is_param: true,
            },
        );
    }
    if with_result {
        scope.insert(
            RESULT_BINDING.to_string(),
            Binding {
                ty: f.ret.clone(),
                affine: is_affine(&f.ret),
                moved: false,
                region: region_of(&f.ret).map(|s| s.to_string()),
                is_cap: false,
                is_param: true,
            },
        );
    }
    scope
}

/// Does `e` contain a `break` that targets the immediately enclosing loop?
/// Descends through control flow but stops at nested `loop`s, whose breaks
/// belong to them, not to us.
fn loop_has_break(e: &Expr) -> bool {
    match e {
        Expr::Break { .. } => true,
        Expr::Loop { .. } => false,
        Expr::Let { init, body, .. } => loop_has_break(init) || loop_has_break(body),
        Expr::Set { value, .. } => loop_has_break(value),
        Expr::Call { args, .. } => args.iter().any(loop_has_break),
        Expr::If { cond, then, els, .. } => {
            loop_has_break(cond) || loop_has_break(then) || loop_has_break(els)
        }
        Expr::Match { scrut, arms, .. } => {
            loop_has_break(scrut) || arms.iter().any(|arm| loop_has_break(&arm.body))
        }
        Expr::Block { stmts, tail, .. } => {
            stmts.iter().any(loop_has_break) || loop_has_break(tail)
        }
        Expr::Region { body, .. } => loop_has_break(body),
        Expr::Return { value, .. } => loop_has_break(value),
        Expr::Copy { value, .. } | Expr::Cast { value, .. } => loop_has_break(value),
        Expr::VecNew { elems, .. } => elems.iter().any(loop_has_break),
        Expr::Index { target, index, .. } => loop_has_break(target) || loop_has_break(index),
        Expr::VecPush { target, value, .. } => loop_has_break(target) || loop_has_break(value),
        Expr::Len { target, .. } | Expr::Field { target, .. } => loop_has_break(target),
        Expr::StructNew { fields, .. } => fields.iter().any(|(_, v)| loop_has_break(v)),
        Expr::Lit { .. } | Expr::Ref { .. } => false,
    }
}

fn is_affine(ty: &Type) -> bool {
    match ty {
        Type::I64 | Type::F64 | Type::Bool | Type::Str | Type::Unit => false,
        Type::Vec(_) | Type::Struct(_) | Type::Ref { .. } | Type::Region => true,
        Type::Result(..) => true,
    }
}

impl Checker {
    /// Does an expression diverge (produce no normal value)? `return` and
    /// `loop` diverge; control structures diverge if all their branches do.
    /// Used so a diverging branch doesn't force a type agreement with its
    /// sibling — the pattern that lets `loop` break out via `return`.
    fn diverges(&self, e: &Expr) -> bool {
        match e {
            // `return` and `break` transfer control elsewhere. A `loop`
            // diverges only if nothing inside it can `break` out (otherwise it
            // produces the break value).
            Expr::Return { .. } | Expr::Break { .. } => true,
            Expr::Loop { body, .. } => !loop_has_break(body),
            // `set` yields unit; it diverges only if evaluating its value does.
            Expr::Set { value, .. } => self.diverges(value),
            Expr::Let { init, body, .. } => self.diverges(init) || self.diverges(body),
            Expr::Call { args, .. } => args.iter().any(|arg| self.diverges(arg)),
            Expr::If { cond, then, els, .. } => {
                self.diverges(cond) || (self.diverges(then) && self.diverges(els))
            }
            Expr::Match { scrut, arms, .. } => {
                self.diverges(scrut)
                    || (!arms.is_empty() && arms.iter().all(|arm| self.diverges(&arm.body)))
            }
            Expr::Block { stmts, tail, .. } => {
                stmts.iter().any(|stmt| self.diverges(stmt)) || self.diverges(tail)
            }
            Expr::Region { body, .. } => self.diverges(body),
            Expr::Copy { value, .. } | Expr::Cast { value, .. } => self.diverges(value),
            Expr::VecNew { elems, .. } => elems.iter().any(|elem| self.diverges(elem)),
            Expr::Index { target, index, .. } => self.diverges(target) || self.diverges(index),
            Expr::VecPush { target, value, .. } => self.diverges(target) || self.diverges(value),
            Expr::Len { target, .. } | Expr::Field { target, .. } => self.diverges(target),
            Expr::StructNew { fields, .. } => {
                fields.iter().any(|(_, value)| self.diverges(value))
            }
            Expr::Lit { .. } | Expr::Ref { .. } => false,
        }
    }

    /// Check an expression and return its inferred type. `ret_ty` is the
    /// enclosing function's declared return type (for return checking).
    fn check_expr(
        &mut self,
        e: &Expr,
        scope: &mut HashMap<String, Binding>,
        ret_ty: Option<&Type>,
    ) -> Type {
        match e {
            Expr::Lit { id: _, value } => lit_type(value),
            Expr::Ref { id, name } => {
                match scope.get(name) {
                    Some(b) => {
                        if b.affine && b.moved {
                            self.reject_use_after_move(*id, name, &b.ty);
                        }
                        b.ty.clone()
                    }
                    None => {
                        self.reject_unbound_ref(*id, name);
                        Type::Unit
                    }
                }
            }
            Expr::Let { id, name, ty, init, body } => {
                let init_ty = self.check_expr(init, scope, ret_ty);
                if !self.diverges(init) && !self.types_agree(&init_ty, ty) {
                    self.reject_type_mismatch(*id, ty, &init_ty, "let binding");
                    // still continue with the declared type in scope
                }
                let region = region_of(ty).map(|s| s.to_string());
                if let Some(r) = &region {
                    self.regions_in_scope.push(r.clone());
                }
                let previous = scope.insert(
                    name.clone(),
                    Binding {
                        ty: ty.clone(),
                        affine: is_affine(ty),
                        moved: false,
                        region: region.clone(),
                        is_cap: false,
                        is_param: false,
                    },
                );
                let body_ty = self.check_expr(body, scope, ret_ty);
                if region.is_some() {
                    self.regions_in_scope.pop();
                }
                if let Some(previous) = previous {
                    scope.insert(name.clone(), previous);
                } else {
                    scope.remove(name);
                }
                body_ty
            }
            Expr::Call { id, op, args } => {
                // Built-in operators.
                if let Some((ret, builtin_effects)) = self.check_builtin(*id, op, args, scope, ret_ty) {
                    let _ = builtin_effects; // builtins are pure in v0 except rng.*
                    return ret;
                }
                // User functions.
                if let Some(sig) = self.fns.get(op).cloned() {
                    if args.len() != sig.params.len() {
                        self.reject_arity(*id, op, sig.params.len(), args.len());
                    }
                    let mut effects = EffectRow::pure_row();
                    for (i, (arg, (pname, pty, is_cap))) in
                        args.iter().zip(sig.params.iter()).enumerate()
                    {
                        let arg_ty = self.check_expr(arg, scope, ret_ty);
                        if !self.diverges(arg) && !is_cap && !self.types_agree(&arg_ty, pty) {
                            self.reject_call_arg_type(*id, op, i, pname, pty, &arg_ty, arg);
                        }
                        if arg.is_cap_value() {
                            // capability argument; record its cap string in effects
                        }
                        effects = effects.union_with(&sig.effects);
                    }
                    if !self.declared_effects.admits(&effects) {
                        let de = self.declared_effects.clone();
                        self.reject_call_effects_exceed_declared(*id, op, &de, &effects);
                    }
                    sig.ret
                } else {
                    self.reject_unknown_call(*id, op);
                    // still check args for downstream errors
                    for a in args {
                        let _ = self.check_expr(a, scope, ret_ty);
                    }
                    Type::Unit
                }
            }
            Expr::If { id, cond, then, els } => {
                let cond_ty = self.check_expr(cond, scope, ret_ty);
                if !self.diverges(cond) && !self.types_agree(&cond_ty, &Type::Bool) {
                    self.reject_type_mismatch(cond.id(), &Type::Bool, &cond_ty, "if condition");
                }
                let then_ty = self.check_expr(then, scope, ret_ty);
                let els_ty = self.check_expr(els, scope, ret_ty);
                // A `return` (or `loop`) diverges: it produces no normal value, so
                // a branch that diverges need not agree with the other. This is
                // what lets `(if cond (return x) (else y))` typecheck — the
                // classic pattern for breaking out of a `loop`.
                let then_div = self.diverges(then);
                let els_div = self.diverges(els);
                if !self.diverges(cond) && !then_div && !els_div && !self.types_agree(&then_ty, &els_ty) {
                    self.reject_branch_mismatch(*id, &then_ty, &els_ty);
                }
                if then_div && els_div {
                    Type::Unit
                } else if then_div {
                    els_ty
                } else if els_div {
                    then_ty
                } else {
                    then_ty
                }
            }
            Expr::Match { id, scrut, arms } => {
                let scrut_ty = self.check_expr(scrut, scope, ret_ty);
                let mut arm_tys = Vec::new();
                let mut arm_div = Vec::new();
                for arm in arms {
                    let mut arm_scope = scope.clone();
                    self.bind_pattern(&mut arm_scope, &arm.pattern, &scrut_ty);
                    arm_tys.push(self.check_expr(&arm.body, &mut arm_scope, ret_ty));
                    arm_div.push(self.diverges(&arm.body));
                }
                // Arms that diverge (e.g. end in `return`) need not agree with
                // the others; the match's type is that of a non-diverging arm.
                let mut agreed: Option<Type> = None;
                for (t, d) in arm_tys.iter().zip(arm_div.iter()) {
                    if self.diverges(scrut) {
                        break;
                    }
                    if *d {
                        continue;
                    }
                    if let Some(ref a) = agreed {
                        if !self.types_agree(a, t) {
                            self.reject_branch_mismatch(*id, a, t);
                        }
                    } else {
                        agreed = Some(t.clone());
                    }
                }
                agreed.unwrap_or(Type::Unit)
            }
            Expr::Loop { id: _, body } => {
                // Open a fresh break-target frame; each `break` inside records
                // its value type here. The loop's type is the agreed break type,
                // or unit if the loop has no `break` (and so diverges).
                self.break_tys.push(None);
                let _ = self.check_expr(body, scope, ret_ty);
                self.break_tys.pop().flatten().unwrap_or(Type::Unit)
            }
            Expr::Break { id, value } => {
                let val_ty = self.check_expr(value, scope, ret_ty);
                if self.break_tys.is_empty() {
                    self.reject_break_outside_loop(*id);
                } else if !self.diverges(value) {
                    let existing = self.break_tys.last().unwrap().clone();
                    match existing {
                        None => *self.break_tys.last_mut().unwrap() = Some(val_ty),
                        Some(prev) => {
                            if !self.types_agree(&prev, &val_ty) {
                                self.reject_break_type_mismatch(*id, &prev, &val_ty, value);
                            }
                        }
                    }
                }
                Type::Unit // break diverges; use unit as a placeholder
            }
            Expr::Set { id, name, value } => {
                let val_ty = self.check_expr(value, scope, ret_ty);
                // Extract what we need before calling self methods (borrows).
                let target = scope.get(name).map(|b| (b.is_param, b.ty.clone()));
                match target {
                    None => self.reject_set_unbound(*id, name),
                    Some((true, ty)) => self.reject_set_of_param(*id, name, &ty),
                    Some((false, ty)) => {
                        if !self.diverges(value) && !self.types_agree(&val_ty, &ty) {
                            self.reject_set_type_mismatch(*id, name, &ty, &val_ty, value);
                        }
                        // Reassignment revives the binding: `(set acc (vec-push
                        // acc x))` moves `acc` inside the push, then rebinds it,
                        // so the accumulator loop pattern type-checks.
                        if let Some(b) = scope.get_mut(name) {
                            b.moved = false;
                        }
                    }
                }
                Type::Unit // set yields unit
            }
            Expr::Return { id, value } => {
                let val_ty = self.check_expr(value, scope, ret_ty);
                if let Some(ret) = ret_ty {
                    if !self.diverges(value) && !self.types_agree(&val_ty, ret) {
                        self.reject_return_type(*id, ret, &val_ty, value);
                    }
                }
                Type::Unit // return diverges; use unit as a placeholder
            }
            Expr::Block { id: _, stmts, tail } => {
                for s in stmts {
                    let _ = self.check_expr(s, scope, ret_ty);
                }
                self.check_expr(tail, scope, ret_ty)
            }
            Expr::Region { id: _, name, body } => {
                self.regions_in_scope.push(name.clone());
                let ty = self.check_expr(body, scope, ret_ty);
                self.regions_in_scope.pop();
                ty
            }
            Expr::Copy { id, value } => {
                // `copy` yields a fresh independent value and is permitted even on
                // a binding that has already been moved (v0 aggregate values are
                // structurally copyable). This is what makes the `insert_copy`
                // repair for USE_AFTER_MOVE converge: replacing a moved `(ref v)`
                // with `(copy v)` type-checks instead of re-triggering the move.
                let v = if let Expr::Ref { name, .. } = value.as_ref() {
                    match scope.get(name) {
                        Some(b) => b.ty.clone(),
                        None => {
                            self.reject_unbound_ref(value.id(), name);
                            Type::Unit
                        }
                    }
                } else {
                    self.check_expr(value, scope, ret_ty)
                };
                if !self.diverges(value) && !is_affine(&v) {
                    self.reject_copy_of_non_affine(*id, &v);
                }
                v
            }
            Expr::VecNew { id, ty, elems } => {
                for (i, el) in elems.iter().enumerate() {
                    let el_ty = self.check_expr(el, scope, ret_ty);
                    let inner = match ty {
                        Type::Vec(t) => t.as_ref().clone(),
                        _ => {
                            self.reject_vec_new_bad_type(*id, ty);
                            continue;
                        }
                    };
                    if !self.diverges(el) && !self.types_agree(&el_ty, &inner) {
                        self.reject_vec_elem_type(*id, i, &inner, &el_ty, el);
                    }
                }
                ty.clone()
            }
            Expr::Index { id, target, index } => {
                let t = self.check_expr(target, scope, ret_ty);
                let idx = self.check_expr(index, scope, ret_ty);
                if !self.diverges(index) && !self.types_agree(&idx, &Type::I64) {
                    self.reject_type_mismatch(index.id(), &Type::I64, &idx, "vec index");
                }
                if self.diverges(target) || self.diverges(index) {
                    Type::Unit
                } else {
                    match &t {
                        Type::Vec(inner) => inner.as_ref().clone(),
                        _ => {
                            self.reject_index_non_vec(*id, &t);
                            Type::Unit
                        }
                    }
                }
            }
            Expr::Len { id, target } => {
                let t = self.check_expr(target, scope, ret_ty);
                if self.diverges(target) {
                    Type::I64
                } else {
                    match &t {
                        Type::Vec(_) => Type::I64,
                        _ => {
                            self.reject_len_non_vec(*id, &t);
                            Type::I64
                        }
                    }
                }
            }
            Expr::VecPush { id, target, value } => {
                // Check the target first (fires USE_AFTER_MOVE if it is a binding
                // that was already moved), then consume it.
                let t = self.check_expr(target, scope, ret_ty);
                // vec-push takes ownership of its target. When the target is a
                // bare `(ref v)` of an affine binding, mark `v` moved so a later
                // plain use is rejected; a `(copy v)` target is non-consuming.
                if let Expr::Ref { name, .. } = target.as_ref() {
                    if let Some(b) = scope.get_mut(name) {
                        if b.affine {
                            b.moved = true;
                        }
                    }
                }
                // Now check the appended value — if it re-uses the just-moved
                // target binding, that surfaces here as USE_AFTER_MOVE.
                let v = self.check_expr(value, scope, ret_ty);
                if self.diverges(target) {
                    return Type::Unit;
                }
                match &t {
                    Type::Vec(inner) => {
                        if !self.diverges(value) && !self.types_agree(&v, inner) {
                            self.reject_type_mismatch(value.id(), inner, &v, "vec-push value");
                        }
                        t.clone()
                    }
                    _ => {
                        self.reject_push_non_vec(*id, &t);
                        Type::Unit
                    }
                }
            }
            Expr::StructNew { id, name, fields } => {
                let sdef = self.structs.get(name).cloned();
                let Some(sdef) = sdef else {
                    self.reject_unknown_struct(*id, name);
                    return Type::Unit;
                };
                let mut seen = HashSet::new();
                for (fname, fval) in fields {
                    let val_ty = self.check_expr(fval, scope, ret_ty);
                    if !seen.insert(fname.as_str()) {
                        self.reject_duplicate_field(*id, name, fname);
                        continue;
                    }
                    let declared = sdef.fields.iter().find(|(n, _)| n == fname);
                    let Some((_, dt)) = declared else {
                        self.reject_unknown_field(*id, name, fname);
                        continue;
                    };
                    if !self.diverges(fval) && !self.types_agree(&val_ty, dt) {
                        self.reject_type_mismatch(fval.id(), dt, &val_ty, "struct field");
                    }
                }
                // missing fields
                for (n, _) in &sdef.fields {
                    if !fields.iter().any(|(fn_, _)| fn_ == n) {
                        self.reject_missing_field(*id, name, n);
                    }
                }
                Type::Struct(name.clone())
            }
            Expr::Field { id, target, field } => {
                let t = self.check_expr(target, scope, ret_ty);
                if self.diverges(target) {
                    Type::Unit
                } else {
                    match &t {
                        Type::Struct(sname) => {
                            let sdef = self.structs.get(sname);
                            if let Some(sdef) = sdef {
                                let f = sdef.fields.iter().find(|(n, _)| n == field);
                                match f {
                                    Some((_, ty)) => ty.clone(),
                                    None => {
                                        self.reject_unknown_field(*id, sname, field);
                                        Type::Unit
                                    }
                                }
                            } else {
                                Type::Unit
                            }
                        }
                        _ => {
                            self.reject_field_non_struct(*id, &t);
                            Type::Unit
                        }
                    }
                }
            }
            Expr::Cast { id, target, value } => {
                let v = self.check_expr(value, scope, ret_ty);
                if !self.diverges(value) && !self.cast_valid(&v, target) {
                    self.reject_invalid_cast(*id, &v, target, value);
                }
                target.clone()
            }
        }
    }

    /// Check one contract clause: it must be a pure boolean predicate. Scoping
    /// (only params, plus `result` for ensures) is enforced by `check_expr`'s
    /// ordinary unbound-reference reporting.
    fn check_contract(&mut self, e: &Expr, scope: &mut HashMap<String, Binding>, which: &str) {
        let t = self.check_expr(e, scope, None);
        if !self.diverges(e) && !self.types_agree(&t, &Type::Bool) {
            self.reject_contract_not_bool(e.id(), which, &t);
        }
        let eff = self.collect_effects(e);
        if !eff.pure {
            self.reject_contract_impure(e.id(), which);
        }
    }

    fn bind_pattern(&self, scope: &mut HashMap<String, Binding>, pat: &Pattern, ty: &Type) {
        match pat {
            Pattern::Wild => {}
            Pattern::Bind(n) => {
                scope.insert(
                    n.clone(),
                    Binding {
                        ty: ty.clone(),
                        affine: is_affine(ty),
                        moved: false,
                        region: region_of(ty).map(|s| s.to_string()),
                        is_cap: false,
                        is_param: false,
                    },
                );
            }
            Pattern::Lit(_) => {}
        }
    }

    /// Built-in operators with fixed signatures. Returns (return_type, effects).
    fn check_builtin(
        &mut self,
        id: NodeId,
        op: &str,
        args: &[Expr],
        scope: &mut HashMap<String, Binding>,
        ret_ty: Option<&Type>,
    ) -> Option<(Type, EffectRow)> {
        // Arity + argument types for builtin ops.
        let (ret, arg_tys): (Type, Vec<Type>) = match op {
            "i64.add" | "i64.sub" | "i64.mul" | "i64.div" | "i64.mod" => {
                (Type::I64, vec![Type::I64, Type::I64])
            }
            "i64.gt" | "i64.lt" | "i64.ge" | "i64.le" | "i64.eq" | "i64.neq" => {
                (Type::Bool, vec![Type::I64, Type::I64])
            }
            "i64.neg" | "i64.abs" => (Type::I64, vec![Type::I64]),
            "i64.from_str" | "i64.parse" => (Type::Result(Box::new(Type::I64), Box::new(Type::Str)), vec![Type::Str]),
            "i64.to_str" => (Type::Str, vec![Type::I64]),
            "f64.add" | "f64.sub" | "f64.mul" | "f64.div" => {
                (Type::F64, vec![Type::F64, Type::F64])
            }
            "f64.gt" | "f64.lt" | "f64.ge" | "f64.le" | "f64.eq" | "f64.neq" => {
                (Type::Bool, vec![Type::F64, Type::F64])
            }
            "f64.neg" | "f64.abs" => (Type::F64, vec![Type::F64]),
            "f64.to_str" => (Type::Str, vec![Type::F64]),
            "bool.and" | "bool.or" => (Type::Bool, vec![Type::Bool, Type::Bool]),
            "bool.not" => (Type::Bool, vec![Type::Bool]),
            "bool.eq" => (Type::Bool, vec![Type::Bool, Type::Bool]),
            "str.eq" | "str.neq" => (Type::Bool, vec![Type::Str, Type::Str]),
            "str.concat" => (Type::Str, vec![Type::Str, Type::Str]),
            "str.len" => (Type::I64, vec![Type::Str]),
            "result.is_ok" => (Type::Bool, vec![Type::Result(Box::new(Type::I64), Box::new(Type::Str))]),
            // rng ops take no args in v0; the `rng` capability is gated by the
            // function's declared effect row. First-class capability values
            // arrive in v1.
            "rng.next" | "rng.i64" => (Type::I64, vec![]),
            _ => return None,
        };
        if args.len() != arg_tys.len() {
            self.reject_arity(id, op, arg_tys.len(), args.len());
            return Some((ret, EffectRow::pure_row()));
        }
        for (i, (arg, expected)) in args.iter().zip(arg_tys.iter()).enumerate() {
            let arg_ty = self.check_expr(arg, scope, ret_ty);
            if !self.diverges(arg) && !self.types_agree(&arg_ty, expected) {
                self.reject_call_arg_type(id, op, i, "_", expected, &arg_ty, arg);
            }
        }
        // rng.* requires the rng capability.
        let effects = if op.starts_with("rng.") {
            EffectRow {
                pure: false,
                caps: vec!["rng".to_string()],
            }
        } else {
            EffectRow::pure_row()
        };
        Some((ret, effects))
    }

    fn collect_effects(&self, e: &Expr) -> EffectRow {
        let mut row = EffectRow::pure_row();
        self.collect_effects_into(e, &mut row);
        row
    }
    fn collect_effects_into(&self, e: &Expr, row: &mut EffectRow) {
        match e {
            Expr::Call { op, args, .. } => {
                if op.starts_with("rng.") {
                    *row = row.union_with(&EffectRow {
                        pure: false,
                        caps: vec!["rng".to_string()],
                    });
                }
                if let Some(sig) = self.fns.get(op) {
                    *row = row.union_with(&sig.effects);
                }
                for a in args {
                    self.collect_effects_into(a, row);
                }
            }
            Expr::Let { init, body, .. } => {
                self.collect_effects_into(init, row);
                self.collect_effects_into(body, row);
            }
            Expr::If { cond, then, els, .. } => {
                self.collect_effects_into(cond, row);
                self.collect_effects_into(then, row);
                self.collect_effects_into(els, row);
            }
            Expr::Match { scrut, arms, .. } => {
                self.collect_effects_into(scrut, row);
                for a in arms {
                    self.collect_effects_into(&a.body, row);
                }
            }
            Expr::Loop { body, .. } => self.collect_effects_into(body, row),
            Expr::Break { value, .. } => self.collect_effects_into(value, row),
            Expr::Set { value, .. } => self.collect_effects_into(value, row),
            Expr::Return { value, .. } => self.collect_effects_into(value, row),
            Expr::Block { stmts, tail, .. } => {
                for s in stmts {
                    self.collect_effects_into(s, row);
                }
                self.collect_effects_into(tail, row);
            }
            Expr::Region { body, .. } => self.collect_effects_into(body, row),
            Expr::Copy { value, .. } => self.collect_effects_into(value, row),
            Expr::VecNew { elems, .. } => {
                for e in elems {
                    self.collect_effects_into(e, row);
                }
            }
            Expr::Index { target, index, .. } => {
                self.collect_effects_into(target, row);
                self.collect_effects_into(index, row);
            }
            Expr::VecPush { target, value, .. } => {
                self.collect_effects_into(target, row);
                self.collect_effects_into(value, row);
            }
            Expr::Len { target, .. } => self.collect_effects_into(target, row),
            Expr::StructNew { fields, .. } => {
                for (_, v) in fields {
                    self.collect_effects_into(v, row);
                }
            }
            Expr::Field { target, .. } => self.collect_effects_into(target, row),
            Expr::Cast { value, .. } => self.collect_effects_into(value, row),
            Expr::Lit { .. } | Expr::Ref { .. } => {}
        }
    }

    // ---- type agreement (structural equality, no subtyping) ----
    fn types_agree(&self, a: &Type, b: &Type) -> bool {
        a == b
    }

    fn cast_valid(&self, from: &Type, to: &Type) -> bool {
        // v0: i64<->str via parse/to_str is a result; we allow identity casts
        // and i64->i64. Real cast matrix is tiny here.
        from == to
            || (matches!(from, Type::I64) && matches!(to, Type::I64))
            || (matches!(from, Type::Str) && matches!(to, Type::I64))
            || (matches!(from, Type::I64) && matches!(to, Type::Str))
            // Numeric casts both directions, plus f64 -> str formatting.
            || (matches!(from, Type::I64) && matches!(to, Type::F64))
            || (matches!(from, Type::F64) && matches!(to, Type::I64))
            || (matches!(from, Type::F64) && matches!(to, Type::Str))
    }

    // ===== rejection builders =====
    // Each builds a structured rejection *with a ranked list of admissible
    // repairs*. Repairs are admissible by construction: they name a checked
    // replacement. The model applies one and resubmits.

    fn next_repair_id(&mut self) -> String {
        self.repair_counter += 1;
        format!("r{}", self.repair_counter)
    }

    fn reject_unbound_ref(&mut self, id: NodeId, name: &str) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "UNBOUND_REF".into(),
            node: id,
            path: name.into(),
            expected: "a binding in scope".into(),
            received: format!("unbound name `{}`", name),
            context: HashMap::new(),
            repairs: vec![],
        });
    }

    fn reject_use_after_move(&mut self, id: NodeId, name: &str, ty: &Type) {
        let rid = self.next_repair_id();
        let repairs = vec![Repair {
            id: rid,
            action: "insert_copy".into(),
            with: Some(Sexpr::List(vec![
                Sexpr::Atom("copy".into()),
                Sexpr::Atom(name.into()),
            ])),
            cost: 1,
            preserves_effects: true,
            preserves_contracts: true,
            propagates: vec![],
            note: "Insert an explicit (copy <name>) before the move site.".into(),
        }];
        self.rejections.push(Rejection {
            gate: Gate::Region,
            kind: "USE_AFTER_MOVE".into(),
            node: id,
            path: name.into(),
            expected: format!("live value of type {:?}", ty),
            received: "moved value".into(),
            context: HashMap::new(),
            repairs,
        });
    }

    fn reject_type_mismatch(&mut self, id: NodeId, expected: &Type, received: &Type, ctx: &str) {
        let repairs = build_type_repairs(self, id, expected, received);
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "TYPE_MISMATCH".into(),
            node: id,
            path: ctx.into(),
            expected: format!("{:?}", expected),
            received: format!("{:?}", received),
            context: HashMap::new(),
            repairs,
        });
    }

    fn reject_call_arg_type(
        &mut self,
        id: NodeId,
        op: &str,
        i: usize,
        pname: &str,
        expected: &Type,
        received: &Type,
        _arg: &Expr,
    ) {
        let mut repairs = build_type_repairs(self, id, expected, received);
        // A param-type-change repair is admissible here if we change the
        // callee's signature; mark its propagation as the call sites.
        repairs.push(Repair {
            id: self.next_repair_id(),
            action: "change_param_type".into(),
            with: None,
            cost: 5,
            preserves_effects: false,
            preserves_contracts: false,
            propagates: vec![format!("callee `{}` param `{}`", op, pname)],
            note: "Change the callee's parameter type (propagates to call sites).".into(),
        });
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "ARG_TYPE_MISMATCH".into(),
            node: id,
            path: format!("call `{}` arg[{}]", op, i),
            expected: format!("{:?}", expected),
            received: format!("{:?}", received),
            context: {
                let mut m = HashMap::new();
                m.insert("param_name".into(), pname.into());
                m
            },
            repairs,
        });
    }

    fn reject_arity(&mut self, id: NodeId, op: &str, expected: usize, received: usize) {
        let rid = self.next_repair_id();
        let repairs = vec![Repair {
            id: rid,
            action: "add_arg".into(),
            with: None,
            cost: 2,
            preserves_effects: true,
            preserves_contracts: true,
            propagates: vec![],
            note: format!("Add {} missing argument(s).", expected.saturating_sub(received)),
        }];
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "ARITY_MISMATCH".into(),
            node: id,
            path: format!("call `{}`", op),
            expected: expected.to_string(),
            received: received.to_string(),
            context: HashMap::new(),
            repairs,
        });
    }

    fn reject_unknown_call(&mut self, id: NodeId, op: &str) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "UNKNOWN_CALL".into(),
            node: id,
            path: op.into(),
            expected: "a known function or builtin".into(),
            received: format!("unknown op `{}`", op),
            context: HashMap::new(),
            repairs: vec![],
        });
    }

    fn reject_branch_mismatch(&mut self, id: NodeId, then_ty: &Type, els_ty: &Type) {
        let repairs = build_type_repairs(self, id, then_ty, els_ty);
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "BRANCH_TYPE_MISMATCH".into(),
            node: id,
            path: "if/match branches".into(),
            expected: format!("{:?}", then_ty),
            received: format!("{:?}", els_ty),
            context: HashMap::new(),
            repairs,
        });
    }

    fn reject_return_type(&mut self, id: NodeId, expected: &Type, received: &Type, _val: &Expr) {
        let repairs = build_type_repairs(self, id, expected, received);
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "RETURN_TYPE_MISMATCH".into(),
            node: id,
            path: "return".into(),
            expected: format!("{:?}", expected),
            received: format!("{:?}", received),
            context: HashMap::new(),
            repairs,
        });
    }

    fn reject_break_outside_loop(&mut self, id: NodeId) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "BREAK_OUTSIDE_LOOP".into(),
            node: id,
            path: "break".into(),
            expected: "a `break` inside an enclosing `loop`".into(),
            received: "`break` with no enclosing loop".into(),
            context: HashMap::new(),
            // No known-good structural replacement without the surrounding
            // context; the fix is to move the `break` into a loop or use
            // `return` to leave the function.
            repairs: vec![],
        });
    }

    fn reject_break_type_mismatch(&mut self, id: NodeId, expected: &Type, received: &Type, _val: &Expr) {
        let repairs = build_type_repairs(self, id, expected, received);
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "BREAK_TYPE_MISMATCH".into(),
            node: id,
            path: "break".into(),
            expected: format!("{:?}", expected),
            received: format!("{:?}", received),
            context: HashMap::new(),
            repairs,
        });
    }

    fn reject_set_unbound(&mut self, id: NodeId, name: &str) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "SET_UNBOUND".into(),
            node: id,
            path: name.into(),
            expected: "an existing local binding to reassign".into(),
            received: format!("`set` of unbound name `{}`", name),
            context: HashMap::new(),
            // Introducing the binding is a structural change to the enclosing
            // scope, not a local node rewrite; leave to the model.
            repairs: vec![],
        });
    }

    fn reject_set_of_param(&mut self, id: NodeId, name: &str, ty: &Type) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "SET_OF_PARAM".into(),
            node: id,
            path: name.into(),
            expected: "a mutable local (`let`) binding".into(),
            received: format!("`set` of parameter `{}` (of type {:?})", name, ty),
            context: HashMap::new(),
            // Fix: introduce a mutable local seeded from the parameter, e.g.
            // `(let name <ty> (ref name) ...)`, and mutate that instead.
            repairs: vec![],
        });
    }

    fn reject_set_type_mismatch(&mut self, id: NodeId, name: &str, expected: &Type, received: &Type, _val: &Expr) {
        let repairs = build_type_repairs(self, id, expected, received);
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "SET_TYPE_MISMATCH".into(),
            node: id,
            path: name.into(),
            expected: format!("{:?}", expected),
            received: format!("{:?}", received),
            context: HashMap::new(),
            repairs,
        });
    }

    /// Region-aliasing pass: within a function signature, two `mut` references
    /// naming the same region can overlap, so they are rejected pointwise. The
    /// repair renames one reference's region to a fresh, disjoint region (the
    /// proposal's "split into two regions").
    fn check_region_aliasing(&mut self, f: &FnDef) {
        // All region names in the signature (for picking a disjoint fresh name).
        let mut used_regions: HashSet<String> = HashSet::new();
        for p in &f.params {
            regions_in_type(&p.ty, &mut used_regions);
        }
        regions_in_type(&f.ret, &mut used_regions);
        // Group reference params by region. A `mut` reference is exclusive: it
        // may not share its region with any other reference (mut or shared).
        // Two shared references may coexist, so a group with no `mut` is fine.
        let mut by_region: HashMap<String, Vec<&Param>> = HashMap::new();
        for p in &f.params {
            if let Type::Ref { region, .. } = &p.ty {
                by_region.entry(region.clone()).or_default().push(p);
            }
        }
        // Deterministic order over regions so repairs are reproducible.
        let mut regions: Vec<&String> = by_region.keys().collect();
        regions.sort();
        for region in regions {
            let members = &by_region[region];
            let has_mut = members
                .iter()
                .any(|p| matches!(&p.ty, Type::Ref { mutable: true, .. }));
            if members.len() < 2 || !has_mut {
                continue;
            }
            // Keep the first `mut` reference in place; rename another member out.
            let first = members
                .iter()
                .find(|p| matches!(&p.ty, Type::Ref { mutable: true, .. }))
                .unwrap();
            let victim = members
                .iter()
                .find(|p| p.name != first.name)
                .unwrap();
            let fresh = fresh_region(region, &used_regions);
            let renamed = match &victim.ty {
                Type::Ref { mutable, ty, .. } => Type::Ref {
                    region: fresh.clone(),
                    mutable: *mutable,
                    ty: ty.clone(),
                },
                _ => continue,
            };
            let ty_node = sexpr_id(&type_to_sexpr(&victim.ty));
            self.reject_alias_conflict(
                ty_node,
                region,
                &first.name,
                &victim.name,
                &fresh,
                type_to_sexpr(&renamed),
            );
        }
    }

    fn reject_alias_conflict(
        &mut self,
        ty_node: NodeId,
        region: &str,
        first: &str,
        second: &str,
        fresh: &str,
        renamed: Sexpr,
    ) {
        let rid = self.next_repair_id();
        self.rejections.push(Rejection {
            gate: Gate::Region,
            kind: "ALIAS_CONFLICT".into(),
            node: ty_node,
            path: format!("region `{}`", region),
            expected: "at most one `mut` reference per region".into(),
            received: format!(
                "region `{}` borrowed mut by `{}` and also by `{}`",
                region, first, second
            ),
            context: {
                let mut m = HashMap::new();
                m.insert("region".into(), region.to_string());
                m.insert("conflicts".into(), format!("{}, {}", first, second));
                m
            },
            repairs: vec![Repair {
                id: rid,
                action: "split_region".into(),
                with: Some(renamed),
                cost: 2,
                preserves_effects: true,
                preserves_contracts: true,
                propagates: vec![],
                note: format!(
                    "Split the region: move `{}` into a fresh disjoint region `{}`.",
                    second, fresh
                ),
            }],
        });
    }

    fn reject_effect(&mut self, declared: &EffectRow, actual: &EffectRow) {
        let missing: Vec<String> = actual
            .caps
            .iter()
            .filter(|c| !declared.caps.contains(c))
            .cloned()
            .collect();
        let widened = declared.union_with(actual);
        let rid = self.next_repair_id();
        self.rejections.push(Rejection {
            gate: Gate::Effect,
            kind: "EFFECT_EXCEEDS_DECLARED".into(),
            // Target the whole `(fn ...)` form: the fix rewrites its effect row,
            // not the body node where the effect was observed.
            node: self.fn_id,
            path: "fn effect row".into(),
            expected: format!("{:?}", declared),
            received: format!("{:?}", actual),
            context: {
                let mut m = HashMap::new();
                m.insert("missing_caps".into(), missing.join(", "));
                m
            },
            repairs: vec![Repair {
                id: rid,
                action: "widen_effect_row".into(),
                // The driver locates the fn node and sets/inserts this clause.
                with: Some(effect_row_to_sexpr(&widened)),
                cost: 2,
                preserves_effects: true,
                preserves_contracts: true,
                propagates: vec![],
                note: format!(
                    "Widen the function's effect row to declare: {}",
                    missing.join(", ")
                ),
            }],
        });
    }

    /// Least-privilege: the row declares a capability the body never uses.
    /// Narrow it to exactly the used effects (an empty row removes the clause).
    fn reject_effect_over_declared(&mut self, declared: &EffectRow, used: &EffectRow) {
        let extra: Vec<String> = declared
            .caps
            .iter()
            .filter(|c| !used.caps.contains(c))
            .cloned()
            .collect();
        let rid = self.next_repair_id();
        self.rejections.push(Rejection {
            gate: Gate::Effect,
            kind: "EFFECT_OVER_DECLARED".into(),
            node: self.fn_id,
            path: "fn effect row".into(),
            expected: format!("{:?}", used),
            received: format!("{:?}", declared),
            context: {
                let mut m = HashMap::new();
                m.insert("unused_caps".into(), extra.join(", "));
                m
            },
            repairs: vec![Repair {
                id: rid,
                action: "drop_unused_effect".into(),
                with: Some(effect_row_to_sexpr(used)),
                cost: 2,
                preserves_effects: true,
                preserves_contracts: true,
                propagates: vec![],
                note: format!(
                    "Narrow the effect row to least privilege; drop unused: {}",
                    extra.join(", ")
                ),
            }],
        });
    }

    /// A declared capability outside the vocabulary. Drop the unknown caps,
    /// keeping the known remainder.
    fn reject_unknown_capability(&mut self, declared: &EffectRow, unknown: &[String]) {
        let kept = EffectRow {
            pure: false,
            caps: declared
                .caps
                .iter()
                .filter(|c| is_known_capability(c))
                .cloned()
                .collect(),
        };
        let rid = self.next_repair_id();
        self.rejections.push(Rejection {
            gate: Gate::Effect,
            kind: "UNKNOWN_CAPABILITY".into(),
            node: self.fn_id,
            path: "fn effect row".into(),
            expected: format!("one of: {}", KNOWN_CAPABILITIES.join(", ")),
            received: unknown.join(", "),
            context: HashMap::new(),
            repairs: vec![Repair {
                id: rid,
                action: "drop_unused_effect".into(),
                with: Some(effect_row_to_sexpr(&kept)),
                cost: 3,
                preserves_effects: true,
                preserves_contracts: true,
                propagates: vec![],
                note: format!("Remove unrecognized capability/ies: {}", unknown.join(", ")),
            }],
        });
    }

    fn reject_call_effects_exceed_declared(
        &mut self,
        _id: NodeId,
        callee: &str,
        declared: &EffectRow,
        actual: &EffectRow,
    ) {
        let widened = declared.union_with(actual);
        let rid = self.next_repair_id();
        self.rejections.push(Rejection {
            gate: Gate::Effect,
            kind: "CALL_EFFECT_EXCEEDS_DECLARED".into(),
            node: self.fn_id,
            path: format!("call `{}`", callee),
            expected: format!("{:?}", declared),
            received: format!("{:?}", actual),
            context: HashMap::new(),
            repairs: vec![Repair {
                id: rid,
                action: "widen_effect_row".into(),
                with: Some(effect_row_to_sexpr(&widened)),
                cost: 2,
                preserves_effects: true,
                preserves_contracts: true,
                propagates: vec![],
                note: format!(
                    "Widen this function's effect row to cover the effects of `{}`.",
                    callee
                ),
            }],
        });
    }

    fn reject_copy_of_non_affine(&mut self, id: NodeId, ty: &Type) {
                let _rid3 = self.next_repair_id();
self.rejections.push(Rejection {
            gate: Gate::Region,
            kind: "COPY_OF_NON_AFFINE".into(),
            node: id,
            path: "copy".into(),
            expected: "an affine (owned) value".into(),
            received: format!("{:?} (copyable)", ty),
            context: HashMap::new(),
            repairs: vec![Repair {
                id: _rid3,
                action: "remove_copy".into(),
                with: None,
                cost: 1,
                preserves_effects: true,
                preserves_contracts: true,
                propagates: vec![],
                note: "Drop the (copy ...) — the value is already freely copyable.".into(),
            }],
        });
    }

    fn reject_vec_new_bad_type(&mut self, id: NodeId, ty: &Type) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "VEC_NEW_BAD_TYPE".into(),
            node: id,
            path: "vec-new".into(),
            expected: "(vec T)".into(),
            received: format!("{:?}", ty),
            context: HashMap::new(),
            repairs: vec![],
        });
    }

    fn reject_vec_elem_type(&mut self, id: NodeId, i: usize, expected: &Type, received: &Type, _el: &Expr) {
                let repairs = build_type_repairs(self, id, expected, received);
self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "VEC_ELEM_TYPE_MISMATCH".into(),
            node: id,
            path: format!("vec-new elem[{}]", i),
            expected: format!("{:?}", expected),
            received: format!("{:?}", received),
            context: HashMap::new(),
            repairs,
        });
    }

    fn reject_index_non_vec(&mut self, id: NodeId, ty: &Type) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "INDEX_NON_VEC".into(),
            node: id,
            path: "idx".into(),
            expected: "(vec T)".into(),
            received: format!("{:?}", ty),
            context: HashMap::new(),
            repairs: vec![],
        });
    }

    fn reject_push_non_vec(&mut self, id: NodeId, ty: &Type) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "PUSH_NON_VEC".into(),
            node: id,
            path: "vec-push".into(),
            expected: "(vec T)".into(),
            received: format!("{:?}", ty),
            context: HashMap::new(),
            repairs: vec![],
        });
    }

    fn reject_len_non_vec(&mut self, id: NodeId, ty: &Type) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "LEN_NON_VEC".into(),
            node: id,
            path: "len".into(),
            expected: "(vec T)".into(),
            received: format!("{:?}", ty),
            context: HashMap::new(),
            repairs: vec![],
        });
    }

    fn reject_unknown_struct(&mut self, id: NodeId, name: &str) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "UNKNOWN_STRUCT".into(),
            node: id,
            path: name.into(),
            expected: "a defined struct".into(),
            received: format!("unknown struct `{}`", name),
            context: HashMap::new(),
            repairs: vec![],
        });
    }

    fn reject_unknown_field(&mut self, id: NodeId, sname: &str, field: &str) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "UNKNOWN_FIELD".into(),
            node: id,
            path: format!("{}.{}", sname, field),
            expected: "a field of the struct".into(),
            received: format!("unknown field `{}`", field),
            context: HashMap::new(),
            repairs: vec![],
        });
    }

    fn reject_duplicate_field(&mut self, id: NodeId, sname: &str, field: &str) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "DUPLICATE_FIELD".into(),
            node: id,
            path: format!("{}.{}", sname, field),
            expected: "each struct field exactly once".into(),
            received: format!("duplicate field `{}`", field),
            context: HashMap::new(),
            repairs: vec![],
        });
    }

    fn reject_missing_field(&mut self, id: NodeId, sname: &str, field: &str) {
                let _rid4 = self.next_repair_id();
self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "MISSING_FIELD".into(),
            node: id,
            path: format!("new-struct {}", sname),
            expected: format!("field `{}`", field),
            received: "absent".into(),
            context: HashMap::new(),
            repairs: vec![Repair {
                id: _rid4,
                action: "add_field".into(),
                with: None,
                cost: 2,
                preserves_effects: true,
                preserves_contracts: true,
                propagates: vec![],
                note: format!("Add a value for field `{}`.", field),
            }],
        });
    }

    fn reject_field_non_struct(&mut self, id: NodeId, ty: &Type) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "FIELD_NON_STRUCT".into(),
            node: id,
            path: "get".into(),
            expected: "(struct Name)".into(),
            received: format!("{:?}", ty),
            context: HashMap::new(),
            repairs: vec![],
        });
    }

    fn reject_contract_not_bool(&mut self, id: NodeId, which: &str, received: &Type) {
        let rid = self.next_repair_id();
        self.rejections.push(Rejection {
            gate: Gate::Contract,
            kind: "CONTRACT_NOT_BOOL".into(),
            node: id,
            path: which.into(),
            expected: "bool".into(),
            received: format!("{:?}", received),
            context: HashMap::new(),
            repairs: vec![Repair {
                id: rid,
                action: "replace_node".into(),
                with: Some(Sexpr::Atom("true".into())),
                cost: 3,
                preserves_effects: true,
                preserves_contracts: false,
                propagates: vec![],
                note: format!(
                    "A `{}` clause must be a bool predicate. Replace it with a \
                     boolean expression over the parameters{}. (The default \
                     `true` type-checks but disables the check.)",
                    which,
                    if which == "ensures" { " and `result`" } else { "" }
                ),
            }],
        });
    }

    fn reject_contract_impure(&mut self, id: NodeId, which: &str) {
        self.rejections.push(Rejection {
            gate: Gate::Contract,
            kind: "CONTRACT_IMPURE".into(),
            node: id,
            path: which.into(),
            expected: "a pure, deterministic predicate".into(),
            received: "clause calls an effectful op (e.g. rng.*)".into(),
            context: HashMap::new(),
            repairs: vec![],
        });
    }

    fn reject_invalid_cast(&mut self, id: NodeId, from: &Type, to: &Type, _val: &Expr) {
        self.rejections.push(Rejection {
            gate: Gate::Type,
            kind: "INVALID_CAST".into(),
            node: id,
            path: "cast".into(),
            expected: format!("castable-to {:?}", to),
            received: format!("{:?}", from),
            context: HashMap::new(),
            repairs: vec![],
        });
    }
}

fn lit_type(l: &Lit) -> Type {
    match l {
        Lit::I64(_) => Type::I64,
        Lit::F64(_) => Type::F64,
        Lit::Bool(_) => Type::Bool,
        Lit::Str(_) => Type::Str,
        Lit::Unit => Type::Unit,
    }
}

/// Build the ranked, admissible repair menu for a type mismatch. These are the
/// repairs the validator *knows* are locally valid:
///   - wrap the value in a known conversion (if one exists)
///   - replace the value with a default literal of the expected type
/// Repairs are ranked by cost (cheaper first).
fn build_type_repairs(c: &mut Checker, id: NodeId, expected: &Type, received: &Type) -> Vec<Repair> {
    let mut repairs = Vec::new();
    // Wrap in a known conversion if one exists between received and expected.
    if let Some(wrap) = known_conversion(received, expected) {
        repairs.push(Repair {
            id: c.next_repair_id(),
            action: "wrap".into(),
            // The `?` atom is a placeholder for the original node; the
            // repair-loop patcher substitutes it with the node being repaired
            // before applying the patch.
            with: Some(Sexpr::List(vec![
                Sexpr::Atom(wrap.clone()),
                Sexpr::Atom("?".into()),
            ])),
            cost: 1,
            preserves_effects: true,
            preserves_contracts: false,
            propagates: vec![],
            note: format!("Wrap the value in a known conversion: {}.", wrap),
        });
    }
    // Replace the node with a default literal of the expected type.
    if let Some(def) = default_literal(expected) {
        repairs.push(Repair {
            id: c.next_repair_id(),
            action: "replace_node".into(),
            with: Some(Sexpr::List(vec![Sexpr::Atom("lit".into()), Sexpr::Atom(def.clone())])),
            cost: 2,
            preserves_effects: true,
            preserves_contracts: false,
            propagates: vec![],
            note: format!("Replace this node with a default literal: {}.", def),
        });
    }
    // Track that this node has had a repair proposed (used by the loop driver
    // to detect cycles on resubmission).
    *c.applied.entry(id).or_insert(0) += 1;
    repairs
}

fn known_conversion(from: &Type, to: &Type) -> Option<String> {
    // A conversion is admissible only if it RETURNS the expected type. We
    // encode the (from, returns) of each builtin and require returns == `to`.
    // i64.parse returns `result i64 str`, NOT i64 — so it is NOT an admissible
    // wrap for str->i64; offering it would create a runaway repair chain
    // (exactly the bug this guard prevents).
    match (from, to) {
        (Type::I64, Type::Str) => Some("i64.to_str".into()), // i64.to_str : i64 -> str
        _ => None,
    }
}

fn default_literal(ty: &Type) -> Option<String> {
    match ty {
        Type::I64 => Some("0".into()),
        Type::F64 => Some("0.0".into()),
        Type::Bool => Some("false".into()),
        Type::Str => Some("\"\"".into()),
        Type::Unit => Some("unit".into()),
        _ => None,
    }
}
