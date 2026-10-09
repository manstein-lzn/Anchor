set -eu
artifact="$1"
files="$artifact/files"
for name in app.py chart.yaml package.json build.sh NOTES.md; do
  test -f "$files/$name" || { echo "$name missing"; exit 1; }
  ! grep -q "1.2.3" "$files/$name" || { echo "$name still has 1.2.3"; exit 1; }
  grep -q "2.0.0" "$files/$name" || { echo "$name has no 2.0.0"; exit 1; }
done
grep -q "bumped to 2.0.0" "$files/NOTES.md" || { echo "NOTES.md has no changelog line"; exit 1; }
