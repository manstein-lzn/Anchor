#!/bin/sh
# Prefix for the shared native pre-install script.
set -eu
case "${1:-}" in
    install)
        if [ "$#" = 1 ]; then set -- 1; else set -- 2; fi
        ;;
    upgrade) set -- 2 ;;
    abort-upgrade) exit 0 ;;
    *) echo 'Anchor package: unsupported pre-install operation.' >&2; exit 1 ;;
esac
