#!/bin/sh
# Prefix for the shared native post-removal script; purge also preserves data.
set -eu
case "${1:-}" in
    remove|purge) set -- 0 ;;
    upgrade|failed-upgrade|abort-install|abort-upgrade|disappear) exit 0 ;;
    *) echo 'Anchor package: unsupported post-removal operation.' >&2; exit 1 ;;
esac
