// Loads the Aury-compiled calculator.wasm (a wasm32-wasi *reactor* module) and
// exposes each exported function as a plain JS function over BigInt.
//
// The Aury source (projects/calculator/calculator.json) is pure i64/bool, so
// every export crosses the boundary as a wasm i64 — a JS BigInt — with no
// linear-memory marshaling. The module links with zero imports, so it needs no
// WASI shim: we instantiate it with an empty import object.

// Names as they appear in the Aury program. Each is exported as `aury__<name>`.
const BINARY = [
  "add", "subtract", "multiply", "divide", "modulo",
  "percent", "average", "maximum", "minimum", "gcd", "lcm", "power",
];
const UNARY = [
  "negate", "absolute", "square", "increment", "decrement", "double",
  "factorial", "fibonacci", "isqrt",
];
const PREDICATE = ["is_even", "is_prime"]; // return bool -> BigInt 0n/1n

let exportsPromise = null;

async function loadExports() {
  const response = await fetch("/calculator.wasm", { cache: "no-store" });
  if (!response.ok) throw new Error(`wasm module HTTP ${response.status}`);
  const { instance } = await WebAssembly.instantiate(await response.arrayBuffer(), {});
  // Reactor modules run their ctors via _initialize before the first call.
  instance.exports._initialize?.();
  return instance.exports;
}

// Resolve a single exported Aury function, wrapping BigInt marshaling.
function bind(exports, name, arity) {
  const raw = exports[`aury__${name}`];
  if (typeof raw !== "function") {
    throw new Error(`wasm module is missing export aury__${name}`);
  }
  return (...args) => {
    if (args.length !== arity) {
      throw new Error(`${name} expects ${arity} argument(s), got ${args.length}`);
    }
    return raw(...args.map((a) => BigInt(a)));
  };
}

// Instantiate once and return a { name -> fn } map plus metadata about arity/kind.
export async function loadCalculator() {
  if (!exportsPromise) exportsPromise = loadExports();
  const exports = await exportsPromise;

  const fns = {};
  const meta = {};
  for (const name of BINARY) {
    fns[name] = bind(exports, name, 2);
    meta[name] = { arity: 2, kind: "binary" };
  }
  for (const name of UNARY) {
    fns[name] = bind(exports, name, 1);
    meta[name] = { arity: 1, kind: "unary" };
  }
  for (const name of PREDICATE) {
    const inner = bind(exports, name, 1);
    fns[name] = (n) => inner(n) === 1n; // bool as JS boolean
    meta[name] = { arity: 1, kind: "predicate" };
  }
  return { fns, meta };
}
