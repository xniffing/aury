#!/usr/bin/env python3
"""Generate the typed-object JSON authoring form for the Aury moon model."""

import json
from pathlib import Path


def lit(value):
    return {"kind": "lit", "value": value}


def ref(name):
    return {"kind": "ref", "name": name}


def call(op, *args):
    return {"kind": "call", "op": op, "args": list(args)}


def let(name, ty, init, body):
    return {"kind": "let", "name": name, "type": ty, "init": init, "body": body}


def iff(cond, then, otherwise):
    return {"kind": "if", "cond": cond, "then": then, "else": otherwise}


def get(target, field):
    return {"kind": "get", "target": target, "field": field}


def new_struct(name, **fields):
    return {
        "kind": "new-struct",
        "name": name,
        "fields": [{"name": key, "value": value} for key, value in fields.items()],
    }


def fn(name, params, ret, body):
    return {
        "kind": "fn",
        "name": name,
        "params": [{"name": pname, "type": ptype} for pname, ptype in params],
        "ret": ret,
        "body": body,
    }


def balanced(op, expressions):
    if not expressions:
        return lit(0)
    if len(expressions) == 1:
        return expressions[0]
    middle = len(expressions) // 2
    return call(op, balanced(op, expressions[:middle]), balanced(op, expressions[middle:]))


def linear_angle(coefficients):
    names = ("d", "m", "mp", "f")
    parts = []
    for coefficient, name in zip(coefficients, names):
        if coefficient == 0:
            continue
        value = ref(name)
        if coefficient != 1:
            value = call("i64.mul", lit(coefficient), value)
        parts.append(value)
    return balanced("i64.add", parts)


# Truncated Meeus Table 47.A distance series. Coefficients are meters and
# arguments are integer combinations of D, M, M', and F.
DISTANCE_TERMS = [
    (-20_905_355, (0, 0, 1, 0)),
    (-3_699_111, (2, 0, -1, 0)),
    (-2_955_968, (2, 0, 0, 0)),
    (-569_925, (0, 0, 2, 0)),
    (48_888, (0, 1, 0, 0)),
    (-3_149, (0, 0, 0, 2)),
    (246_158, (2, 0, -2, 0)),
    (-152_138, (2, -1, -1, 0)),
    (-170_733, (2, 0, 1, 0)),
    (-204_586, (2, -1, 0, 0)),
    (-129_620, (0, 1, -1, 0)),
    (108_743, (1, 0, 0, 0)),
    (104_755, (0, 1, 1, 0)),
    (10_321, (2, 0, 0, -2)),
    (79_661, (0, 0, 1, -2)),
    (-34_782, (4, 0, -1, 0)),
    (-23_210, (0, 0, 3, 0)),
    (-21_636, (4, 0, -2, 0)),
    (24_208, (2, 1, -1, 0)),
    (30_824, (2, 1, 0, 0)),
    (-8_379, (1, 0, -1, 0)),
    (-16_675, (1, 1, 0, 0)),
    (-12_831, (2, -1, 1, 0)),
    (-10_445, (2, 0, 2, 0)),
    (-11_650, (4, 0, 0, 0)),
    (14_403, (2, 0, -3, 0)),
    (-7_003, (0, 1, -2, 0)),
    (10_056, (2, -1, -2, 0)),
    (6_322, (1, 0, 1, 0)),
    (-9_884, (2, -2, 0, 0)),
    (5_751, (0, 1, 2, 0)),
    (-4_950, (2, -2, -1, 0)),
    (4_130, (2, 0, 1, -2)),
    (-3_958, (3, 0, -1, 0)),
    (3_258, (4, -1, -2, 0)),
    (2_616, (0, 2, -1, 0)),
    (-1_897, (2, 2, -1, 0)),
    (-2_117, (2, 0, 3, 0)),
    (2_354, (2, 0, -1, -2)),
    (-1_423, (0, 0, 4, 0)),
    (-1_117, (4, -1, 0, 0)),
    (-1_571, (1, 0, -2, 0)),
    (-1_739, (2, 1, -2, 0)),
    (-4_421, (0, 0, 2, -2)),
    (1_165, (0, 2, 1, 0)),
    (8_752, (2, 0, 0, 2)),
]

qmul_body = call(
    "i64.div",
    call("i64.mul", ref("a"), ref("b")),
    lit(1_000_000),
)

normalize_body = let(
    "r",
    "i64",
    call("i64.mod", ref("angle"), lit(360_000)),
    iff(
        call("i64.lt", ref("r"), lit(0)),
        call("i64.add", ref("r"), lit(360_000)),
        ref("r"),
    ),
)

cos_polynomial = call(
    "i64.add",
    call(
        "i64.sub",
        call(
            "i64.add",
            call("i64.sub", lit(1_000_000), call("i64.div", ref("x2"), lit(2))),
            call("i64.div", ref("x4"), lit(24)),
        ),
        call("i64.div", ref("x6"), lit(720)),
    ),
    call("i64.div", ref("x8"), lit(40_320)),
)

