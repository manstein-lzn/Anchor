#!/usr/bin/env python3
"""Paired task-quality experiment for the AgentNode tool surface.

One variable changes between arms: the Host binary (and therefore whether
`anchor_read`/`anchor_edit` exist). Everything else - task, instructions, model,
provider, wall-clock budget, seed - is identical, and the pass/fail decision
comes from an independent artifact check, never from the model's own summary.
"""
import argparse, json, os, pathlib, shutil, struct, subprocess, sys, tempfile, time

ROOT = pathlib.Path(__file__).resolve().parent


def dotenv(path):
    values = {}
    for line in pathlib.Path(path).read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        values[key.strip()] = value.strip().strip("'\"")
    return values


def host_environment(env, state, work, bundle, tasks_root, host_env=None):
    goose = env["ANCHOR_GOOSE_BINARY"]
    built = {
        "PATH": "/usr/bin:/bin",
        "LANG": "C.UTF-8",
        "TZ": "UTC",
        "HOME": str(work),
        "XDG_CONFIG_HOME": str(work / "xdg/config"),
        "XDG_DATA_HOME": str(work / "xdg/data"),
        "XDG_STATE_HOME": str(work / "xdg/state"),
        "XDG_CACHE_HOME": str(work / "xdg/cache"),
        "ANCHOR_RUNNER_STATE_ROOT": str(state),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(work / "workspaces"),
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(bundle),
        "ANCHOR_RUNNER_CATALOG_ROOT": str(tasks_root),
        "ANCHOR_RUNNER_GRAPH_NAME": "experiment",
        "ANCHOR_RUNNER_SCHEDULES_PATH": str(state / "schedules.json"),
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,grep,sed,printf,ls,test,true,python3",
        "ANCHOR_RUNNER_AGENT_RUNTIME": "goose",
        "ANCHOR_GOOSE_ALLOW_SHARED_NETWORK": "1",
        "ANCHOR_GOOSE_BINARY": goose,
        "ANCHOR_GOOSE_BINARY_SHA256": env["ANCHOR_GOOSE_BINARY_SHA256"],
        "ANCHOR_MODEL_URL": env["ANCHOR_MODEL_URL"],
        "ANCHOR_MODEL_API_KEY": env["ANCHOR_MODEL_API_KEY"],
        "ANCHOR_MODEL_NAME": env.get("ANCHOR_MODEL_NAME", "deepseek-flash"),
        "ANCHOR_MODEL_WIRE_API": env.get("ANCHOR_MODEL_WIRE_API", "chat"),
        # The experiment pins one alias so both arms resolve the same model.
        "ANCHOR_MODEL_ALIASES": json.dumps(
            {"models.worker": env.get("ANCHOR_MODEL_NAME", "deepseek-flash")}
        ),
        "RUST_LOG": "warn",
    }
    built.update({k: str(v) for k, v in (host_env or {}).items()})
    return built


def run_once(host, task, env, arm, out_root, repetition, timeout, host_env=None):
    work = out_root / arm / task.name / f"rep{repetition}"
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)
    state = work / "state"
    work_root = work / "work"
    bundle = work / "bundle"
    state.mkdir(exist_ok=True)
    (work_root / "xdg").mkdir(parents=True, exist_ok=True)
    bundle.mkdir(exist_ok=True)
    shutil.copy(task.bundle, bundle / "graph.json")
    (bundle / "manifest.json").write_text(
        json.dumps({"format": 1, "graph": "graph.json", "plugins": []})
    )
    request = {
        "op": "start_bundle",
        "version": 1,
        "request_id": f"{task.name}-{arm}-{repetition}",
        "run_id": "experiment",
        "input": {},
    }
    payload = json.dumps(request).encode()
    started = time.time()
    process = subprocess.run(
        [str(host)],
        input=struct.pack(">I", len(payload)) + payload,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=host_environment(env, state, work_root, bundle, task.root, host_env),
        cwd=work,
        timeout=timeout,
    )
    elapsed = time.time() - started
    (work / "stderr.txt").write_bytes(process.stderr)
    response = None
    if len(process.stdout) >= 4:
        length = struct.unpack(">I", process.stdout[:4])[0]
        if len(process.stdout) == length + 4:
            response = json.loads(process.stdout[4:])
    (work / "response.json").write_text(json.dumps(response, indent=2))
    artifact = worker_artifact(state)
    check = subprocess.run(
        ["sh", str(task.check), str(artifact)],
        cwd=bundle,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=60,
    )
    usage = node_usage(state)
    evidence = " ".join(
        path.read_text(errors="replace")
        for path in (state / "goose-acp").glob("*.evidence.json")
    )
    return {
        "task": task.name,
        "used_disclosure": "anchor_tools" in evidence,
        "arm": arm,
        "repetition": repetition,
        "returncode": process.returncode,
        "status": ((response or {}).get("state") or response or {}).get("status"),
        "error": ((response or {}).get("state") or response or {}).get("error"),
        "passed": check.returncode == 0,
        "check_output": check.stdout.decode(errors="replace")[:2000],
        "elapsed_s": round(elapsed, 1),
        "usage": usage,
    }


def worker_artifact(state):
    """Directory of the agent node's frozen workspace snapshot."""
    record = json.loads((state / "runs" / "experiment.json").read_text())
    commit = record["results"]["worker"][0]["commit"]["id"]
    return state / "artifacts" / commit


def node_usage(state):
    totals = {"messages": 0, "input_tokens": 0, "output_tokens": 0, "cost_usd": 0.0}
    for evidence in (state / "goose-acp").glob("*.evidence.json"):
        try:
            data = json.loads(evidence.read_text())
        except Exception:
            continue
        usage = data.get("usage") or {}
        for key in ("messages", "input_tokens", "output_tokens"):
            totals[key] += int(usage.get(key) or 0)
        totals["cost_usd"] += float(usage.get("cost_usd") or 0.0)
    return totals


class Task:
    def __init__(self, root):
        self.root = root
        self.name = root.name
        self.bundle = root / "graph.json"
        self.check = root / "check.sh"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", required=True)
    parser.add_argument("--arm", required=True)
    parser.add_argument("--env-file", default="/home/mansteinl/Anchor/.env")
    parser.add_argument("--tasks", default=str(ROOT / "tasks"))
    parser.add_argument("--out", required=True)
    parser.add_argument("--repetitions", type=int, default=1)
    parser.add_argument("--only")
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--host-env", default="{}", help="JSON object of extra Host env")
    options = parser.parse_args()
    env = dotenv(options.env_file)
    tasks = sorted(
        (Task(path) for path in pathlib.Path(options.tasks).iterdir() if path.is_dir()),
        key=lambda task: task.name,
    )
    if options.only:
        tasks = [task for task in tasks if task.name == options.only]
    out_root = pathlib.Path(options.out)
    results = []
    for task in tasks:
        for repetition in range(1, options.repetitions + 1):
            result = run_once(
                pathlib.Path(options.host),
                task,
                env,
                options.arm,
                out_root,
                repetition,
                options.timeout,
                json.loads(options.host_env),
            )
            results.append(result)
            print(json.dumps(result, ensure_ascii=False), flush=True)
    (out_root / f"{options.arm}.json").write_text(json.dumps(results, indent=2, ensure_ascii=False))
    passed = sum(1 for result in results if result["passed"])
    print(f"{options.arm}: {passed}/{len(results)} passed", flush=True)


if __name__ == "__main__":
    main()
