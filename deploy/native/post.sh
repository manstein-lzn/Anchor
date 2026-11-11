#!/bin/sh
# Package-owned release files and private mutable data have separate owners.
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

fail() {
    echo "Anchor package: $*" >&2
    exit 1
}

if [ "$#" -ne 1 ] || [ "$1" != 1 ]; then
    fail 'refusing configuration outside a fresh installation.'
fi

check_directory() {
    if [ ! -d "$1" ] || [ -L "$1" ] || [ "$(stat -c '%u:%g:%a' -- "$1")" != "$2:$3:$4" ]; then
        fail "$1 must be a real directory with the expected owner and mode; existing data is never adopted or repaired."
    fi
}

positive_id() {
    case "$1" in ''|*[!0-9]*) return 1 ;; esac
    [ "$1" -gt 0 ]
}

load_group() {
    if record=$(getent group anchor); then
        IFS=: read -r group_name group_password group_gid group_members <<EOF
$record
EOF
        if [ "$group_name" != anchor ] || ! positive_id "$group_gid" || [ -n "$group_members" ]; then
            fail 'the existing anchor group does not match the dedicated service group.'
        fi
        group_exists=1
    else
        status=$?
        [ "$status" = 2 ] || fail 'cannot inspect the service group.'
        group_exists=0
    fi
}

load_user() {
    if record=$(getent passwd anchor); then
        IFS=: read -r user_name user_password user_uid user_gid user_gecos user_home user_shell <<EOF
$record
EOF
        if [ "$group_exists" != 1 ] || [ "$user_name" != anchor ] || ! positive_id "$user_uid" || \
            [ "$user_gid" != "$group_gid" ] || [ "$user_home" != /var/lib/anchor ] || \
            [ "$user_shell" != /sbin/nologin ]; then
            fail 'the existing anchor account does not match the dedicated non-login service account.'
        fi
        user_exists=1
    else
        status=$?
        [ "$status" = 2 ] || fail 'cannot inspect the service account.'
        user_exists=0
    fi
}

for directory in /var /var/lib /var/lib/anchor /var/lib/anchor/platform; do
    check_directory "$directory" 0 0 755
done
check_directory /var/lib/anchor/platform/release 0 0 555

load_group
load_user
data=/var/lib/anchor/platform/data
children='home state catalog library workspaces'
for child in '' $children; do
    directory=$data
    [ -z "$child" ] || directory=$data/$child
    if [ -e "$directory" ] || [ -L "$directory" ]; then
        [ "$user_exists" = 1 ] || fail 'unexpected existing data without its dedicated account; no state was changed.'
        check_directory "$directory" "$user_uid" "$group_gid" 700
    fi
done

# A failed configure may have already created its group, account or directories.
if [ "$group_exists" = 0 ]; then
    groupadd --system anchor
    load_group
fi
if [ "$user_exists" = 0 ]; then
    useradd --system --gid anchor --home-dir /var/lib/anchor \
        --no-create-home --shell /sbin/nologin anchor
    load_user
fi

if [ ! -d "$data" ]; then
    install -d -o "$user_uid" -g "$group_gid" -m 0700 "$data"
fi

# Never perform root ownership/mode changes below a service-owned directory.
for child in $children; do
    if [ ! -d "$data/$child" ]; then
        runuser -u anchor -- mkdir -m 0700 -- "$data/$child"
    fi
done

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
    if ! systemctl enable --now anchor.service; then
        echo 'Anchor package is installed, but its service did not start. Inspect systemctl status anchor.service and journalctl -u anchor.service.' >&2
        exit 1
    fi
    echo 'Open http://127.0.0.1:8077. First configure a model service in WebUI Settings.'
else
    echo 'Anchor package is installed into an inactive system; enable and start anchor.service after boot.'
fi
