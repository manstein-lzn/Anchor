"""Run the unchanged academic Graph against a controlled provider and real sandbox.

This checks execution contracts, not research quality or live-provider acceptance.
The fixture forces research and writing feedback, uses an operator local grant,
and verifies that reviews are tied to the actual immutable draft Git HEAD.
"""

from __future__ import annotations

import argparse
from collections import Counter
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import threading


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "examples/graphs/deep-academic-research.json"
COMMANDS = {
    "建立可修正的研究问题": "test ! -e /local-inputs/reference/brief.txt; printf 'fixture framing\\n' > framing.md",
    "持续负责同一项学术研究": """test -r /in/frame/framing.md
test "$(cat /local-inputs/reference/brief.txt)" = 'authorized fixture'
if test -f research.md; then
    grep -q '^first$' research.md
    test -f /in/feedback/feedback.md
    printf 'second\n' >> research.md
else
    printf 'first\n' > research.md
fi
printf 'Controlled fixture, no scholarly claim.\n' > sources.md""",
    "独立检验研究解释": """if grep -q '^second$' /in/investigate/research.md; then
    test -f critique.md
    printf 'DECISION: synthesize\n' > critique.md
else
    printf 'DECISION: investigate\n' > critique.md
fi""",
    "将研究消化为面向读者的学术综述论文": """if test -f answer.md; then
    grep -q '^initial draft$' answer.md
    test -f /in/review-gate/review-gate.md
    printf '\nrevised draft\n' >> answer.md
else
    cat > answer.md <<'PAPER'
# Controlled fixture
initial draft
## Abstract
## Introduction
## Survey Methodology
## Comparative Analysis
## Open Problems
## Threats to Validity
## Conclusion
## References
PAPER
fi""",
    "独立评审 /in/synthesize/answer.md": """head=$(git --git-dir=/in/synthesize/.git rev-parse HEAD)
if test -f reviewed-commit; then
    test "$(cat reviewed-commit)" != "$head"
    test -f review.md
fi
printf '%s\n' "$head" > reviewed-commit
if grep -q '^revised draft$' /in/synthesize/answer.md; then
    printf 'DECISION: pass\n' > review.md
else
    printf 'DECISION: writing\n' > review.md
fi""",
}


