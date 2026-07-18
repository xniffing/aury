//! Tree-walking interpreter. This is the v0 execution backend: it runs
//! Aury programs directly so we can execute property tests and contracts.
//! The proposal's real target is MLIR → LLVM lowering; that path is stubbed
//! out and swappable. The interpreter is sufficient to demonstrate the
//! generate → validate → repair → run → test loop.

use crate::ast::*;
use crate::types::Type;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(String),
    Unit,
    Vec(Vec<Value>),
    Struct(String, Vec<(String, Value)>),
    /// A region handle (opaque; rng seed lives here in v0).
    Region(u64),
    /// Result variant — used by parse-style builtins.
    ResultOk(Box<Value>),
    ResultErr(Box<Value>),
}

impl Value {
    pub fn type_of(&self) -> Type {
        match self {
            Value::I64(_) => Type::I64,
            Value::F64(_) => Type::F64,
            Value::Bool(_) => Type::Bool,
            Value::Str(_) => Type::Str,
            Value::Unit => Type::Unit,
            Value::Vec(_) => Type::Vec(Box::new(Type::I64)), // best-effort
            Value::Struct(n, _) => Type::Struct(n.clone()),
            Value::Region(_) => Type::Region,
            Value::ResultOk(v) => Type::Result(Box::new(v.type_of()), Box::new(Type::Str)),
            Value::ResultErr(v) => Type::Result(Box::new(Type::I64), Box::new(v.type_of())),
        }
    }
}

/// Canonical `f64` → decimal string, used everywhere a float is rendered:
/// `f64.to_str`, CLI/`show_value` display, and (reimplemented byte-identically
/// in `runtime/aury_rt.c`) the native/wasm value printer.
///
/// The format is deliberately *not* the prettiest shortest round-trip — it is
/// the one that can be produced identically in Rust and C. Finite values use
/// 17 significant digits in normalized scientific form `d.dddddddddddddddde±dd`
/// (`format!("{:.16e}")` here, `%.16e` there — both correctly rounded, so the
/// digits agree). `NaN`, `inf`, and `-inf` are spelled out explicitly because
/// the two libraries disagree on their default spellings.
pub fn format_f64(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x.is_infinite() {
        return if x < 0.0 { "-inf".to_string() } else { "inf".to_string() };
    }
    let scientific = format!("{:.16e}", x);
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("{:e} always contains an exponent");
    let exponent: i32 = exponent.parse().expect("exponent is an integer");
    format!(
        "{}e{}{:02}",
        mantissa,
        if exponent < 0 { '-' } else { '+' },
        exponent.abs()
    )
}

/// A control-flow signal from evaluating an expression.
enum Flow {
    Value(Value),
    Return(Value),
    /// Exit the nearest enclosing loop with this value (`break`).
    Break(Value),
}

/// Evaluate a child expression, propagating any non-local control flow
/// (`return` or `break`) unchanged. Keeping this as a macro makes every
/// expression arm use the same rule.
macro_rules! value_or_return {
    ($interp:expr, $expr:expr, $scope:expr) => {
        match $interp.eval($expr, $scope)? {
            Flow::Value(value) => value,
            other => return Ok(other),
        }
    };
}

/// A runtime error (trapped condition: divide by zero, out of bounds, etc.).
#[derive(Debug)]
pub struct InterpError(pub String);
impl std::fmt::Display for InterpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "runtime error: {}", self.0)
    }
}
impl std::error::Error for InterpError {}

pub struct Interp {
    pub fns: HashMap<String, FnDef>,
    pub structs: HashMap<String, StructDef>,
    /// A deterministic seed used for the `rng` capability. Same seed + same
    /// program = same result (per the proposal's determinism default).
    pub seed: u64,
    step: u64,
}

impl Interp {
    pub fn new(module: &Module, seed: u64) -> Interp {
        let mut fns = HashMap::new();
        let mut structs = HashMap::new();
        for item in &module.items {
            match item {
                ModuleItem::Fn(f) => {
                    fns.insert(f.name.clone(), f.clone());
                }
                ModuleItem::Struct(s) => {
                    structs.entry(s.name.clone()).or_insert_with(|| s.clone());
                }
                _ => {}
            }
        }
        Interp {
            fns,
            structs,
            seed,
            step: 0,
        }
    }

