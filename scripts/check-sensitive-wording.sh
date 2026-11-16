#!/usr/bin/env bash
# Fail when project text carries a two-character classification term that the
# deployment model gateway matches as a sensitive word and rejects the whole
# request for. All terms are assembled here from octal escapes so this script is
# not a carrier itself.
#
# Default scope is the tracked working tree. `--all` additionally walks untracked
# project files while still skipping ignored runtime state (`.local/`), dependency
# and build directories.
#
# A term can be flagged for two different reasons:
#   * an accident: the two characters are produced by two ordinary words that
#     happen to meet (a verb or noun ending in the first character followed by
#     密钥/密码/…); reword so the characters no longer touch.
#   * a genuine use of the classification word; reword to a synonym such as
#     保密的字段 / 凭据字段 / 内部资料.
set -eu

# 「绝」「密」
term_classified=$(printf '\347\273\235\345\257\206')
# 「机」「密」
term_confidential=$(printf '\346\234\272\345\257\206')
# 「秘」「密」
term_secret=$(printf '\347\247\230\345\257\206')

root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$root"

listing=$(mktemp)
trap 'rm -f "$listing"' EXIT

if [ "${1:-}" = "--all" ]; then
    git ls-files -z --cached --others --exclude-standard > "$listing"
else
    git ls-files -z > "$listing"
fi

status=0
while IFS= read -r -d '' file; do
    [ -f "$file" ] || continue
    for term in "$term_classified" "$term_confidential" "$term_secret"; do
        matches=$(grep -n -F --binary-files=without-match -- "$term" "$file" 2>/dev/null || true)
        [ -n "$matches" ] || continue
        printf '%s\n' "$matches" | while IFS= read -r line; do
            printf '%s:%s\n' "$file" "$line"
        done
        status=1
    done
done < "$listing"

if [ "$status" -ne 0 ]; then
    echo "被网关拦截的分类词连写出现在工程文本中；改用同义写法，见 docs/development.md。" >&2
    exit 1
fi

echo "工程文本不含被网关拦截的分类词连写"
