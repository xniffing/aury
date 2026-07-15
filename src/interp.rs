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

/// A control-flow signal from evaluating an expression.
enum Flow {
    Value(Value),
    Return(Value),
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
                    structs.insert(s.name.clone(), s.clone());
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
        let mut scope = HashMap::new();
        for (p, a) in f.params.iter().zip(args.into_iter()) {
            scope.insert(p.name.clone(), a);
        }
        match self.eval(&f.body, &mut scope)? {
            Flow::Value(v) | Flow::Return(v) => Ok(v),
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
                let v = match self.eval(init, scope)? {
                    Flow::Value(v) | Flow::Return(v) => v,
                };
                scope.insert(name.clone(), v);
                let r = self.eval(body, scope);
                scope.remove(name);
                r
            }
            Expr::Call { op, args, .. } => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    let v = match self.eval(a, scope)? {
                        Flow::Value(v) | Flow::Return(v) => v,
                    };
                    vals.push(v);
                }
                let v = self.eval_call(op, vals)?;
                Ok(Flow::Value(v))
            }
            Expr::If { cond, then, els, .. } => {
                let c = match self.eval(cond, scope)? {
                    Flow::Value(v) | Flow::Return(v) => v,
                };
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
                let s = match self.eval(scrut, scope)? {
                    Flow::Value(v) | Flow::Return(v) => v,
                };
                for arm in arms {
                    if let Some(bindings) = match_pattern(&arm.pattern, &s) {
                        let mut arm_scope = scope.clone();
                        for (n, v) in bindings {
                            arm_scope.insert(n, v);
                        }
                        return self.eval(&arm.body, &mut arm_scope);
                    }
                }
                Err(InterpError("match: no arm matched".into()))
            }
            Expr::Loop { body, .. } => {
                loop {
                    match self.eval(body, scope)? {
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Value(_) => {}
                    }
                }
            }
            Expr::Return { value, .. } => {
                let v = match self.eval(value, scope)? {
                    Flow::Value(v) | Flow::Return(v) => v,
                };
                Ok(Flow::Return(v))
            }
            Expr::Block { stmts, tail, .. } => {
                for s in stmts {
                    match self.eval(s, scope)? {
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Value(_) => {}
                    }
                }
                self.eval(tail, scope)
            }
            Expr::Region { body, .. } => {
                // In v0 a region just evaluates its body; allocations live in
                // the value space and are GC'd by Rust on scope exit.
                self.eval(body, scope)
            }
            Expr::Copy { value, .. } => self.eval(value, scope),
            Expr::VecNew { elems, .. } => {
                let mut vs = Vec::with_capacity(elems.len());
                for el in elems {
                    let v = match self.eval(el, scope)? {
                        Flow::Value(v) | Flow::Return(v) => v,
                    };
                    vs.push(v);
                }
                Ok(Flow::Value(Value::Vec(vs)))
            }
            Expr::Index { target, index, .. } => {
                let t = match self.eval(target, scope)? {
                    Flow::Value(v) | Flow::Return(v) => v,
                };
                let i = match self.eval(index, scope)? {
                    Flow::Value(v) | Flow::Return(v) => v,
                };
                let (Value::Vec(vs), Value::I64(idx)) = (&t, &i) else {
                    return Err(InterpError("idx: bad operands".into()));
                };
                if *idx < 0 || *idx as usize >= vs.len() {
                    return Err(InterpError(format!("index out of bounds: {}", idx)));
                }
                Ok(Flow::Value(vs[*idx as usize].clone()))
            }
            Expr::Len { target, .. } => {
                let t = match self.eval(target, scope)? {
                    Flow::Value(v) | Flow::Return(v) => v,
                };
                match t {
                    Value::Vec(vs) => Ok(Flow::Value(Value::I64(vs.len() as i64))),
                    _ => Err(InterpError("len: not a vec".into())),
                }
            }
            Expr::StructNew { name, fields, .. } => {
                let mut vals = Vec::new();
                for (fname, fval) in fields {
                    let v = match self.eval(fval, scope)? {
                        Flow::Value(v) | Flow::Return(v) => v,
                    };
                    vals.push((fname.clone(), v));
                }
                Ok(Flow::Value(Value::Struct(name.clone(), vals)))
            }
            Expr::Field { target, field, .. } => {
                let t = match self.eval(target, scope)? {
                    Flow::Value(v) | Flow::Return(v) => v,
                };
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
                let v = match self.eval(value, scope)? {
                    Flow::Value(v) | Flow::Return(v) => v,
                };
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
            "i64.neg" => Value::I64(-one_i64(&args)?),
            "i64.abs" => Value::I64(one_i64(&args)?.abs()),
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
                // consume a region arg, ignore it; produce deterministic next.
                let v = self.next_rand();
                Value::I64(v as i64)
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
                let mut scope = HashMap::new();
                for (p, a) in f.params.iter().zip(args.into_iter()) {
                    scope.insert(p.name.clone(), a);
                }
                match self.eval(&f.body, &mut scope)? {
                    Flow::Value(v) | Flow::Return(v) => v,
                }
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