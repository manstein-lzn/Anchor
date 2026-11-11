#!/bin/sh
# Prefix for the shared native configure script, including configure retries.
set -eu
case "${1:-}" in
    configure)
        if [ "$#" -le 2 ] && [ -z "${2:-}" ]; then set -- 1; else set -- 2; fi
        ;;
    abort-upgrade|abort-remove|abort-deconfigure) exit 0 ;;
    *) echo 'Anchor package: unsupported configure operation.' >&2; exit 1 ;;
esac
