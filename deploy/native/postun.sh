#!/bin/sh
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
fi

# The package manager removes its release files only. Keep data and the account.
