//! S-expression parser. Aury's canonical surface is s-expressions; the
//! parser is a trivial one-screen reader with no ambiguity (no infix, no
//! precedence, no significant whitespace, no semicolons).
//!
//! The parser produces a raw [`Sexpr`] tree (atoms and lists). Conversion
//! into the typed AST + Merkle ids lives in [`crate::ast`].

use std::fmt;

#[derive(Clone, PartialEq, Eq)]
pub enum Sexpr {
    Atom(String),
    List(Vec<Sexpr>),
}

impl fmt::Debug for Sexpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Sexpr::Atom(a) => {
                if needs_quote(a) {
                    write!(f, "{}", quote(a))
                } else {
                    write!(f, "{}", a)
                }
            }
            Sexpr::List(xs) => {
                write!(f, "(")?;
                for (i, x) in xs.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{:?}", x)?;
                }
                write!(f, ")")
            }
        }
    }
}

impl Sexpr {
    pub fn list(&self) -> Option<&[Sexpr]> {
        match self {
            Sexpr::List(xs) => Some(xs),
            _ => None,
        }
    }
    pub fn atom(&self) -> Option<&str> {
        match self {
            Sexpr::Atom(a) => Some(a),
            _ => None,
        }
    }
    pub fn head(&self) -> Option<&str> {
        match self {
            Sexpr::List(xs) => xs.first().and_then(|x| x.atom()),
            _ => None,
        }
    }

    /// Pretty-print with indentation.
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.pretty_into(&mut out, 0);
        out
    }
    fn pretty_into(&self, out: &mut String, indent: usize) {
        match self {
            Sexpr::Atom(a) => out.push_str(a),
            Sexpr::List(xs) => {
                if xs.is_empty() {
                    out.push_str("()");
                    return;
                }
                out.push('(');
                out.push_str(xs[0].debug_str().as_str());
                let mut same_line = true;
                for x in &xs[1..] {
                    if let Sexpr::List(_) = x {
                        same_line = false;
                        break;
                    }
                }
                if same_line {
                    for x in &xs[1..] {
                        out.push(' ');
                        out.push_str(&x.debug_str());
                    }
                    out.push(')');
                } else {
                    out.push('\n');
                    for x in &xs[1..] {
                        for _ in 0..indent + 2 {
                            out.push(' ');
                        }
                        x.pretty_into(out, indent + 2);
                        out.push('\n');
                    }
                    for _ in 0..indent {
                        out.push(' ');
                    }
                    out.push(')');
                }
            }
        }
    }
    fn debug_str(&self) -> String {
        format!("{:?}", self)
    }
}

fn needs_quote(a: &str) -> bool {
    a.is_empty()
        || a.chars().any(|c| {
            c.is_whitespace() || c == '(' || c == ')' || c == '"' || c == ';' || c == '\\'
        })
}
fn quote(a: &str) -> String {
    let mut s = String::from("\"");
    for c in a.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            _ => s.push(c),
        }
    }
    s.push('"');
    s
}

#[derive(Debug)]
pub struct ParseError {
    pub msg: String,
    pub pos: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error at byte {}: {}", self.pos, self.msg)
    }
}
impl std::error::Error for ParseError {}

pub fn parse(input: &str) -> Result<Vec<Sexpr>, ParseError> {
    let mut p = Parser {
        bytes: input.as_bytes(),
        pos: 0,
    };
    let mut out = Vec::new();
    p.skip_ws();
    while p.pos < p.bytes.len() {
        out.push(p.parse_one()?);
        p.skip_ws();
    }
    Ok(out)
}

/// Count of currently-unmatched `(` in a source string — the number of `)`
/// that must be appended to make it balance. Used by the repair loop's parse
/// gate: the most common authoring error (the one this whole design exists
/// to absorb) is forgetting to close nested forms, and appending the deficit
/// is an admissible mechanical repair for that class.
pub fn paren_deficit(input: &str) -> usize {
    let mut depth: i64 = 0;
    let mut deficit: usize = 0;
    let mut in_str = false;
    let mut in_comment = false;
    let mut prev = b' ';
    for &c in input.as_bytes() {
        if in_comment {
            if c == b'\n' {
                in_comment = false;
            }
            prev = c;
            continue;
        }
        if in_str {
            if c == b'"' && prev != b'\\' {
                in_str = false;
            }
            prev = c;
            continue;
        }
        if c == b';' && prev != b'"' {
            in_comment = true;
            prev = c;
            continue;
        }
        if c == b'"' {
            in_str = true;
            prev = c;
            continue;
        }
        if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
            if depth < 0 {
                deficit += 1;
                depth = 0;
            }
        }
        prev = c;
    }
    if depth > 0 {
        deficit += depth as usize;
    }
    deficit
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        loop {
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            if self.pos + 1 < self.bytes.len() && self.bytes[self.pos] == b';' {
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn err<T>(&self, msg: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            msg: msg.into(),
            pos: self.pos,
        })
    }

    fn parse_one(&mut self) -> Result<Sexpr, ParseError> {
        self.skip_ws();
        if self.pos >= self.bytes.len() {
            return self.err("unexpected end of input");
        }
        match self.bytes[self.pos] {
            b'(' => self.parse_list(),
            b')' => self.err("unexpected ')'"),
            b'"' => self.parse_string(),
            _ => self.parse_atom(),
        }
    }

    fn parse_list(&mut self) -> Result<Sexpr, ParseError> {
        self.pos += 1; // (
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            if self.pos >= self.bytes.len() {
                return self.err("unterminated list");
            }
            if self.bytes[self.pos] == b')' {
                self.pos += 1;
                return Ok(Sexpr::List(items));
            }
            items.push(self.parse_one()?);
        }
    }

    fn parse_string(&mut self) -> Result<Sexpr, ParseError> {
        self.pos += 1; // opening quote
        let mut s = String::new();
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            self.pos += 1;
            match c {
                b'"' => return Ok(Sexpr::Atom(s)),
                b'\\' => {
                    if self.pos >= self.bytes.len() {
                        return self.err("unterminated string escape");
                    }
                    let e = self.bytes[self.pos];
                    self.pos += 1;
                    match e {
                        b'n' => s.push('\n'),
                        b't' => s.push('\t'),
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        _ => s.push(e as char),
                    }
                }
                _ => s.push(c as char),
            }
        }
        self.err("unterminated string")
    }

    fn parse_atom(&mut self) -> Result<Sexpr, ParseError> {
        let start = self.pos;
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if c.is_ascii_whitespace() || c == b'(' || c == b')' || c == b';' || c == b'"' {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return self.err("empty atom");
        }
        Ok(Sexpr::Atom(
            std::str::from_utf8(&self.bytes[start..self.pos])
                .map_err(|e| ParseError { msg: e.to_string(), pos: start })?
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let xs = parse("(module m (fn add (params (a i64) (b i64)) (ret i64) (body (call i64.add (ref a) (ref b)))))").unwrap();
        assert_eq!(xs.len(), 1);
        assert_eq!(xs[0].head(), Some("module"));
    }

    #[test]
    fn parse_strings() {
        let xs = parse(r#"(lit "hello world")"#).unwrap();
        match &xs[0] {
            Sexpr::List(items) => {
                assert_eq!(items[1].atom(), Some("hello world"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parse_comments() {
        let xs = parse("; a comment\n(fn) ; trailing\n").unwrap();
        assert_eq!(xs.len(), 1);
    }
}