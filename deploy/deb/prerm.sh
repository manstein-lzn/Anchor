#!/bin/sh
# Prefix for the shared native pre-removal script.
set -eu
case "${1:-}" in
    remove) set -- 0 ;;
    failed-upgrade) exit 0 ;;
    upgrade|deconfigure)
        echo 'Anchor package: upgrades and deconfiguration require a separate migration contract.' >&2
        exit 1
        ;;
    *) echo 'Anchor package: unsupported pre-removal operation.' >&2; exit 1 ;;
esac
