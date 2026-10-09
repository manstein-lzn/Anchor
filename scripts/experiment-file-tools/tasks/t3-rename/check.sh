set -eu
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
