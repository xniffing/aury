//! Aury types. Everything is explicit — no inference, no subtyping beyond
//! declared numeric casts. Types appear literally in source.

use std::fmt;

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Type {
    I64,
    Bool,
    Str,
    Unit,
    Vec(Box<Type>),
    Struct(String),
    /// A reference: names its region explicitly, its mutability, and the
    /// pointee type. No elided lifetimes, no variance subtleties.
    Ref {
        region: String,
        mutable: bool,
        ty: Box<Type>,
    },
    /// A region value (passed around as a capability).
    Region,
    /// The result of a fallible operation: value or an error code.
    Result(Box<Type>, Box<Type>),
}

impl fmt::Debug for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::I64 => write!(f, "i64"),
            Type::Bool => write!(f, "bool"),
            Type::Str => write!(f, "str"),
            Type::Unit => write!(f, "unit"),
            Type::Vec(t) => write!(f, "(vec {:?})", t),
            Type::Struct(n) => write!(f, "(struct {})", n),
            Type::Ref { region, mutable, ty } => {
                let m = if *mutable { "mut" } else { "ref" };
                write!(f, "(ref {} {} {:?})", region, m, ty)
            }
            Type::Region => write!(f, "region"),
            Type::Result(ok, err) => write!(f, "(result {:?} {:?})", ok, err),
        }
    }
}

impl Type {
    pub fn parse(s: &crate::sexpr::Sexpr) -> Result<Type, String> {
        use crate::sexpr::Sexpr;
        match s {
            Sexpr::Atom(a) => match a.as_str() {
                "i64" => Ok(Type::I64),
                "bool" => Ok(Type::Bool),
                "str" => Ok(Type::Str),
                "unit" => Ok(Type::Unit),
                "region" => Ok(Type::Region),
                other => Err(format!("unknown type atom: {}", other)),
            },
            Sexpr::List(xs) => {
                let head = xs
                    .first()
                    .and_then(|x| x.atom())
                    .ok_or_else(|| "empty type".to_string())?;
                match head {
                    "vec" => {
                        if xs.len() != 2 {
                            return Err("(vec T) needs one type arg".into());
                        }
                        Ok(Type::Vec(Box::new(Type::parse(&xs[1])?)))
                    }
                    "struct" => {
                        let n = xs
                            .get(1)
                            .and_then(|x| x.atom())
                            .ok_or_else(|| "(struct Name)".to_string())?;
                        Ok(Type::Struct(n.to_string()))
                    }
                    "ref" => {
                        if xs.len() != 4 {
                            return Err("(ref region mut/ref T) needs 3 args".into());
                        }
                        let region = xs[1].atom().ok_or("region name")?.to_string();
                        let m = match xs[2].atom() {
                            Some("mut") => true,
                            Some("ref") => false,
                            _ => return Err("mutability must be mut or ref".into()),
                        };
                        let ty = Type::parse(&xs[3])?;
                        Ok(Type::Ref {
                            region,
                            mutable: m,
                            ty: Box::new(ty),
                        })
                    }
                    "result" => {
                        if xs.len() != 3 {
                            return Err("(result OkT ErrT)".into());
                        }
                        let ok = Type::parse(&xs[1])?;
                        let err = Type::parse(&xs[2])?;
                        Ok(Type::Result(Box::new(ok), Box::new(err)))
                    }
                    other => Err(format!("unknown type form: {}", other)),
                }
            }
        }
    }
}

/// Effect rows. Effects are part of a function's type. A function may declare
/// `pure`, or a set of capabilities it requires.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct EffectRow {
    pub pure: bool,
    /// capabilities, e.g. "fs read", "net", "clock"
    pub caps: Vec<String>,
}

impl fmt::Debug for EffectRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.pure {
            write!(f, "(pure)")
        } else {
            write!(f, "(effects {})", self.caps.join(" "))
        }
    }
}

impl EffectRow {
    pub fn parse(s: &crate::sexpr::Sexpr) -> Result<EffectRow, String> {
        use crate::sexpr::Sexpr;
        match s {
            Sexpr::Atom(a) if a == "pure" => Ok(EffectRow {
                pure: true,
                caps: vec![],
            }),
            Sexpr::List(xs) => {
                let head = xs
                    .first()
                    .and_then(|x| x.atom())
                    .ok_or_else(|| "empty effect".to_string())?;
                let mut caps = Vec::new();
                for x in &xs[1..] {
                    match x {
                        Sexpr::Atom(a) => caps.push(a.clone()),
                        Sexpr::List(inner) => {
                            // (fs read) style capability with sub-flags
                            let parts: Vec<&str> = inner
                                .iter()
                                .filter_map(|y| y.atom())
                                .collect();
                            if parts.is_empty() {
                                return Err("empty capability".into());
                            }
                            caps.push(parts.join(" "));
                        }
                    }
                }
                match head {
                    "pure" => {
                        if caps.is_empty() {
                            Ok(EffectRow { pure: true, caps })
                        } else {
                            Err("pure takes no caps".into())
                        }
                    }
                    "effects" => Ok(EffectRow { pure: false, caps }),
                    _ => Err(format!("unknown effect form: {}", head)),
                }
            }
            _ => Err("bad effect row".into()),
        }
    }

    pub fn is_pure(&self) -> bool {
        self.pure
    }

    /// True if `other`'s requirements are a subset of `self`'s caps.
    pub fn admits(&self, other: &EffectRow) -> bool {
        if other.pure {
            return true;
        }
        if self.pure {
            return other.caps.is_empty();
        }
        other.caps.iter().all(|c| self.caps.contains(c))
    }

    pub fn union_with(&self, other: &EffectRow) -> EffectRow {
        if self.pure && other.pure {
            return EffectRow::pure_row();
        }
        let mut caps = self.caps.clone();
        for c in &other.caps {
            if !caps.contains(c) {
                caps.push(c.clone());
            }
        }
        EffectRow { pure: false, caps }
    }
}

impl EffectRow {
    pub fn pure_row() -> EffectRow {
        EffectRow { pure: true, caps: vec![] }
    }
}