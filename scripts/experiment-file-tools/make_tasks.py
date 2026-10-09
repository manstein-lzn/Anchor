#!/usr/bin/env python3
"""Generate the frozen task set for the tool-surface experiment."""
import json, pathlib

ROOT = pathlib.Path(__file__).resolve().parent / "tasks"
BUDGET = 240

def graph(objective, seed, instructions):
    return {
        "objective": objective,
        "entry": "seed",
        "agents": {
            "worker": {
                "model": "models.worker",
                "instructions": instructions,
                "wall_time_limit_seconds": BUDGET,
            }
        },
        "ops": {"seed": {"run": seed}},
        "nodes": [{"id": "seed", "op": "seed"}, {"id": "worker", "agent": "worker"}],
        "edges": [{"from": "seed", "to": "worker"}],
    }

FINISH = (
    " Read files from /in/seed/ (read-only); your own /workspace is writable. "
    "When the task is done, call final_result alone with a short summary and route `done`."
)

TASKS = {
    "t1-config": (
        "adjust a JSON configuration",
        """sh -c "printf '{\\042retries\\042: 3, \\042timeout_seconds\\042: 10}\\n' > config.json" """.strip(),
        "Copy /in/seed/config.json into your workspace, set `retries` to 7 and `timeout_seconds` to 30, "
        "keep it valid JSON with exactly those two keys, and leave no other files behind." + FINISH,
        """set -eu
artifact="$1"
python3 - "$artifact" <<'EOF'
import json, sys, pathlib
root = pathlib.Path(sys.argv[1]) / "files" / "config.json"
data = json.loads(root.read_text())
assert data == {"retries": 7, "timeout_seconds": 30}, data
EOF
""",
    ),
    "t2-script-bug": (
        "fix a shell script that prints the wrong total",
        'sh -c "printf \'%s\\n\' \'i=1\' \'total=0\' \'while [ $i -le 4 ]; do total=$((total + i - 1)); i=$((i + 1)); done\' \'echo $total\' > sum.sh"',
        "`sum.sh` should print the sum of 1..4 but prints 9. Copy /in/seed/sum.sh into your workspace, fix it so "
        "`sh sum.sh` prints exactly 10, and keep it a shell script." + FINISH,
        """set -eu
artifact="$1"
directory=$(mktemp -d)
cp "$artifact/files/sum.sh" "$directory/sum.sh"
output=$(cd "$directory" && sh sum.sh)
test "$output" = "10" || { echo "sum.sh printed $output"; exit 1; }
""",
    ),
    "t3-rename": (
        "rename a function across two files",
        """sh -c "printf '%s\\n' 'def fetch():' '    return 41' > helper.py; printf '%s\\n' 'from helper import fetch' 'print(fetch() + 1)' > app.py" """.strip(),
        "`/in/seed/helper.py` defines `fetch` and `/in/seed/app.py` imports it and prints `fetch() + 1`. "
        "Copy both into your workspace, rename `fetch` to `load` in both files so `python3 app.py` still prints 42, "
        "and leave both files in place." + FINISH,
        """set -eu
artifact="$1"
test -f "$artifact/files/helper.py" || { echo "helper.py missing"; exit 1; }
test -f "$artifact/files/app.py" || { echo "app.py missing"; exit 1; }
grep -q "def load" "$artifact/files/helper.py" || { echo "helper.py has no load"; exit 1; }
! grep -q "def fetch" "$artifact/files/helper.py" || { echo "fetch still defined"; exit 1; }
grep -q "load" "$artifact/files/app.py" || { echo "app.py does not use load"; exit 1; }
! grep -q "fetch" "$artifact/files/app.py" || { echo "app.py still imports fetch"; exit 1; }
directory=$(mktemp -d)
cp "$artifact/files/helper.py" "$artifact/files/app.py" "$directory/"
output=$(cd "$directory" && python3 app.py)
test "$output" = "42" || { echo "app.py printed $output"; exit 1; }
""",
    ),

    "t4-surgical-edit": (
        "change one value in a large file without touching the rest",
        """sh -c "i=1; while [ $i -le 400 ]; do printf 'key_%s=%s\\n' \"$i\" \"$i\"; i=$((i + 1)); done > data.txt" """.strip(),
        "`/in/seed/data.txt` has 400 lines of the form `key_<n>=<n>`. Copy it into your workspace and change the "
        "value of `key_237` from 237 to 9990, changing **nothing else** — every other line must stay byte-identical." + FINISH,
        """set -eu
artifact="$1"
python3 - "$artifact" <<'EOF'
import pathlib, sys
before = {f"key_{i}": str(i) for i in range(1, 401)}
lines = (pathlib.Path(sys.argv[1]) / "files" / "data.txt").read_text().splitlines()
assert len(lines) == 400, f"expected 400 lines, got {len(lines)}"
changed = []
for line in lines:
    key, _, value = line.partition("=")
    if before.get(key) != value:
        changed.append((key, value))
assert changed == [("key_237", "9990")], f"changed {changed}"
EOF
""",
    ),
    "t5-version-bump": (
        "bump one version across five files with different syntaxes",
        """sh -c "printf '%s\\n' 'VERSION = \0421.2.3\042' > app.py; printf '%s\\n' 'version: 1.2.3' > chart.yaml; printf '%s\\n' '{\042version\042: \0421.2.3\042}' > package.json; printf '%s\\n' 'VERSION=1.2.3' > build.sh; printf '%s\\n' 'release 1.2.3' > NOTES.md" """.strip(),
        "Five files under /in/seed each mention version `1.2.3` in a different syntax (`app.py`, `chart.yaml`, "
        "`package.json`, `build.sh`, `NOTES.md`). Copy them into your workspace and bump every mention to `2.0.0`, "
        "leaving no mention of `1.2.3` anywhere, then add a line `bumped to 2.0.0` to `NOTES.md`." + FINISH,
        """set -eu
artifact="$1"
files="$artifact/files"
for name in app.py chart.yaml package.json build.sh NOTES.md; do
  test -f "$files/$name" || { echo "$name missing"; exit 1; }
  ! grep -q "1.2.3" "$files/$name" || { echo "$name still has 1.2.3"; exit 1; }
  grep -q "2.0.0" "$files/$name" || { echo "$name has no 2.0.0"; exit 1; }
done
grep -q "bumped to 2.0.0" "$files/NOTES.md" || { echo "NOTES.md has no changelog line"; exit 1; }
""",
    ),
}

for name, (objective, seed, instructions, check) in TASKS.items():
    directory = ROOT / name
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "graph.json").write_text(json.dumps(graph(objective, seed, instructions), indent=2) + "\n")
    (directory / "check.sh").write_text(check)
    (directory / "check.sh").chmod(0o755)
    print("wrote", directory)
