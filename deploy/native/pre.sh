#!/bin/sh
# Refuse takeover before the package manager writes any files.
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

fail() {
    echo "Anchor package: $*" >&2
    exit 1
}

if [ "$#" -ne 1 ] || [ "$1" != 1 ]; then
    fail 'only fresh installations are supported; upgrades require a separate migration contract.'
fi

for directory in /var /var/lib; do
    if [ ! -d "$directory" ] || [ -L "$directory" ]; then
        fail "$directory must be a real system directory, not a link."
    fi
done

for path in \
    /var/lib/anchor \
    /usr/bin/anchor \
    /usr/share/applications/anchor.desktop \
    /usr/lib/systemd/system/anchor.service \
    /usr/lib/systemd/system/anchor.service.d \
    /etc/systemd/system/anchor.service \
    /etc/systemd/system/anchor.service.d \
    /run/systemd/system/anchor.service \
    /run/systemd/system/anchor.service.d \
    /run/systemd/transient/anchor.service
do
    if [ -e "$path" ] || [ -L "$path" ]; then
        fail "$path already exists; existing installations, overrides and preserved data are never replaced."
    fi
done

for database in passwd group; do
    if getent "$database" anchor >/dev/null; then
        fail "the anchor $database entry already exists; this fresh installer never reuses an existing account or group."
    else
        [ "$?" = 2 ] || fail "cannot inspect the $database database."
    fi
done
