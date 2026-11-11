#!/bin/sh
# Exercise native lifecycle rules in private namespaces without a service manager.
set -eu

directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
for tool in bwrap sh getent mkdir ln cat chmod stat readlink mktemp cp rm; do
    command -v "$tool" >/dev/null || {
        echo "Required lifecycle test tool is missing: $tool" >&2
        exit 1
    }
done

scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
printf 'root:x:0:0:root:/root:/bin/sh\n' > "$scratch/passwd"
printf 'root:x:0:\n' > "$scratch/group"
cp "$scratch/passwd" "$scratch/passwd-anchor"
cp "$scratch/group" "$scratch/group-anchor"
printf 'anchor:x:12345:12345:fixture:/var/lib/anchor:/sbin/nologin\n' >> "$scratch/passwd-anchor"
printf 'anchor:x:12345:\n' >> "$scratch/group-anchor"
printf 'passwd: files\ngroup: files\n' > "$scratch/nsswitch.conf"

shell=$(readlink -f -- "$(command -v sh)")
getent=$(readlink -f -- "$(command -v getent)")
mkdir=$(readlink -f -- "$(command -v mkdir)")
ln=$(readlink -f -- "$(command -v ln)")
cat=$(readlink -f -- "$(command -v cat)")
chmod=$(readlink -f -- "$(command -v chmod)")
stat=$(readlink -f -- "$(command -v stat)")
nsswitch=$(readlink -f -- /etc/nsswitch.conf)
deb=$(CDPATH= cd -- "$directory/../deb" && pwd -P)
passed=0

# Account and ownership helpers report only private fixture state. They never
# invoke the real account tools or change the ownership of any host files.
cat > "$scratch/configure-fixture.sh" <<'FIXTURE'
mkdir -p /run/fixture /var/lib/anchor/platform/release
chmod 0755 /var/lib /var/lib/anchor /var/lib/anchor/platform
chmod 0555 /var/lib/anchor/platform/release
cat > /usr/bin/getent <<'MOCK'
#!/usr/bin/sh
set -eu
case "$1" in
    passwd) test -f /run/fixture/user || exit 2; cat /run/fixture/user ;;
    group) test -f /run/fixture/group || exit 2; cat /run/fixture/group ;;
    *) exit 1 ;;