    /// Call a top-level function by name with arguments.
    pub fn call_fn(&mut self, name: &str, args: Vec<Value>) -> Result<Value, InterpError> {
        let f = self
            .fns
            .get(name)
            .ok_or_else(|| InterpError(format!("unknown function `{}`", name)))?
            .clone();
        if args.len() != f.params.len() {
            return Err(InterpError(format!(
                "arity mismatch calling `{}`: expected {}, got {}",
                name,
                f.params.len(),
                args.len()
            )));
        }
        self.invoke_fn(&f, args)
    }

    /// Run a function's body with its contracts enforced: preconditions are
    /// checked on entry (parameters in scope) and postconditions on exit
    /// (parameters plus the `result` binding in scope). A violated contract
    /// traps like any other runtime error, so contract failures surface both
    /// here and through the intent gate ([`crate::spec`]).
    fn invoke_fn(&mut self, f: &FnDef, args: Vec<Value>) -> Result<Value, InterpError> {
        let mut scope = HashMap::new();
        for (p, a) in f.params.iter().zip(args.into_iter()) {
            scope.insert(p.name.clone(), a);
        }
        for (i, req) in f.requires.iter().enumerate() {
            if !self.eval_predicate(req, &mut scope)? {
                return Err(InterpError(format!(
                    "precondition violated in `{}` (requires clause #{})",
                    f.name,
                    i + 1
                )));
            }
        }
        // A bare `break` at function top level (not inside a loop) is rejected
        // by the validator; treat its value like a normal result if it reaches
        // here.
        let ret = match self.eval(&f.body, &mut scope)? {
            Flow::Value(v) | Flow::Return(v) | Flow::Break(v) => v,
        };
        if !f.ensures.is_empty() {
            scope.insert(RESULT_BINDING.to_string(), ret.clone());
            for (i, ens) in f.ensures.iter().enumerate() {
                if !self.eval_predicate(ens, &mut scope)? {
                    return Err(InterpError(format!(
                        "postcondition violated in `{}` (ensures clause #{})",
                        f.name,
                        i + 1
                    )));
                }
            }
        }
        Ok(ret)
    }

    /// Evaluate a contract expression that must yield a boolean.
    fn eval_predicate(
        &mut self,
        e: &Expr,
        scope: &mut HashMap<String, Value>,
    ) -> Result<bool, InterpError> {
        match self.eval(e, scope)? {
            Flow::Value(Value::Bool(b)) | Flow::Return(Value::Bool(b)) | Flow::Break(Value::Bool(b)) => Ok(b),
            Flow::Value(other) | Flow::Return(other) | Flow::Break(other) => Err(InterpError(format!(
                "contract expression is not bool: {:?}",
                other
            ))),
        }
    }

