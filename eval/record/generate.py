#!/usr/bin/env python3
"""Track D — record model generations for the generation-reliability baseline.

Run this OFFLINE (it calls a model API; the scoring harness stays deterministic
and hermetic). For each task in tasks.json it asks a named model, on a named
date, to generate `k` programs in Aury AND in Python from matched prompts, and
writes the raw generations (failures included) plus a provenance manifest under
eval/generated/<model>-<date>/. Then score them with:

    aury eval-generated eval/generated/<model>-<date>

Provider: the **Ollama Cloud** chat API (native `/api/chat` endpoint, no extra
Python packages — uses stdlib urllib). Auth: set `OLLAMA_API_KEY` (a bearer
token from ollama.com). Point `--host` at a local Ollama (http://localhost:11434)
to run a local model with no key. Usage:

    export OLLAMA_API_KEY=...          # from https://ollama.com
    python3 eval/record/generate.py --model glm-5.2 --k 5

`--model` must be an exact Ollama model tag (e.g. glm-5.2, glm-4.6, gpt-oss:120b).
Unlike current Anthropic models, Ollama accepts `temperature`, so the k samples
diversify via `--temperature` (default 1.0).
"""
import argparse
import datetime
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request

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
        lines = t.splitlines()
        if lines and lines[0].startswith("```"):
            lines = lines[1:]
        if lines and lines[-1].strip().startswith("```"):
            lines = lines[:-1]
        t = "\n".join(lines)
    return t.strip() + "\n"


def ollama_chat(host: str, api_key: str, model: str, prompt: str,
                temperature: float, num_predict: int, retries: int = 2) -> str:
    """One non-streaming Ollama Cloud /api/chat call → the assistant text."""
    url = host.rstrip("/") + "/api/chat"
    body = json.dumps({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": False,
        "options": {"temperature": temperature, "num_predict": num_predict},
    }).encode("utf-8")
    headers = {"Content-Type": "application/json"}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    last_err = None
    for attempt in range(retries + 1):
        req = urllib.request.Request(url, data=body, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(req, timeout=300) as resp:
                data = json.loads(resp.read().decode("utf-8"))
            return data["message"]["content"]
        except (urllib.error.URLError, KeyError, json.JSONDecodeError) as e:
            last_err = e
            if attempt < retries:
                time.sleep(2 * (attempt + 1))
    raise RuntimeError(f"Ollama request failed after {retries + 1} attempts: {last_err}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--model", default="glm-5.2", help="exact Ollama model tag (recorded in the manifest)")
    ap.add_argument("--host", default="https://ollama.com", help="Ollama host (use http://localhost:11434 for a local model)")
    ap.add_argument("--k", type=int, default=5, help="generations per task per language")
    ap.add_argument("--temperature", type=float, default=1.0, help="sampling temperature for k-sample diversity")
    ap.add_argument("--tasks", default=os.path.join(HERE, "tasks.json"))
    ap.add_argument("--out", default=os.path.join(HERE, "..", "generated"))
    ap.add_argument("--max-tokens", type=int, default=2048, help="num_predict (max output tokens)")
    args = ap.parse_args()

    api_key = os.environ.get("OLLAMA_API_KEY", "")
    if not api_key and "localhost" not in args.host and "127.0.0.1" not in args.host:
        sys.exit("set OLLAMA_API_KEY (from https://ollama.com), or point --host at a local Ollama")

    with open(args.tasks, encoding="utf-8") as f:
        corpus = json.load(f)
    tasks = corpus["tasks"]

    prompts = {"aury": load_prompt("aury"), "python": load_prompt("python")}
    prompt_hashes = {
        lang: hashlib.sha256(text.encode()).hexdigest()[:16] for lang, text in prompts.items()
    }

    date = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")
    # Model tags can contain '/', ':' — sanitize for the run-directory name.
    safe_model = args.model.replace("/", "_").replace(":", "_")
    run_dir = os.path.join(args.out, f"{safe_model}-{date}")
    os.makedirs(run_dir, exist_ok=True)

    exts = {"aury": "aury", "python": "py"}

    for task in tasks:
        name = task["name"]
        for lang in ("aury", "python"):
            prompt = render(prompts[lang], task, lang)
            lang_dir = os.path.join(run_dir, name, lang)
            os.makedirs(lang_dir, exist_ok=True)
            for i in range(args.k):
                try:
                    text = ollama_chat(
                        args.host, api_key, args.model, prompt,
                        args.temperature, args.max_tokens,
                    )
                except RuntimeError as e:
                    print(f"  ! {name}/{lang}/{i}: {e}", file=sys.stderr)
                    continue
                out = strip_fences(text)
                path = os.path.join(lang_dir, f"{i}.{exts[lang]}")
                with open(path, "w", encoding="utf-8") as f:
                    f.write(out)
                print(f"wrote {path} ({len(out)} bytes)")

    manifest = {
        "model": args.model,
        "date": date,
        "k": args.k,
        "provider": "ollama",
        "host": args.host,
        "temperature": args.temperature,
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
