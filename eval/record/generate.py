#!/usr/bin/env python3
"""Track D — record model generations for the generation-reliability baseline.

Run this OFFLINE (it calls the Anthropic API; the scoring harness stays
deterministic and hermetic). For each task in tasks.json it asks a named model,
on a named date, to generate `k` programs in Aury AND in Python from matched
prompts, and writes the raw generations (failures included) plus a provenance
manifest under eval/generated/<model>-<date>/. Then score them with:

    aury eval-generated eval/generated/<model>-<date>

Auth: uses the Anthropic SDK's normal credential resolution (ANTHROPIC_API_KEY,
or an `ant auth login` profile). Usage:

    python3 eval/record/generate.py --model claude-opus-4-8 --k 5

Note: current models reject `temperature`, so the k samples are independent
sampled calls (the API samples by default) rather than a fixed temperature.
"""
import argparse
import datetime
import hashlib
import json
import os
import sys

try:
    from anthropic import Anthropic
except ImportError:
    sys.exit("the `anthropic` package is required: pip install anthropic")

HERE = os.path.dirname(os.path.abspath(__file__))


def load_prompt(name: str) -> str:
    with open(os.path.join(HERE, "prompts", f"{name}.md"), encoding="utf-8") as f:
        return f.read()


def render(template: str, task: dict, lang: str) -> str:
    sig = task["signature"]
    if lang == "aury":
        params = ", ".join(f"`{p}` ({t})" for p, t in sig["params"]) or "(none)"
    else:
        params = ", ".join(p for p, _ in sig["params"]) or "(none)"
    return (
        template.replace("{{INTENT}}", task["intent"])
        .replace("{{FN}}", sig["fn"])
        .replace("{{PARAMS}}", params)
        .replace("{{RET}}", sig["ret"])
    )


def strip_fences(text: str) -> str:
    """Remove a leading/trailing markdown code fence if the model added one."""
    t = text.strip()
    if t.startswith("```"):
        # Drop the first fence line and a trailing fence.
        lines = t.splitlines()
        if lines and lines[0].startswith("```"):
            lines = lines[1:]
        if lines and lines[-1].strip().startswith("```"):
            lines = lines[:-1]
        t = "\n".join(lines)
    return t.strip() + "\n"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--model", default="claude-opus-4-8", help="model id (recorded in the manifest)")
    ap.add_argument("--k", type=int, default=5, help="generations per task per language")
    ap.add_argument("--tasks", default=os.path.join(HERE, "tasks.json"))
    ap.add_argument("--out", default=os.path.join(HERE, "..", "generated"))
    ap.add_argument("--max-tokens", type=int, default=2048)
    args = ap.parse_args()

    with open(args.tasks, encoding="utf-8") as f:
        corpus = json.load(f)
    tasks = corpus["tasks"]

    prompts = {"aury": load_prompt("aury"), "python": load_prompt("python")}
    prompt_hashes = {
        lang: hashlib.sha256(text.encode()).hexdigest()[:16] for lang, text in prompts.items()
    }

    date = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")
    run_dir = os.path.join(args.out, f"{args.model}-{date}")
    os.makedirs(run_dir, exist_ok=True)

    client = Anthropic()
    exts = {"aury": "aury", "python": "py"}

    for task in tasks:
        name = task["name"]
        for lang in ("aury", "python"):
            prompt = render(prompts[lang], task, lang)
            lang_dir = os.path.join(run_dir, name, lang)
            os.makedirs(lang_dir, exist_ok=True)
            for i in range(args.k):
                resp = client.messages.create(
                    model=args.model,
                    max_tokens=args.max_tokens,
                    messages=[{"role": "user", "content": prompt}],
                )
                text = "".join(b.text for b in resp.content if b.type == "text")
                out = strip_fences(text)
                path = os.path.join(lang_dir, f"{i}.{exts[lang]}")
                with open(path, "w", encoding="utf-8") as f:
                    f.write(out)
                print(f"wrote {path} ({len(out)} bytes)")

    manifest = {
        "model": args.model,
        "date": date,
        "k": args.k,
        "prompt_hashes": prompt_hashes,
        "tasks": [t["name"] for t in tasks],
        "note": "Generation is non-hermetic and model/date-stamped. Scoring (aury eval-generated) is deterministic.",
    }
    with open(os.path.join(run_dir, "manifest.json"), "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
    print(f"\nwrote manifest → {os.path.join(run_dir, 'manifest.json')}")
    print(f"score it: aury eval-generated {run_dir}")


if __name__ == "__main__":
    main()