    fn eval(&mut self, e: &Expr, scope: &mut HashMap<String, Value>) -> Result<Flow, InterpError> {
        match e {
            Expr::Lit { value, .. } => Ok(Flow::Value(lit_to_value(value))),
            Expr::Ref { name, .. } => {
                let v = scope
                    .get(name)
                    .ok_or_else(|| InterpError(format!("unbound: {}", name)))?
                    .clone();
                Ok(Flow::Value(v))
            }
            Expr::Let { name, init, body, .. } => {
                let v = value_or_return!(self, init, scope);
                let previous = scope.insert(name.clone(), v);
                let result = self.eval(body, scope);
                if let Some(previous) = previous {
                    scope.insert(name.clone(), previous);
                } else {
                    scope.remove(name);
                }
                result
            }
            Expr::Call { op, args, .. } => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    let v = value_or_return!(self, a, scope);
                    vals.push(v);
                }
                let v = self.eval_call(op, vals)?;
                Ok(Flow::Value(v))
            }
            Expr::If { cond, then, els, .. } => {
                let c = value_or_return!(self, cond, scope);
                match c {
                    Value::Bool(true) => self.eval(then, scope),
                    Value::Bool(false) => self.eval(els, scope),
                    other => Err(InterpError(format!(
                        "if condition not bool: {:?}",
                        other
                    ))),
                }
            }
            Expr::Match { scrut, arms, .. } => {
                let s = value_or_return!(self, scrut, scope);
                for arm in arms {
                    if let Some(bindings) = match_pattern(&arm.pattern, &s) {
                        // Bind pattern variables in the current scope (saving any
                        // shadowed values) rather than a clone, so a `set` inside
                        // the arm mutates the enclosing binding — matching native
                        // lowering, where arms share the same slots.
                        let mut saved = Vec::new();
                        for (n, v) in bindings {
                            let previous = scope.insert(n.clone(), v);
                            saved.push((n, previous));
                        }
                        let result = self.eval(&arm.body, scope);
                        for (n, previous) in saved.into_iter().rev() {
                            match previous {
                                Some(p) => { scope.insert(n, p); }
                                None => { scope.remove(&n); }
                            }
                        }
                        return result;
                    }
                }
                Err(InterpError("match: no arm matched".into()))
            }
            Expr::Loop { body, .. } => {
                loop {
                    match self.eval(body, scope)? {
                        // `break v` exits the loop, which then yields `v`.
                        Flow::Break(v) => return Ok(Flow::Value(v)),
                        // `return` unwinds past the loop.
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        // A normal iteration value is discarded; loop again.
                        Flow::Value(_) => {}
                    }
                }
            }
            Expr::Break { value, .. } => {
                let v = value_or_return!(self, value, scope);
                Ok(Flow::Break(v))
            }
            Expr::Set { name, value, .. } => {
                let v = value_or_return!(self, value, scope);
                if !scope.contains_key(name) {
                    return Err(InterpError(format!("set of unbound binding `{}`", name)));
                }
                scope.insert(name.clone(), v);
                Ok(Flow::Value(Value::Unit))
            }
            Expr::Return { value, .. } => {
                let v = value_or_return!(self, value, scope);
                Ok(Flow::Return(v))
            }
            Expr::Block { stmts, tail, .. } => {
                for s in stmts {
                    // Propagate any non-local control flow (`return`/`break`).
                    match self.eval(s, scope)? {
                        Flow::Value(_) => {}
                        other => return Ok(other),
                    }
                }
                self.eval(tail, scope)
            }
            Expr::Region { body, .. } => {
                // In v0 a region just evaluates its body; allocations live in
                // the value space and are GC'd by Rust on scope exit.
                self.eval(body, scope)
            }
            Expr::With { body, .. } => {
                // `with` grants capabilities to a lexical scope; the grant is a
                // checking-time concept (v0 capabilities are deterministic), so
                // execution is transparent — evaluate the body.
                self.eval(body, scope)
            }
            Expr::Copy { value, .. } => self.eval(value, scope),
            Expr::VecNew { elems, .. } => {
                let mut vs = Vec::with_capacity(elems.len());
                for el in elems {
                    let v = value_or_return!(self, el, scope);
                    vs.push(v);
                }
                Ok(Flow::Value(Value::Vec(vs)))
            }
            Expr::Index { target, index, .. } => {
                let t = value_or_return!(self, target, scope);
                let i = value_or_return!(self, index, scope);
                let (Value::Vec(vs), Value::I64(idx)) = (&t, &i) else {
                    return Err(InterpError("idx: bad operands".into()));
                };
                if *idx < 0 || *idx as usize >= vs.len() {
                    return Err(InterpError(format!("index out of bounds: {}", idx)));
                }
                Ok(Flow::Value(vs[*idx as usize].clone()))
            }
            Expr::Len { target, .. } => {
                let t = value_or_return!(self, target, scope);
                match t {
                    Value::Vec(vs) => Ok(Flow::Value(Value::I64(vs.len() as i64))),
                    _ => Err(InterpError("len: not a vec".into())),
                }
            }
            Expr::VecPush { target, value, .. } => {
                let t = value_or_return!(self, target, scope);
                let v = value_or_return!(self, value, scope);
                match t {
                    Value::Vec(mut vs) => {
                        vs.push(v);
                        Ok(Flow::Value(Value::Vec(vs)))
                    }
                    _ => Err(InterpError("vec-push: not a vec".into())),
                }
            }
            Expr::StructNew { name, fields, .. } => {
                // Evaluate source expressions left-to-right, then normalize the
                // immutable value to declared-field order (the native slot ABI).
                let mut evaluated = Vec::new();
                for (fname, fval) in fields {
                    let v = value_or_return!(self, fval, scope);
                    evaluated.push((fname.clone(), v));
                }
                let definition = self.structs.get(name)
                    .ok_or_else(|| InterpError(format!("unknown struct {}", name)))?;
                let vals = definition.fields.iter().map(|(field, _)| {
                    evaluated.iter().find(|(candidate, _)| candidate == field)
                        .map(|(_, value)| (field.clone(), value.clone()))
                        .ok_or_else(|| InterpError(format!("missing field {}", field)))
                }).collect::<Result<Vec<_>, _>>()?;
                Ok(Flow::Value(Value::Struct(name.clone(), vals)))
            }
            Expr::Field { target, field, .. } => {
                let t = value_or_return!(self, target, scope);
                match t {
                    Value::Struct(_, fs) => {
                        for (n, v) in fs {
                            if &n == field {
                                return Ok(Flow::Value(v));
                            }
                        }
                        Err(InterpError(format!("no field {}", field)))
                    }
                    _ => Err(InterpError("get: not a struct".into())),
                }
            }
            Expr::Cast { target, value, .. } => {
                let v = value_or_return!(self, value, scope);
                self.eval_cast(target, v)
            }
        }
    }

    fn eval_call(&mut self, op: &str, args: Vec<Value>) -> Result<Value, InterpError> {
        // Builtins.
        let r = match op {
            "i64.add" => bin_i64(&args, |a, b| a.wrapping_add(b))?,
            "i64.sub" => bin_i64(&args, |a, b| a.wrapping_sub(b))?,
            "i64.mul" => bin_i64(&args, |a, b| a.wrapping_mul(b))?,
            "i64.div" => {
                let (a, b) = two_i64(&args)?;
                if b == 0 {
                    return Err(InterpError("divide by zero".into()));
                }
                Value::I64(a.wrapping_div(b))
            }
            "i64.mod" => {
                let (a, b) = two_i64(&args)?;
                if b == 0 {
                    return Err(InterpError("mod by zero".into()));
                }
                Value::I64(a.wrapping_rem(b))
            }
            "i64.gt" => Value::Bool(two_i64(&args)?.0 > two_i64(&args)?.1),
            "i64.lt" => Value::Bool(two_i64(&args)?.0 < two_i64(&args)?.1),
            "i64.ge" => Value::Bool(two_i64(&args)?.0 >= two_i64(&args)?.1),
            "i64.le" => Value::Bool(two_i64(&args)?.0 <= two_i64(&args)?.1),
            "i64.eq" => Value::Bool(two_i64(&args)?.0 == two_i64(&args)?.1),
            "i64.neq" => Value::Bool(two_i64(&args)?.0 != two_i64(&args)?.1),
            "i64.neg" => Value::I64(one_i64(&args)?.wrapping_neg()),
            "i64.abs" => Value::I64(one_i64(&args)?.wrapping_abs()),
            "i64.from_str" | "i64.parse" => match &args[0] {
                Value::Str(s) => match s.trim().parse::<i64>() {
                    Ok(n) => Value::ResultOk(Box::new(Value::I64(n))),
                    Err(_) => Value::ResultErr(Box::new(Value::Str(format!(
                        "not an i64: {}",
                        s
                    )))),
                },
                _ => return Err(InterpError("parse: not a str".into())),
            },
            "i64.to_str" => Value::Str(one_i64(&args)?.to_string()),
            // ---- f64 builtins ----
            // All arithmetic is IEEE-754 and never traps: `f64.div` by zero
            // yields ±inf (or NaN for 0.0/0.0), matching LLVM `fdiv` and C.
            "f64.add" => bin_f64(&args, |a, b| a + b)?,
            "f64.sub" => bin_f64(&args, |a, b| a - b)?,
            "f64.mul" => bin_f64(&args, |a, b| a * b)?,
            "f64.div" => bin_f64(&args, |a, b| a / b)?,
            "f64.neg" => Value::F64(-one_f64(&args)?),
            "f64.abs" => Value::F64(one_f64(&args)?.abs()),
            // Comparisons follow IEEE ordering: any comparison involving NaN is
            // false, except `f64.neq`, which is true (NaN != anything).
            "f64.gt" => Value::Bool(two_f64(&args)?.0 > two_f64(&args)?.1),
            "f64.lt" => Value::Bool(two_f64(&args)?.0 < two_f64(&args)?.1),
            "f64.ge" => Value::Bool(two_f64(&args)?.0 >= two_f64(&args)?.1),
            "f64.le" => Value::Bool(two_f64(&args)?.0 <= two_f64(&args)?.1),
            "f64.eq" => Value::Bool(two_f64(&args)?.0 == two_f64(&args)?.1),
            "f64.neq" => Value::Bool(two_f64(&args)?.0 != two_f64(&args)?.1),
            "f64.to_str" => Value::Str(format_f64(one_f64(&args)?)),
            "bool.and" => Value::Bool(two_bool(&args)?.0 && two_bool(&args)?.1),
            "bool.or" => Value::Bool(two_bool(&args)?.0 || two_bool(&args)?.1),
            "bool.not" => Value::Bool(!one_bool(&args)?),
            "bool.eq" => Value::Bool(two_bool(&args)?.0 == two_bool(&args)?.1),
            "str.eq" => Value::Bool(str_arg(&args, 0) == str_arg(&args, 1)),
            "str.neq" => Value::Bool(str_arg(&args, 0) != str_arg(&args, 1)),
            "str.concat" => Value::Str(format!("{}{}", str_arg(&args, 0), str_arg(&args, 1))),
            "str.len" => Value::I64(str_arg(&args, 0).len() as i64),
            "result.is_ok" => match &args[0] {
                Value::ResultOk(_) => Value::Bool(true),
                Value::ResultErr(_) => Value::Bool(false),
                _ => return Err(InterpError("result.is_ok: not a result".into())),
            },
            "rng.next" | "rng.i64" => {
                if !args.is_empty() {
                    return Err(InterpError(format!(
                        "arity mismatch in `{}`: expected 0 got {}",
                        op,
                        args.len()
                    )));
                }
                let v = self.next_rand();
                Value::I64(v as i64)
            }
            "log.i64" => {
                // v0.3 Track A: `log` is a lexically-scoped capability (gated by
                // the `with` scope at check time). The interpreter is the
                // semantic reference: logging is modeled deterministically as an
                // identity with a side effect — it yields its argument so it
                // composes in expression position. Real emission plus native/wasm
                // parity arrive in Track B.
                match args.first() {
                    Some(Value::I64(n)) => Value::I64(*n),
                    _ => return Err(InterpError("log.i64: expected one i64 argument".into())),
                }
            }
            _ => {
                // User function call.
                let f = self
                    .fns
                    .get(op)
                    .ok_or_else(|| InterpError(format!("unknown op `{}`", op)))?
                    .clone();
                if args.len() != f.params.len() {
                    return Err(InterpError(format!(
                        "arity mismatch in `{}`: expected {} got {}",
                        op,
                        f.params.len(),
                        args.len()
                    )));
                }
                self.invoke_fn(&f, args)?
            }
        };
        Ok(r)
    }

    fn eval_cast(&self, target: &Type, v: Value) -> Result<Flow, InterpError> {
        match (target, &v) {
            (Type::I64, Value::I64(_)) => Ok(Flow::Value(v)),
            (Type::I64, Value::Str(s)) => match s.parse::<i64>() {
                Ok(n) => Ok(Flow::Value(Value::I64(n))),
                Err(_) => Err(InterpError(format!("cast str->i64: {}", s))),
            },
            (Type::Str, Value::I64(n)) => Ok(Flow::Value(Value::Str(n.to_string()))),
            (Type::Str, Value::Str(_)) => Ok(Flow::Value(v)),
            // Numeric casts. i64->f64 rounds to nearest (exact for small ints);
            // f64->i64 truncates toward zero and *saturates* — NaN->0, out of
            // range clamps to i64::MIN/MAX — matching Rust's `as` and the C
            // `aury_f64_to_i64` helper used by the native backend.
            (Type::F64, Value::F64(_)) => Ok(Flow::Value(v)),
            (Type::F64, Value::I64(n)) => Ok(Flow::Value(Value::F64(*n as f64))),
            (Type::I64, Value::F64(x)) => Ok(Flow::Value(Value::I64(*x as i64))),
            (Type::Str, Value::F64(x)) => Ok(Flow::Value(Value::Str(format_f64(*x)))),
            _ => Err(InterpError(format!("cast not supported: {:?} <- {:?}", target, v))),
        }
    }

    fn next_rand(&mut self) -> u64 {
        // splitmix64, seeded by self.seed; deterministic.
        self.step = self.step.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.seed.wrapping_add(self.step);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
}

