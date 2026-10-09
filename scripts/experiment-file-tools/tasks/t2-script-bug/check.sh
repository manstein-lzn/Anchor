set -eu
artifact="$1"
directory=$(mktemp -d)
cp "$artifact/files/sum.sh" "$directory/sum.sh"
output=$(cd "$directory" && sh sum.sh)
test "$output" = "10" || { echo "sum.sh printed $output"; exit 1; }
