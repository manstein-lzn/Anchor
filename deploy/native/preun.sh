#!/bin/sh
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

# Failed service shutdown must stop removal, not leave children without their release.
if [ "$#" -eq 1 ] && [ "$1" = 0 ] && [ -d /run/systemd/system ]; then
    systemctl disable --now anchor.service
fi
