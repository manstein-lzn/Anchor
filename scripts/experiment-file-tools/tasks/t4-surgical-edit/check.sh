set -eu
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
