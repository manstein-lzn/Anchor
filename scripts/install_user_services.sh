#!/usr/bin/env bash
# Install Anchor's persistent user services and refuse to claim success unless they run.
#
# These units are what make a long run survive a host reboot. Before them the services
# were started as transient units, which do not come back: a run would stall at reboot and
# nothing in `systemctl --user` said so, because the unit simply no longer existed.
#
# The script enables six units and then *checks* them. A unit that fails five starts is
# reported by systemd as `failed`; this script surfaces that instead of printing "enabled"
# and leaving an operator to discover it after the next reboot.
set -euo pipefail

anchor_root="${ANCHOR_ROOT:-$HOME/Anchor}"
unit_dir="$HOME/.config/systemd/user"
units=(
  anchor-api.service
  anchor-worker.service
  anchor-control-worker.service
  anchor-verifier-worker.service
  anchor-receiver.service
  anchor-scheduler.service
  anchor-supervisor.service
)

if [[ ! -d "$anchor_root/.venv" ]]; then
  echo "no virtualenv at $anchor_root/.venv; set ANCHOR_ROOT or create it first" >&2
  exit 2
fi

mkdir -p "$unit_dir"
for unit in "${units[@]}"; do
  install -m 0644 "$anchor_root/infra/systemd/$unit" "$unit_dir/$unit"
done
systemctl --user daemon-reload

# Start from a clean slate so a stale failed state from an earlier attempt is not mistaken
# for the outcome of this one.
systemctl --user reset-failed "${units[@]}" 2>/dev/null || true
systemctl --user enable --now "${units[@]}"

# A service that cannot start exits with code 2 and prints a JSON reason to stderr. Give
# systemd a moment to reach either "active" or "failed" before judging, because
# `enable --now` returns as soon as the job is queued, not when the unit has settled.
sleep 3

failed=()
for unit in "${units[@]}"; do
  state="$(systemctl --user is-active "$unit" 2>/dev/null || true)"
  if [[ "$state" != "active" ]]; then
    failed+=("$unit=$state")
  fi
done

if ((${#failed[@]})); then
  echo "these services did not start: ${failed[*]}" >&2
  echo >&2
  for entry in "${failed[@]}"; do
    unit="${entry%%=*}"
    echo "── $unit ──" >&2
    systemctl --user status --no-pager --full "$unit" >&2 || true
    journalctl --user -u "$unit" -n 20 --no-pager >&2 || true
  done
  exit 1
fi

echo "all ${#units[@]} services are active"
systemctl --user --no-pager --full status "${units[@]}"