fn lit_to_value(l: &Lit) -> Value {
    match l {
        Lit::I64(n) => Value::I64(*n),
        Lit::F64(bits) => Value::F64(f64::from_bits(*bits)),
        Lit::Bool(b) => Value::Bool(*b),
        Lit::Str(s) => Value::Str(s.clone()),
        Lit::Unit => Value::Unit,
    }
}

fn match_pattern(p: &Pattern, v: &Value) -> Option<Vec<(String, Value)>> {
    match p {
        Pattern::Wild => Some(vec![]),
        Pattern::Bind(n) => Some(vec![(n.clone(), v.clone())]),
        Pattern::Lit(l) => {
            let lv = lit_to_value(l);
            if value_eq(&lv, v) {
                Some(vec![])
            } else {
                None
            }
        }
    }
}

fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::I64(x), Value::I64(y)) => x == y,
        (Value::F64(x), Value::F64(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Unit, Value::Unit) => true,
        _ => false,
    }
}

fn one_i64(args: &[Value]) -> Result<i64, InterpError> {
    match args.get(0) {
        Some(Value::I64(n)) => Ok(*n),
        _ => Err(InterpError("expected i64".into())),
    }
}
fn two_i64(args: &[Value]) -> Result<(i64, i64), InterpError> {
    Ok((one_i64(&args[..1])?, one_i64(&args[1..])?))
}
fn bin_i64(args: &[Value], f: impl Fn(i64, i64) -> i64) -> Result<Value, InterpError> {
    let (a, b) = two_i64(args)?;
    Ok(Value::I64(f(a, b)))
}
fn one_f64(args: &[Value]) -> Result<f64, InterpError> {
    match args.get(0) {
        Some(Value::F64(x)) => Ok(*x),
        _ => Err(InterpError("expected f64".into())),
    }
}
fn two_f64(args: &[Value]) -> Result<(f64, f64), InterpError> {
    Ok((one_f64(&args[..1])?, one_f64(&args[1..])?))
}
fn bin_f64(args: &[Value], f: impl Fn(f64, f64) -> f64) -> Result<Value, InterpError> {
    let (a, b) = two_f64(args)?;
    Ok(Value::F64(f(a, b)))
}
fn one_bool(args: &[Value]) -> Result<bool, InterpError> {
    match args.get(0) {
        Some(Value::Bool(b)) => Ok(*b),
        _ => Err(InterpError("expected bool".into())),
    }
}
fn two_bool(args: &[Value]) -> Result<(bool, bool), InterpError> {
    Ok((one_bool(&args[..1])?, one_bool(&args[1..])?))
}
fn str_arg(args: &[Value], i: usize) -> String {
    match args.get(i) {
        Some(Value::Str(s)) => s.clone(),
        _ => String::new(),
    }
}
