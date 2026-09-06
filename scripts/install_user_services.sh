#!/usr/bin/env bash
set -euo pipefail
anchor_root="${ANCHOR_ROOT:-$HOME/Anchor}"
unit_dir="$HOME/.config/systemd/user"
mkdir -p "$unit_dir"
install -m 0644 "$anchor_root/infra/systemd/anchor-worker.service" "$unit_dir/anchor-worker.service"
install -m 0644 "$anchor_root/infra/systemd/anchor-control-worker.service" "$unit_dir/anchor-control-worker.service"
install -m 0644 "$anchor_root/infra/systemd/anchor-verifier-worker.service" "$unit_dir/anchor-verifier-worker.service"
install -m 0644 "$anchor_root/infra/systemd/anchor-scheduler.service" "$unit_dir/anchor-scheduler.service"
install -m 0644 "$anchor_root/infra/systemd/anchor-supervisor.service" "$unit_dir/anchor-supervisor.service"
systemctl --user daemon-reload
systemctl --user enable --now anchor-worker.service anchor-control-worker.service anchor-verifier-worker.service anchor-scheduler.service anchor-supervisor.service
systemctl --user --no-pager --full status anchor-worker.service anchor-control-worker.service anchor-verifier-worker.service anchor-scheduler.service anchor-supervisor.service