class FixtureProvider(BaseHTTPRequestHandler):
    calls: list[dict] = []
    failures: list[str] = []

    def log_message(self, *_args: object) -> None:
        pass

    def do_POST(self) -> None:
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        try:
            assert request["model"] == "fixture-academic", "Graph alias did not select its model"
            messages = request["messages"]
            serialized = json.dumps(messages, ensure_ascii=False)
            selected = [key for key in COMMANDS if key in serialized]
            assert len(selected) == 1, f"cannot identify fixture node: {selected}"
            tool_results = [message for message in messages if message.get("role") == "tool"]
            if tool_results:
                # A command failure must not be disguised as successful completion.
                observed = json.dumps(tool_results, ensure_ascii=False)
                assert '"exit_code": 0' in observed or '\\"exit_code\\":0' in observed or '\\"exit_code\\": 0' in observed, observed
                name, arguments = "final_result", {"summary": "Controlled fixture completed", "route": None}
            else:
                name = "anchor_run"
                arguments = {"command": ["sh", "-c", "set -eu\n" + COMMANDS[selected[0]]]}
            self.calls.append({"model": request["model"], "node": selected[0], "tool": name})
            response = {
                "id": f"chatcmpl-{len(self.calls)}", "object": "chat.completion", "created": 1,
                "model": request["model"],
                "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
                    "role": "assistant", "content": None,
                    "tool_calls": [{"id": f"call-{len(self.calls)}", "type": "function", "function": {
                        "name": name, "arguments": json.dumps(arguments),
                    }}],
                }}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            }
            payload = json.dumps(response).encode()
            self.send_response(200)
        except (AssertionError, KeyError, ValueError) as error:
            self.failures.append(str(error))
            payload = json.dumps({"error": {"message": str(error), "type": "fixture_error"}}).encode()
            self.send_response(400)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def bundle_graph(bundle: Path) -> None:
    plugin = bundle / "plugins/academic-research"
    shutil.copytree(ROOT / "plugins/academic-research", plugin)
    shutil.copyfile(SOURCE, bundle / "graph.json")
    resources = sorted(path.relative_to(plugin).as_posix() for path in plugin.rglob("*") if path.is_file())
    digest = hashlib.sha256()
    for name in resources:
        digest.update(name.encode())
        digest.update(hashlib.sha256((plugin / name).read_bytes()).digest())
    (bundle / "manifest.json").write_text(json.dumps({
        "format": 1, "graph": "graph.json", "plugins": [{
            "id": "academic-research", "digest": digest.hexdigest(),
            "resources": resources, "mcp_servers": [],
        }],
    }))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/debug/anchor-runner-host")
    args = parser.parse_args()
    (ROOT / ".local").mkdir(exist_ok=True)
    proof = Path(tempfile.mkdtemp(prefix="rust-product-parity-", dir=ROOT / ".local"))
    print(f"Evidence directory: {proof}", flush=True)
    bundle = proof / "bundle"
    bundle_graph(bundle)
    reference = proof / "reference"
    reference.mkdir()
    (reference / "brief.txt").write_text("authorized fixture\n")
    grants = proof / "operator-workspaces" / "deep-academic-research"
    grants.mkdir(parents=True)
    (grants / "local-inputs.json").write_text(json.dumps({"investigate": {"reference": str(reference)}}))
    state = proof / "state"
    server = ThreadingHTTPServer(("127.0.0.1", 0), FixtureProvider)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    env = {
        "PATH": "/usr/bin:/bin",
        "ANCHOR_MODEL_API_KEY": "fixture-not-a-secret",
        "ANCHOR_MODEL_URL": f"http://127.0.0.1:{server.server_port}/v1",
        "ANCHOR_MODEL_NAME": "fixture-default",
        "ANCHOR_MODEL_ALIASES": json.dumps({"models.academic": "fixture-academic"}),
        "ANCHOR_MODEL_WIRE_API": "chat",
        "ANCHOR_RUNNER_BUNDLE_ROOT": str(bundle),
        "ANCHOR_RUNNER_GRAPH_NAME": "deep-academic-research",
        "ANCHOR_RUNNER_LOCAL_INPUTS_ROOT": str(grants.parent),
        "ANCHOR_RUNNER_STATE_ROOT": str(state),
        "ANCHOR_RUNNER_WORKSPACE_ROOT": str(proof / "workspaces"),
        "ANCHOR_RUNNER_ALLOWED_COMMANDS": "sh,cat,git,sed,cp,grep,printf",
    }
    request = json.dumps({"op": "start_bundle", "version": 1, "request_id": "parity",
                          "run_id": "parity", "input": {}}).encode()
    evidence = {"status": "failed", "provider": "controlled local fixture", "graph_unchanged": True,
                "graph_sha256": hashlib.sha256(SOURCE.read_bytes()).hexdigest()}
    try:
        result = subprocess.run([str(args.binary.resolve())], input=struct.pack(">I", len(request)) + request,
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=180, check=False)
        (proof / "host.stderr").write_bytes(result.stderr)
        result.check_returncode()
        response = json.loads(result.stdout[4:])
        (proof / "response.json").write_text(json.dumps(response, indent=2))
        assert response.get("status") == "completed", response
        assert not FixtureProvider.failures, FixtureProvider.failures
        record = json.loads((state / "runs/parity.json").read_text())
        expected = {"frame": 1, "investigate": 2, "challenge": 2, "feedback": 2,
                    "synthesize": 2, "review": 2, "review-gate": 2, "report": 1}
        assert {node: len(results) for node, results in record["results"].items()} == expected

        def artifact(node: str, index: int, name: str) -> str:
            commit = record["results"][node][index]["commit"]["id"]
            return (state / "artifacts" / commit / "files" / name).read_text()

        assert artifact("investigate", 0, "research.md") == "first\n"
        assert artifact("investigate", 1, "research.md") == "first\nsecond\n"
        assert "revised draft" not in artifact("synthesize", 0, "answer.md")
        assert "revised draft" in artifact("report", 0, "paper.md")
        first_head = artifact("review", 0, "reviewed-commit").strip()
        revised_head = artifact("review", 1, "reviewed-commit").strip()
        assert first_head != revised_head
        assert (bundle / "graph.json").read_bytes() == SOURCE.read_bytes()
        evidence.update(status="passed", node_passes=expected,
                        provider_requests=len(FixtureProvider.calls),
                        tool_calls=dict(Counter(call["tool"] for call in FixtureProvider.calls)),
                        draft_heads=[first_head, revised_head],
                        scope="Original Graph execution, two feedback loops, local input isolation, immutable review binding; not research quality")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        evidence["provider_failures"] = FixtureProvider.failures
        (proof / "evidence.json").write_text(json.dumps(evidence, indent=2, ensure_ascii=False))
        print(json.dumps({"evidence": str(proof / "evidence.json"), **evidence}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
