// Loads the Aury-compiled calculator.wasm (a wasm32-wasi *reactor* module) and
// exposes each exported function as a plain JS function with correct marshaling.
//
// The Aury source (projects/calculator/calculator.json) is a mix of i64/bool and
// f64. Across the wasm boundary:
//   - i64 and bool  cross as wasm i64  -> JS BigInt
//   - f64           crosses as wasm f64 -> JS Number   (a real wasm scalar, not a
//                       linear-memory pointer, despite the wasm-lib note)
// So each export needs per-argument type marshaling: i64/bool args are passed as
// BigInt, f64 args as Number. The module links with zero imports, so it needs no
// WASI shim: we instantiate it with an empty import object.
//
// SIGNATURES maps each export to { args: [type...], ret: type }. `bind` uses it
// to marshal call args and to coerce the return (bool returns come back as
// BigInt 0n/1n and are wrapped to a JS boolean).

const SIGNATURES = {
  // integer binary  (i64, i64) -> i64
  add:      { args: ["i64", "i64"], ret: "i64" },
  subtract: { args: ["i64", "i64"], ret: "i64" },
  multiply: { args: ["i64", "i64"], ret: "i64" },
  divide:   { args: ["i64", "i64"], ret: "i64" },
  modulo:   { args: ["i64", "i64"], ret: "i64" },
  percent:  { args: ["i64", "i64"], ret: "i64" },
  average:  { args: ["i64", "i64"], ret: "i64" },
  maximum:  { args: ["i64", "i64"], ret: "i64" },
  minimum:  { args: ["i64", "i64"], ret: "i64" },
  gcd:      { args: ["i64", "i64"], ret: "i64" },
  lcm:      { args: ["i64", "i64"], ret: "i64" },
  power:    { args: ["i64", "i64"], ret: "i64" },
  // integer unary  (i64) -> i64
  negate:    { args: ["i64"], ret: "i64" },
  absolute:  { args: ["i64"], ret: "i64" },
  square:    { args: ["i64"], ret: "i64" },
  increment: { args: ["i64"], ret: "i64" },
  decrement: { args: ["i64"], ret: "i64" },
  double:    { args: ["i64"], ret: "i64" },
  factorial: { args: ["i64"], ret: "i64" },
  fibonacci: { args: ["i64"], ret: "i64" },
  isqrt:     { args: ["i64"], ret: "i64" },
  // integer predicates  (i64) -> bool
  is_even:  { args: ["i64"], ret: "bool" },
  is_prime: { args: ["i64"], ret: "bool" },

  // float binary  (f64, f64) -> f64
  fadd:      { args: ["f64", "f64"], ret: "f64" },
  fsubtract: { args: ["f64", "f64"], ret: "f64" },
  fmultiply: { args: ["f64", "f64"], ret: "f64" },
  fdivide:   { args: ["f64", "f64"], ret: "f64" },
  fmaximum:  { args: ["f64", "f64"], ret: "f64" },
  fminimum:  { args: ["f64", "f64"], ret: "f64" },
  // float power: f64 base, i64 exponent -> f64
  fpower:    { args: ["f64", "i64"], ret: "f64" },
  // float unary  (f64) -> f64
  fnegate:     { args: ["f64"], ret: "f64" },
  fabs:        { args: ["f64"], ret: "f64" },
  fsquare:     { args: ["f64"], ret: "f64" },
  freciprocal: { args: ["f64"], ret: "f64" },
  fsqrt:       { args: ["f64"], ret: "f64" },
  // conversions / predicates
  to_float: { args: ["i64"], ret: "f64" },   // i64 -> f64
  to_int:   { args: ["f64"], ret: "i64" },   // f64 -> i64
  is_nan:   { args: ["f64"], ret: "bool" },  // f64 -> bool
};

let exportsPromise = null;

async function loadExports() {
  const response = await fetch("/calculator.wasm", { cache: "no-store" });
  if (!response.ok) throw new Error(`wasm module HTTP ${response.status}`);
  const { instance } = await WebAssembly.instantiate(await response.arrayBuffer(), {});
  // Reactor modules run their ctors via _initialize before the first call.
  instance.exports._initialize?.();
  return instance.exports;
}

// Marshal a single JS value to the wasm ABI for the given Aury type.
function marshalIn(value, type) {
  if (type === "f64") return Number(value);
  return BigInt(value); // i64 and bool both cross as wasm i64
}

// Marshal the wasm return value back to JS for the given Aury type.
function marshalOut(raw, type) {
  if (type === "bool") return raw === 1n;
  return raw; // i64 -> BigInt, f64 -> Number
}

// Resolve a single exported Aury function, wrapping per-arg marshaling.
function bind(exports, name) {
  const sig = SIGNATURES[name];
  if (!sig) throw new Error(`no signature registered for ${name}`);
  const raw = exports[`aury__${name}`];
  if (typeof raw !== "function") {
    throw new Error(`wasm module is missing export aury__${name}`);
  }
  return (...args) => {
    if (args.length !== sig.args.length) {
      throw new Error(`${name} expects ${sig.args.length} argument(s), got ${args.length}`);
    }
    const marshaled = args.map((a, i) => marshalIn(a, sig.args[i]));
    return marshalOut(raw(...marshaled), sig.ret);
  };
}

// Instantiate once and return a { name -> fn } map plus per-fn metadata.
export async function loadCalculator() {
  if (!exportsPromise) exportsPromise = loadExports();
  const exports = await exportsPromise;

  const fns = {};
  const meta = {};
  for (const [name, sig] of Object.entries(SIGNATURES)) {
    fns[name] = bind(exports, name);
    meta[name] = { arity: sig.args.length, ret: sig.ret, args: sig.args };
  }
  return { fns, meta };
}