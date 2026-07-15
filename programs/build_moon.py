import json
def ref(n): return {"kind":"ref","name":n}
def lit(v): return {"kind":"lit","value":v}
def call(op,*a): return {"kind":"call","op":op,"args":list(a)}
def iff(c,t,e): return {"kind":"if","cond":c,"then":t,"else":e}
def letn(n,t,i,b): return {"kind":"let","name":n,"type":t,"init":i,"body":b}

# Constants (i64). Distances in metres; fixed-point scale S = 1,000,000.
A   = 382_320_000   # OSCULATING semi-major axis, metres (derived from Jul-13 perigee & Jul-25 apogee)
E   = 60_719        # OSCULATING eccentricity 0.06072 * S (this lunation; mean is 0.0549)
S   = 1_000_000     # fixed-point scale (1.0 == S)
TAU = 6_283_185     # 2*pi * S
PI  = 3_141_593     # pi * S
PIH = 1_570_796     # pi/2 * S
TD  = 20_000        # distance tolerance (metres) for the range property
TE  = 500           # exact tolerance (metres) for perigee/apogee points (fixed-point rounding)
TC  = 2_000         # cosine tolerance (fp units)
SC  = 62_832        # theta scaler so property RNG ([-100,100]) covers the circle

fns = []

# perigee = a(1-e), apogee = a(1+e), mean = a  (closed forms, metres)
fns.append({"kind":"fn","name":"perigee","params":[],"ret":"i64",
  "body": call("i64.div", call("i64.mul", A, call("i64.sub", S, E)), S)})
fns.append({"kind":"fn","name":"apogee","params":[],"ret":"i64",
  "body": call("i64.div", call("i64.mul", A, call("i64.add", S, E)), S)})
fns.append({"kind":"fn","name":"mean","params":[],"ret":"i64","body": A})
fns.append({"kind":"fn","name":"perigee-km","params":[],"ret":"i64",
  "body": call("i64.div", call("perigee"), 1000)})
fns.append({"kind":"fn","name":"apogee-km","params":[],"ret":"i64",
  "body": call("i64.div", call("apogee"), 1000)})
fns.append({"kind":"fn","name":"mean-km","params":[],"ret":"i64",
  "body": call("i64.div", call("mean"), 1000)})

# Taylor cosine for x in [0, pi/2], fixed point. Sum 12 terms by recursion.
# term_k = -term_{k-1} * x^2 / (S * (2k) * (2k-1))
ts = call("taylor-step",
          call("i64.div", call("i64.mul", ref("x"), ref("x")), S),
          S, S, 1)
fns.append({"kind":"fn","name":"cos-taylor","params":[{"name":"x","type":"i64"}],"ret":"i64","body": ts})

tk_gt = call("i64.gt", ref("k"), 12)
tk_twok = call("i64.mul", ref("k"), 2)
tk_f = call("i64.mul", tk_twok, call("i64.sub", tk_twok, 1))
tk_denom = call("i64.mul", S, tk_f)
tk_next = call("i64.div", call("i64.mul", call("i64.neg", ref("term")), ref("x2")), tk_denom)
tk_res = call("i64.add", ref("res"), tk_next)
tk_rec = call("taylor-step", ref("x2"), tk_next, tk_res, call("i64.add", ref("k"), 1))
fns.append({"kind":"fn","name":"taylor-step",
  "params":[{"name":"x2","type":"i64"},{"name":"term","type":"i64"},{"name":"res","type":"i64"},{"name":"k","type":"i64"}],
  "ret":"i64","body": iff(tk_gt, ref("res"), tk_rec)})

# Range-reduce x into [0, 2*pi).
rt_ge = call("i64.ge", ref("x"), TAU)
rt_lt = call("i64.lt", ref("x"), 0)
fns.append({"kind":"fn","name":"reduce-tau","params":[{"name":"x","type":"i64"}],"ret":"i64",
  "body": iff(rt_ge, call("reduce-tau", call("i64.sub", ref("x"), TAU)),
              iff(rt_lt, call("reduce-tau", call("i64.add", ref("x"), TAU)), ref("x")))})