cos_body = let(
    "a",
    "i64",
    call("normalize-angle", ref("angle")),
    let(
        "half",
        "i64",
        iff(
            call("i64.gt", ref("a"), lit(180_000)),
            call("i64.sub", lit(360_000), ref("a")),
            ref("a"),
        ),
        let(
            "xdeg",
            "i64",
            iff(
                call("i64.gt", ref("half"), lit(90_000)),
                call("i64.sub", lit(180_000), ref("half")),
                ref("half"),
            ),
            let(
                "x",
                "i64",
                call("i64.div", call("i64.mul", ref("xdeg"), lit(3_141_593)), lit(180_000)),
                let(
                    "x2",
                    "i64",
                    call("qmul", ref("x"), ref("x")),
                    let(
                        "x4",
                        "i64",
                        call("qmul", ref("x2"), ref("x2")),
                        let(
                            "x6",
                            "i64",
                            call("qmul", ref("x4"), ref("x2")),
                            let(
                                "x8",
                                "i64",
                                call("qmul", ref("x4"), ref("x4")),
                                let(
                                    "magnitude",
                                    "i64",
                                    cos_polynomial,
                                    iff(
                                        call("i64.gt", ref("half"), lit(90_000)),
                                        call("i64.neg", ref("magnitude")),
                                        ref("magnitude"),
                                    ),
                                ),
                            ),
                        ),
                    ),
                ),
            ),
        ),
    ),
)

fundamental_angle_body = call(
    "normalize-angle",
    call(
        "i64.add",
        ref("base_mdeg"),
        call(
            "i64.div",
            call(
                "i64.mul",
                call("i64.sub", ref("unix_seconds"), lit(946_728_000)),
                ref("rate_scaled"),
            ),
            lit(8_640_000_000),
        ),
    ),
)

distance_term_body = call(
    "i64.div",
    call("i64.mul", ref("coefficient_m"), call("cos-q6", ref("angle_mdeg"))),
    lit(1_000_000),
)

term_expressions = [
    call("distance-term", lit(coefficient), linear_angle(argument))
    for coefficient, argument in DISTANCE_TERMS
]

distance_series = call("i64.add", lit(385_000_560), balanced("i64.add", term_expressions))

distance_meters_body = let(
    "d",
    "i64",
    call("fundamental-angle", ref("unix_seconds"), lit(297_850), lit(1_219_074_912)),
    let(
        "m",
        "i64",
        call("fundamental-angle", ref("unix_seconds"), lit(357_529), lit(98_560_028)),
        let(
            "mp",
            "i64",
            call("fundamental-angle", ref("unix_seconds"), lit(134_963), lit(1_306_499_295)),
            let(
                "f",
                "i64",
                call("fundamental-angle", ref("unix_seconds"), lit(93_272), lit(1_322_935_024)),
                distance_series,
            ),
        ),
    ),
)

distance_km_body = call("i64.div", call("moon-distance-m", ref("unix_seconds")), lit(1_000))

classify_body = iff(
    call("i64.lt", ref("distance_km"), lit(370_000)),
    lit("near perigee"),
    iff(
        call("i64.gt", ref("distance_km"), lit(400_000)),
        lit("near apogee"),
        lit("mid-range"),
    ),
)

report_body = let(
    "km",
    "i64",
    call("moon-distance-km", ref("unix_seconds")),
    new_struct(
        "MoonDistance",
        unix_seconds=ref("unix_seconds"),
        center_distance_km=ref("km"),
        surface_distance_km=call("i64.sub", ref("km"), lit(8_108)),
        one_way_light_time_ms=call("i64.div", call("i64.mul", ref("km"), lit(1_000)), lit(299_792)),
        range=call("classify-distance", ref("km")),
    ),
)

normalize_bounds = call(
    "bool.and",
    call("i64.ge", call("normalize-angle", ref("a")), lit(0)),
    call("i64.lt", call("normalize-angle", ref("a")), lit(360_000)),
)

cos_even = call(
    "i64.eq",
    call("cos-q6", ref("a")),
    call("cos-q6", call("i64.neg", ref("a"))),
)

distance_plausible = let(
    "km",
    "i64",
    call("moon-distance-km", ref("timestamp")),
    call(
        "bool.and",
        call("i64.gt", ref("km"), lit(350_000)),
        call("i64.lt", ref("km"), lit(410_000)),
    ),
)

module = {
    "kind": "module",
    "name": "moon-distance",
    "items": [
        {
            "kind": "struct",
            "name": "MoonDistance",
            "fields": [
                {"name": "unix_seconds", "type": "i64"},
                {"name": "center_distance_km", "type": "i64"},
                {"name": "surface_distance_km", "type": "i64"},
                {"name": "one_way_light_time_ms", "type": "i64"},
                {"name": "range", "type": "str"},
            ],
        },
        {
            "kind": "spec",
            "properties": [
                {
                    "name": "normalize-angle-bounds",
                    "forall": [{"name": "a", "type": "i64"}],
                    "body": normalize_bounds,
                },
                {
                    "name": "cosine-is-even",
                    "forall": [{"name": "a", "type": "i64"}],
                    "body": cos_even,
                },
                {
                    "name": "lunar-distance-is-physical",
                    "forall": [{"name": "timestamp", "type": "i64"}],
                    "body": distance_plausible,
                },
            ],
        },
        fn("qmul", [("a", "i64"), ("b", "i64")], "i64", qmul_body),
        fn("normalize-angle", [("angle", "i64")], "i64", normalize_body),
        fn("cos-q6", [("angle", "i64")], "i64", cos_body),
        fn(
            "fundamental-angle",
            [("unix_seconds", "i64"), ("base_mdeg", "i64"), ("rate_scaled", "i64")],
            "i64",
            fundamental_angle_body,
        ),
        fn("distance-term", [("coefficient_m", "i64"), ("angle_mdeg", "i64")], "i64", distance_term_body),
        fn("moon-distance-m", [("unix_seconds", "i64")], "i64", distance_meters_body),
        fn("moon-distance-km", [("unix_seconds", "i64")], "i64", distance_km_body),
        fn("classify-distance", [("distance_km", "i64")], "str", classify_body),
        fn("moon-report", [("unix_seconds", "i64")], "(struct MoonDistance)", report_body),
    ],
}

output = Path(__file__).with_name("moon-distance.json")
output.write_text(json.dumps(module, indent=2) + "\n")
print(output)