esac
MOCK
cat > /usr/bin/stat <<'MOCK'
#!/usr/bin/sh
set -eu
case "$4" in
    /var/lib/anchor/platform/data|/var/lib/anchor/platform/data/*)
        mode=$(/usr/bin/fixture-stat -c '%a' -- "$4")
        echo "12345:12345:$mode"
        ;;
    *) echo "0:0:$(/usr/bin/fixture-stat -c '%a' -- "$4")" ;;
esac
MOCK
cat > /usr/sbin/groupadd <<'MOCK'
#!/usr/bin/sh
set -eu
test "$*" = '--system anchor'
test ! -f /run/fixture/group
echo 'anchor:x:12345:' > /run/fixture/group
MOCK
cat > /usr/sbin/useradd <<'MOCK'
#!/usr/bin/sh
set -eu
test "$*" = '--system --gid anchor --home-dir /var/lib/anchor --no-create-home --shell /sbin/nologin anchor'
test ! -f /run/fixture/user
echo 'anchor:x:12345:12345:fixture:/var/lib/anchor:/sbin/nologin' > /run/fixture/user
MOCK
cat > /usr/bin/install <<'MOCK'
#!/usr/bin/sh
set -eu
test "$*" = '-d -o 12345 -g 12345 -m 0700 /var/lib/anchor/platform/data'
test ! -e /var/lib/anchor/platform/data
mkdir -m 0700 /var/lib/anchor/platform/data
MOCK
cat > /usr/sbin/runuser <<'MOCK'
#!/usr/bin/sh
set -eu
test "$1 $2 $3" = '-u anchor --'
shift 3
exec "$@"
MOCK
chmod 0700 /usr/bin/getent /usr/bin/stat /usr/sbin/groupadd \
    /usr/sbin/useradd /usr/bin/install /usr/sbin/runuser
FIXTURE

for pair in preinst:pre postinst:post prerm:preun postrm:postun; do
    maintainer=${pair%%:*} canonical=${pair#*:}
    cat "$deb/$maintainer.sh" "$directory/$canonical.sh" > "$scratch/$maintainer"
done

run_case() {
    name=$1 expected=$2 diagnostic=$3 command=$4
    passwd=${5:-"$scratch/passwd"} group=${6:-"$scratch/group"}
    # Hide account tools, systemctl, any installed Anchor and all mutable system
    # roots. Even an accidental scriptlet branch cannot change the real system.
    if bwrap --unshare-all --die-with-parent --ro-bind / / \
        --tmpfs /var/lib --tmpfs /run --tmpfs /tmp --setenv TMPDIR /tmp \
        --tmpfs /etc/systemd/system \
        --tmpfs /usr/lib/systemd/system --tmpfs /usr/share/applications \
        --tmpfs /usr/bin --tmpfs /usr/sbin \
        --ro-bind "$shell" /usr/bin/sh --ro-bind "$getent" /usr/bin/fixture-getent \
        --ro-bind "$mkdir" /usr/bin/mkdir --ro-bind "$ln" /usr/bin/ln \
        --ro-bind "$cat" /usr/bin/cat --ro-bind "$chmod" /usr/bin/chmod \
        --ro-bind "$stat" /usr/bin/fixture-stat \
        --ro-bind "$passwd" /etc/passwd --ro-bind "$group" /etc/group \
        --ro-bind "$scratch/nsswitch.conf" "$nsswitch" \
        --ro-bind "$scratch" /run/fixtures \
        --proc /proc --dev /dev -- /usr/bin/sh -eu -c '
            cat /usr/bin/fixture-getent > /usr/bin/getent
            chmod 0700 /usr/bin/getent
        '"$command" \
        scriptlet-fixture "$directory/pre.sh" "$directory/post.sh" \
        "$directory/preun.sh" "$directory/postun.sh" /run/fixtures/configure-fixture.sh \
        /run/fixtures/preinst /run/fixtures/postinst /run/fixtures/prerm /run/fixtures/postrm \
        > "$scratch/output" 2>&1
    then
        actual=0
    else
        actual=$?
    fi
    output=$(cat "$scratch/output")
    if [ "$actual" != "$expected" ]; then
        printf 'FAIL %s: status %s, expected %s\n%s\n' "$name" "$actual" "$expected" "$output" >&2
        exit 1
    fi
    case "$output" in
        *"$diagnostic"*) ;;
        *) printf 'FAIL %s: missing diagnostic %s\n%s\n' "$name" "$diagnostic" "$output" >&2; exit 1 ;;
    esac
    passed=$((passed + 1))
    printf 'PASS %s\n' "$name"
}

run_case fresh-pre 0 '' '/usr/bin/sh "$1" 1'
run_case upgrade-refused 1 'only fresh installations' '/usr/bin/sh "$1" 2'
run_case erase-count-refused 1 'only fresh installations' '/usr/bin/sh "$1" 0'
run_case missing-count-refused 1 'only fresh installations' '/usr/bin/sh "$1"'
run_case existing-data-preserved 0 'existing installations' '
    mkdir /var/lib/anchor
    echo keep > /var/lib/anchor/fact
    if /usr/bin/sh "$1" 1; then exit 91; fi
    test "$(cat /var/lib/anchor/fact)" = keep
'
run_case dangling-home-refused 0 'existing installations' '
    ln -s /missing-anchor-home /var/lib/anchor
    if /usr/bin/sh "$1" 1; then exit 92; fi
    test -L /var/lib/anchor
'
run_case operator-override-refused 0 'existing installations' '
    mkdir /etc/systemd/system/anchor.service.d
    if /usr/bin/sh "$1" 1; then exit 93; fi
    test -d /etc/systemd/system/anchor.service.d
'
run_case vendor-unit-refused 0 'existing installations' '
    echo keep > /usr/lib/systemd/system/anchor.service
    if /usr/bin/sh "$1" 1; then exit 94; fi
    test "$(cat /usr/lib/systemd/system/anchor.service)" = keep
'
run_case desktop-entry-refused 0 'existing installations' '
    echo keep > /usr/share/applications/anchor.desktop
    if /usr/bin/sh "$1" 1; then exit 96; fi
    test "$(cat /usr/share/applications/anchor.desktop)" = keep
'
run_case existing-account-refused 1 'passwd entry already exists' \
    '/usr/bin/sh "$1" 1' "$scratch/passwd-anchor"
run_case existing-group-refused 1 'group entry already exists' \
    '/usr/bin/sh "$1" 1' "$scratch/passwd" "$scratch/group-anchor"
run_case post-existing-data-preserved 0 'unexpected existing data' '
    . "$5"
    mkdir -p /var/lib/anchor/platform/data/state
    echo keep > /var/lib/anchor/platform/data/state/fact
    if /usr/bin/sh "$2" 1; then exit 95; fi
    test "$(cat /var/lib/anchor/platform/data/state/fact)" = keep
'
run_case post-upgrade-refused 1 'outside a fresh installation' '/usr/bin/sh "$2" 2'
run_case post-fresh-and-retry 0 'inactive system' '
    . "$5"
    /usr/bin/sh "$2" 1
    echo keep > /var/lib/anchor/platform/data/state/fact
    /usr/bin/sh "$2" 1
    test "$(cat /var/lib/anchor/platform/data/state/fact)" = keep
'
run_case post-partial-group 0 'inactive system' '
    . "$5"
    groupadd --system anchor
    /usr/bin/sh "$2" 1
    test -d /var/lib/anchor/platform/data/workspaces
'
run_case post-partial-account 0 'inactive system' '
    . "$5"
    groupadd --system anchor
    useradd --system --gid anchor --home-dir /var/lib/anchor --no-create-home --shell /sbin/nologin anchor
    /usr/bin/sh "$2" 1
    test -d /var/lib/anchor/platform/data/workspaces
'
run_case post-partial-directories 0 'inactive system' '
    . "$5"
    groupadd --system anchor
    useradd --system --gid anchor --home-dir /var/lib/anchor --no-create-home --shell /sbin/nologin anchor
    install -d -o 12345 -g 12345 -m 0700 /var/lib/anchor/platform/data
    runuser -u anchor -- mkdir -m 0700 -- /var/lib/anchor/platform/data/state
    echo keep > /var/lib/anchor/platform/data/state/fact
    /usr/bin/sh "$2" 1
    test "$(cat /var/lib/anchor/platform/data/state/fact)" = keep
'
run_case post-unsafe-account-refused 1 'non-login service account' '
    . "$5"
    groupadd --system anchor
    echo "anchor:x:12345:12345:fixture:/root:/bin/sh" > /run/fixture/user
    /usr/bin/sh "$2" 1
'
run_case post-unsafe-directory-refused 0 'expected owner and mode' '
    . "$5"
    /usr/bin/sh "$2" 1
    chmod 0755 /var/lib/anchor/platform/data/state
    if /usr/bin/sh "$2" 1; then exit 97; fi
    test "$(/usr/bin/fixture-stat -c %a /var/lib/anchor/platform/data/state)" = 755
'
run_case post-data-link-refused 0 'expected owner and mode' '
    . "$5"
    groupadd --system anchor
    useradd --system --gid anchor --home-dir /var/lib/anchor --no-create-home --shell /sbin/nologin anchor
    ln -s /run/fixture /var/lib/anchor/platform/data
    if /usr/bin/sh "$2" 1; then exit 98; fi
    test -L /var/lib/anchor/platform/data
'
run_case post-failed-start-retry 0 'First configure a model service' '
    . "$5"
    mkdir -p /run/systemd/system
    cat > /usr/bin/systemctl <<\MOCK
#!/usr/bin/sh
if [ "$1" = enable ] && [ ! -f /run/fixture/start-failed ]; then
    echo failed > /run/fixture/start-failed
    exit 42
fi
MOCK
    chmod 0700 /usr/bin/systemctl
    if /usr/bin/sh "$2" 1; then exit 99; fi
    echo keep > /var/lib/anchor/platform/data/state/fact
    /usr/bin/sh "$2" 1
    test "$(cat /var/lib/anchor/platform/data/state/fact)" = keep
'
run_case offline-removal-preserves-data 0 '' '
    mkdir -p /var/lib/anchor/platform/data/state
    echo keep > /var/lib/anchor/platform/data/state/fact
    /usr/bin/sh "$3" 0
    /usr/bin/sh "$4" 0
    test "$(cat /var/lib/anchor/platform/data/state/fact)" = keep
'
run_case failed-stop-blocks-removal 0 '' '
    mkdir -p /run/systemd/system /var/lib/anchor/platform/data/state
    echo keep > /var/lib/anchor/platform/data/state/fact
    cat > /usr/bin/systemctl <<\MOCK
#!/usr/bin/sh
echo "$*" > /run/systemctl-called
exit 42
MOCK
    chmod 0700 /usr/bin/systemctl
    set +e
    /usr/bin/sh "$3" 0
    status=$?
    set -e
    test "$status" = 42
    test "$(cat /run/systemctl-called)" = "disable --now anchor.service"
    test "$(cat /var/lib/anchor/platform/data/state/fact)" = keep
'

run_case deb-fresh-preinst 0 '' '/usr/bin/sh "$6" install'
run_case deb-upgrade-preinst-refused 1 'only fresh installations' '/usr/bin/sh "$6" upgrade 0.1.0'
run_case deb-preserved-install-refused 1 'only fresh installations' '/usr/bin/sh "$6" install 0.1.0'
run_case deb-configure-retry 0 'inactive system' '
    . "$5"
    /usr/bin/sh "$7" configure
    echo keep > /var/lib/anchor/platform/data/state/fact
    /usr/bin/sh "$7" configure ""
    test "$(cat /var/lib/anchor/platform/data/state/fact)" = keep
'
run_case deb-old-configure-refused 1 'outside a fresh installation' '/usr/bin/sh "$7" configure 0.1.0'
run_case deb-prerm-upgrade-refused 1 'migration contract' '/usr/bin/sh "$8" upgrade 0.2.0'
run_case deb-purge-preserves-data 0 '' '
    mkdir -p /var/lib/anchor/platform/data/state
    echo keep > /var/lib/anchor/platform/data/state/fact
    /usr/bin/sh "$8" remove
    /usr/bin/sh "$9" remove
    /usr/bin/sh "$9" purge
    test "$(cat /var/lib/anchor/platform/data/state/fact)" = keep
'

printf '%s lifecycle checks passed; root installation and live systemd were not exercised.\n' "$passed"