# cos-fixed: reduce to [0,pi/2] with sign flips, then Taylor.
cf_x0 = call("reduce-tau", ref("x"))
cf_x1 = iff(call("i64.gt", cf_x0, PI), call("i64.sub", cf_x0, PI), cf_x0)
cf_s1 = iff(call("i64.gt", cf_x0, PI), call("i64.neg", 1), 1)
cf_x2 = iff(call("i64.gt", cf_x1, PIH), call("i64.sub", PI, cf_x1), cf_x1)
cf_s2 = iff(call("i64.gt", cf_x1, PIH), call("i64.neg", 1), 1)
cf_t  = call("cos-taylor", cf_x2)
cf_out = call("i64.mul", call("i64.mul", cf_t, cf_s1), cf_s2)
fns.append({"kind":"fn","name":"cos-fixed","params":[{"name":"x","type":"i64"}],"ret":"i64","body": cf_out})

# distance-at(theta) = a(1-e^2)/(1 + e*cos(theta)), metres.
da_e2 = call("i64.div", call("i64.mul", E, E), S)
da_num = call("i64.div", call("i64.mul", A, call("i64.sub", S, da_e2)), S)
da_c = call("cos-fixed", ref("theta"))
da_den = call("i64.add", S, call("i64.div", call("i64.mul", E, da_c), S))
da_out = call("i64.div", call("i64.mul", da_num, S), da_den)
fns.append({"kind":"fn","name":"distance-at","params":[{"name":"theta","type":"i64"}],"ret":"i64","body": da_out})
fns.append({"kind":"fn","name":"distance-km","params":[{"name":"theta","type":"i64"}],"ret":"i64",
  "body": call("i64.div", call("distance-at", ref("theta")), 1000)})

# ---- specs ----
props = []
def P(name, body): props.append({"name":name,"forall":[{"name":"x","type":"i64"}],"body":body})

# The computed orbit distance stays within [perigee, apogee] (± fixed-point tol).
P("distance-in-range",
  letn("t","i64", call("i64.mul", ref("x"), SC),
    letn("d","i64", call("distance-at", ref("t")),
      letn("lo","i64", call("i64.sub", call("perigee"), TD),
        letn("hi","i64", call("i64.add", call("apogee"), TD),
          call("bool.and", call("i64.le", ref("lo"), ref("d")), call("i64.le", ref("d"), ref("hi"))))))))

# cos(0) and cos(pi) reduce to cos(0)/-cos(0) exactly, so the endpoints are exact.
P("distance-at-zero-is-perigee",
  iff(call("i64.ge", ref("x"), 0),
      call("i64.le", call("i64.abs", call("i64.sub", call("distance-at", 0), call("perigee"))), TE),
      lit(True)))
P("distance-at-pi-is-apogee",
  iff(call("i64.ge", ref("x"), 0),
      call("i64.le", call("i64.abs", call("i64.sub", call("distance-at", PI), call("apogee"))), TE),
      lit(True)))

# cos in [-1,1]; cos is even.
P("cos-bounded",
  call("i64.le", call("i64.abs", call("cos-fixed", call("i64.mul", ref("x"), SC))),
       call("i64.add", S, TC)))
P("cos-even",
  letn("t","i64", call("i64.mul", ref("x"), 1000),
    call("i64.le",
      call("i64.abs", call("i64.sub", call("cos-fixed", ref("t")), call("cos-fixed", call("i64.neg", ref("t"))))),
      TC)))

# perigee < apogee (constant sanity).
P("perigee-lt-apogee", call("i64.lt", call("perigee"), call("apogee")))

spec = {"kind":"spec","properties":props}
mod = {"kind":"module","name":"moon","items":[spec]+fns}
json.dump(mod, open("moon.json","w"), indent=2)
# sanity
json.load(open("moon.json"))
d = sum(1 for c in open("moon.json").read() if c=='{') - sum(1 for c in open("moon.json").read() if c=='}')
print("moon.json written; brace balance:", d)
