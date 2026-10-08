#!/usr/bin/env python3
"""Download the runtime assets depth needs.

Dev-time helper only: the shipped application never touches the network.

Usage:
    python scripts/fetch_assets.py whistle          # Cactus needle engine + whistle.cact
    python scripts/fetch_assets.py whisper          # whisper.cpp CLI binary
    python scripts/fetch_assets.py model --size small
    python scripts/fetch_assets.py all

Assets land in vendor/ and models/, relative to the repository root, matching the paths the app
reads at runtime.
"""

from __future__ import annotations

import argparse
import io
import sys
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

NEEDLE3 = "https://huggingface.co/Cactus-Compute/needle3/resolve/main"
WHISTLE = "https://huggingface.co/Cactus-Compute/whistle/resolve/main"
WHISPER_MODELS = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main"

# Prebuilt whisper.cpp CLI for Windows x64. Pinned to a nightly because the versioned releases
# carry no binary assets; bump this tag to pick up a newer build.
WHISPER_BIN_TAG = "b5130"
WHISPER_BIN_ZIP = f"https://github.com/ggml-org/whisper.cpp/releases/download/{WHISPER_BIN_TAG}/whisper-bin-x64.zip"

# Whisper checkpoints usable for the Indonesian path, smallest first.
WHISPER_SIZES = {
    "tiny": "ggml-tiny-q5_1.bin",
    "base": "ggml-base-q5_1.bin",
    "small": "ggml-small-q5_1.bin",
    "medium": "ggml-medium-q5_0.bin",
}

ENGINE_FILES = [
    ("needle.h", "vendor/needle/windows-x86_64/needle.h"),
    ("libneedle.a", "vendor/needle/windows-x86_64/libneedle.a"),
    ("needle.exe", "vendor/needle/windows-x86_64/needle.exe"),
]


def read_url(url: str, on_progress=None) -> bytes:
    """Fetch a URL into memory, reporting progress when a callback is given."""
    req = urllib.request.Request(url, headers={"User-Agent": "depth-fetch/1.0"})
    try:
        with urllib.request.urlopen(req, timeout=600) as resp:
            total = int(resp.headers.get("Content-Length") or 0)
            chunks: list[bytes] = []
            done = 0
            while chunk := resp.read(1 << 20):
                chunks.append(chunk)
                done += len(chunk)
                if on_progress:
                    on_progress(done, total)
            return b"".join(chunks)
    except urllib.error.URLError as exc:  # DNS, TLS, HTTP status
        raise SystemExit(f"FAILED {url}\n  {exc}") from exc


def download(url: str, dest: Path) -> None:
    """Stream a URL to a file, writing through a `.part` sibling."""
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_suffix(dest.suffix + ".part")
    req = urllib.request.Request(url, headers={"User-Agent": "depth-fetch/1.0"})
    try:
        with urllib.request.urlopen(req, timeout=600) as resp:
            total = int(resp.headers.get("Content-Length") or 0)
            done = 0
            with tmp.open("wb") as fh:
                while chunk := resp.read(1 << 20):
                    fh.write(chunk)
                    done += len(chunk)
                    if total:
                        print(f"\r  {dest.name}: {100 * done / total:5.1f}% ({done:,}/{total:,} B)", end="")
                    else:
                        print(f"\r  {dest.name}: {done:,} B", end="")
        print()
    except urllib.error.URLError as exc:
        tmp.unlink(missing_ok=True)
        raise SystemExit(f"FAILED {url}\n  {exc}") from exc
    tmp.replace(dest)
    print(f"  -> {dest.relative_to(ROOT)} ({dest.stat().st_size:,} bytes)")


def fetch_whistle() -> None:
    """The Cactus engine (English) and its model."""
    print("Whistle engine (windows-x86_64) + model")
    for name, rel in ENGINE_FILES:
        dest = ROOT / rel
        if dest.exists() and dest.stat().st_size > 0:
            print(f"  = {rel} already present ({dest.stat().st_size:,} bytes)")
            continue
        download(f"{NEEDLE3}/windows-x86_64/{name}", dest)
    model = ROOT / "models" / "whistle.cact"
    if model.exists() and model.stat().st_size > 0:
        print(f"  = models/whistle.cact already present ({model.stat().st_size:,} bytes)")
    else:
        download(f"{WHISTLE}/whistle.cact", model)


def fetch_whisper_bin() -> None:
    """The prebuilt whisper.cpp CLI used by the Indonesian sidecar."""
    dest = ROOT / "vendor" / "whisper"
    existing = list(dest.glob("whisper-cli.exe")) + list(dest.glob("main.exe"))
    if existing:
        print(f"  = vendor/whisper already present ({existing[0].name})")
        return
    print(f"whisper.cpp prebuilt CLI ({WHISPER_BIN_TAG})")

    def progress(done: int, total: int) -> None:
        if total:
            print(f"\r  whisper-bin-x64.zip: {100 * done / total:5.1f}%", end="")

    data = read_url(WHISPER_BIN_ZIP, progress)
    print()
    dest.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        archive.extractall(dest)
    for produced in sorted(p.name for p in dest.iterdir()):
        print(f"  -> vendor/whisper/{produced}")


def fetch_model(size: str) -> None:
    """A whisper.cpp GGML checkpoint for the Indonesian path."""
    filename = WHISPER_SIZES[size]
    dest = ROOT / "models" / filename
    print(f"Whisper checkpoint: {size}")
    if dest.exists() and dest.stat().st_size > 0:
        print(f"  = models/{filename} already present ({dest.stat().st_size:,} bytes)")
        return
    download(f"{WHISPER_MODELS}/{filename}", dest)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("target", choices=["whistle", "whisper", "model", "all"])
    ap.add_argument("--size", choices=sorted(WHISPER_SIZES), default="small",
                    help="whisper checkpoint size for the Indonesian path (default: small)")
    args = ap.parse_args()

    if args.target in ("whistle", "all"):
        fetch_whistle()
    if args.target in ("whisper", "all"):
        fetch_whisper_bin()
    if args.target in ("model", "all"):
        fetch_model(args.size)
    return 0


if __name__ == "__main__":
    sys.exit(main())
