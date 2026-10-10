set -eu
artifact="$1"
python3 - "$artifact" <<'EOF'
import pathlib, sys
result = pathlib.Path(sys.argv[1]) / "files" / "result.txt"
assert result.is_file(), "result.txt missing"
got = result.read_text().strip()
assert got == "line_237=237", f"result.txt is {got!r}"
EOF
