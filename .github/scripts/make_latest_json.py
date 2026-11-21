"""Build the updater manifest (latest.json) for a release from the downloaded .sig files.

Usage: python make_latest_json.py <tag> <repo owner/name> <folder with .sig files>
"""

from __future__ import annotations

import datetime as dt
import json
import sys
from pathlib import Path

# Updater platform -> asset uploaded by release.yml
ASSETS = {
    "darwin-aarch64": "PC.Bridge.app.tar.gz",
    "windows-x86_64": "PC.Bridge.Setup.exe",
}


def manifest(tag: str, repo: str, sig_dir: Path) -> dict:
    base = f"https://github.com/{repo}/releases/download/{tag}/"
    platforms = {}
    for platform, asset in ASSETS.items():
        sig = sig_dir / f"{asset}.sig"
        if sig.is_file():
            platforms[platform] = {"signature": sig.read_text().strip(), "url": base + asset}
    if not platforms:
        raise SystemExit("No .sig files found: nothing to publish")
    return {
        "version": tag.removeprefix("v"),
        "notes": f"See https://github.com/{repo}/releases/tag/{tag}",
        "pub_date": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": platforms,
    }


if __name__ == "__main__":
    print(json.dumps(manifest(sys.argv[1], sys.argv[2], Path(sys.argv[3])), indent=2))
