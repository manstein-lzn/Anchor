#!/usr/bin/env python3
"""Import model API keys from the Pi agent model config into Anchor's secret file.

Anchor resolves secrets only through a runtime `SecretProvider`; this script is
the explicit one-time bridge from Pi's `models.json` layout to Anchor's flat
`{ref: value}` secret file. It never prints key material.

Usage:
    .venv/bin/python scripts/import_pi_secrets.py \
        --pi-config /home/mansteinl/.pi/agent/models.json \
        --target /home/mansteinl/Anchor/.local/anchor-secrets.json
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path


# provider name in Pi's config -> (Anchor secret ref, profile provider)
PROVIDER_SECRET_REFS = {
    "DeepSeek": "DEEPSEEK_API_KEY",
    "cwiseapi": "CWISEAPI_API_KEY",
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pi-config", default="/home/mansteinl/.pi/agent/models.json")
    parser.add_argument("--target", default="/home/mansteinl/Anchor/.local/anchor-secrets.json")
    args = parser.parse_args()

    config = json.loads(Path(args.pi_config).read_text(encoding="utf-8"))
    target = Path(args.target)
    secrets = json.loads(target.read_text(encoding="utf-8")) if target.exists() else {}

    imported = []
    for provider, body in (config.get("providers") or {}).items():
        ref = PROVIDER_SECRET_REFS.get(provider)
        key = body.get("apiKey")
        if ref and isinstance(key, str) and key and not key.startswith("!"):
            secrets[ref] = key
            imported.append(f"{provider} -> {ref}")

    target.write_text(json.dumps(secrets, indent=4) + "\n", encoding="utf-8")
    target.chmod(0o600)
    print(f"wrote {len(imported)} secret reference(s) to {target}:")
    for item in imported:
        print(f"  {item}")


if __name__ == "__main__":
    main()
