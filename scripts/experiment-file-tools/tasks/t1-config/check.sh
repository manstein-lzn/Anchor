set -eu
artifact="$1"
python3 - "$artifact" <<'EOF'
import json, sys, pathlib
root = pathlib.Path(sys.argv[1]) / "files" / "config.json"
data = json.loads(root.read_text())
assert data == {"retries": 7, "timeout_seconds": 30}, data
EOF
