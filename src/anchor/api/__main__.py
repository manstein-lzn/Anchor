"""Loopback-only development launcher; no implicit database migration."""

import argparse
import os
import secrets
from pathlib import Path

import uvicorn


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8090)
    parser.add_argument("--token-file", type=Path, default=Path(".local/api-token"))
    args = parser.parse_args()
    if not os.environ.get("ANCHOR_DATABASE_URL"):
        parser.error("set ANCHOR_DATABASE_URL and apply migrations first")
    if not os.environ.get("ANCHOR_API_TOKEN"):
        args.token_file.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        try:
            descriptor = os.open(args.token_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            pass
        else:
            with os.fdopen(descriptor, "w") as output:
                output.write(secrets.token_urlsafe(32))
        os.environ["ANCHOR_API_TOKEN"] = args.token_file.read_text().strip()
    uvicorn.run("anchor.api.app:create_app", factory=True, host="127.0.0.1", port=args.port,
                proxy_headers=False)


if __name__ == "__main__":
    main()
