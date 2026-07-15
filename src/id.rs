//! Content-addressed Merkle node IDs.
//!
//! Every AST node's id is the SHA-256 of a canonical serialization of its
//! kind + its children's ids + its key attributes. The same source always
//! yields the same ids, on any machine. Repair patches reference nodes by id.

use sha2::{Digest, Sha256};
use crate::sexpr::Sexpr;

/// A 16-char hex prefix of SHA-256. For v0 this is plenty of collision
/// resistance for repair-keying; a production version would use the full
/// digest or a 128-bit truncation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct NodeId(pub [u8; 8]);

impl NodeId {
    pub fn of(bytes: &[u8]) -> NodeId {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let out = hasher.finalize();
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&out[..8]);
        NodeId(buf)
    }

    pub fn hex(&self) -> String {
        let mut s = String::with_capacity(16);
        for b in &self.0 {
            s.push_str(&format!("{:02x}", b));
        }
        s
    }

    pub fn nil() -> NodeId {
        NodeId([0; 8])
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.hex())
    }
}

impl std::fmt::Debug for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "m1:{}", &self.hex()[..6])
    }
}

/// Build a node id from a tag and child ids. Attributes are folded in as
/// bytes via [`IdBuilder`].
pub fn make_id(tag: &str, parts: &[&[u8]]) -> NodeId {
    let mut buf = Vec::new();
    buf.extend_from_slice(tag.as_bytes());
    buf.push(0);
    for p in parts {
        buf.extend_from_slice(p);
        buf.push(0);
    }
    NodeId::of(&buf)
}

/// Helper to accumulate attributes into a canonical byte sequence for hashing.
pub struct IdBuilder {
    buf: Vec<u8>,
}

impl IdBuilder {
    pub fn new(tag: &str) -> Self {
        let mut buf = Vec::new();
        buf.extend_from_slice(tag.as_bytes());
        buf.push(0);
        IdBuilder { buf }
    }
    pub fn str(&mut self, s: &str) -> &mut Self {
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
        self
    }
    pub fn id(&mut self, id: NodeId) -> &mut Self {
        self.buf.extend_from_slice(&id.0);
        self.buf.push(0);
        self
    }
    pub fn ids(&mut self, ids: &[NodeId]) -> &mut Self {
        for i in ids {
            self.buf.extend_from_slice(&i.0);
            self.buf.push(0);
        }
        self
    }
    pub fn finish(&self) -> NodeId {
        NodeId::of(&self.buf)
    }
}

/// Compute a node id by hashing the *raw s-expression form*. This is the
/// content-addressed id the parser assigns: identical source forms yield
/// identical ids, on any machine. Crucially, the repair-loop patcher hashes
/// sub-forms of the source with the *same* function, so a rejection's node id
/// (computed here during AST building) can be located in the source and
/// replaced. This keeps the validator and the patcher in agreement without
/// carrying ids inline in the source text.
pub fn sexpr_id(s: &Sexpr) -> NodeId {
    let mut h = Sha256::new();
    sexpr_hash_into(s, &mut h);
    let out = h.finalize();
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&out[..8]);
    NodeId(buf)
}

fn sexpr_hash_into(s: &Sexpr, h: &mut Sha256) {
    match s {
        Sexpr::Atom(a) => {
            h.update(b"A");
            h.update(a.as_bytes());
            h.update(b"\0");
        }
        Sexpr::List(xs) => {
            h.update(b"L");
            h.update(&(xs.len() as u64).to_le_bytes());
            for x in xs {
                sexpr_hash_into(x, h);
            }
            h.update(b"\0");
        }
    }
}