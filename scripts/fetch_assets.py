#!/usr/bin/env python3
"""Download the runtime assets depth needs.

Installer asset helper. The application also offers opt-in model downloads.

Usage:
    python scripts/fetch_assets.py whistle          # Cactus needle engine + whistle.cact
    python scripts/fetch_assets.py whisper          # whisper.cpp CLI binary
    python scripts/fetch_assets.py model --size small
    python scripts/fetch_assets.py speakers         # local diarization models/runtime/licenses
    python scripts/fetch_assets.py all

Assets land in vendor/ and models/, relative to the repository root, matching the paths the app
reads at runtime.
"""

from __future__ import annotations

import argparse
import json
import hashlib
import io
import sys
import urllib.error
import urllib.request
import zipfile
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

NEEDLE3 = "https://huggingface.co/Cactus-Compute/needle3/resolve/main"
WHISTLE = "https://huggingface.co/Cactus-Compute/whistle/resolve/main"
WHISPER_MODELS = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main"

# Matching prebuilt CPU and CUDA runners pinned to one whisper.cpp revision.
SHERPA_VERSION = "1.13.8"
WHISPER_BIN_TAG = "b5130"
WHISPER_BIN_ZIP = f"https://github.com/ggml-org/whisper.cpp/releases/download/{WHISPER_BIN_TAG}/whisper-bin-x64.zip"

# Whisper checkpoints usable for the Indonesian path, smallest first.
MODEL_CATALOG = json.loads((ROOT / "assets" / "asr-models.json").read_text(encoding="utf-8"))
WHISPER_SIZES = {model["id"]: model["filename"] for model in MODEL_CATALOG}
WHISPER_RUNTIMES = {
    "cpu": ("whisper-bin-x64.zip", "f9ec6c52a2e949b62ab51fa21d0d497958f9e41c3010c157c4e42932d5316f3c"),
    "cuda": ("whisper-cublas-12.4.0-bin-x64.zip", "af520ddd034d985b55dfeea3e465ed93653ba2aee1a55e865033edc548c272a7"),
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
    """Isolated CPU and CUDA runners from one pinned release, verified before extraction."""
    for runtime, (archive_name, digest) in WHISPER_RUNTIMES.items():
        dest = ROOT / "vendor" / "whisper" / runtime
        stamp = dest / "runtime.sha256"
        if stamp.exists() and stamp.read_text().strip() == digest and (dest / "whisper-cli.exe").exists():
            print(f"  = Whisper {runtime} {WHISPER_BIN_TAG} already installed")
            continue
        archive_path = ROOT / ".scratch" / "downloads" / archive_name
        archive_path.parent.mkdir(parents=True, exist_ok=True)
        cached = False
        if archive_path.exists():
            with archive_path.open("rb") as stream:
                cached = hashlib.file_digest(stream, "sha256").hexdigest() == digest
        if not cached:
            download(f"https://github.com/ggml-org/whisper.cpp/releases/download/{WHISPER_BIN_TAG}/{archive_name}", archive_path)
        with archive_path.open("rb") as stream:
            if hashlib.file_digest(stream, "sha256").hexdigest() != digest:
                raise SystemExit(f"Checksum mismatch for {archive_name}")
        dest.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(archive_path) as archive:
            for entry in archive.infolist():
                filename = Path(entry.filename).name
                if filename and Path(filename).suffix.lower() in (".exe", ".dll", ".txt", ".md"):
                    (dest / filename).write_bytes(archive.read(entry))
        if not (dest / "whisper-cli.exe").exists():
            raise SystemExit(f"Runner missing from {archive_name}")
        stamp.write_text(digest + "\n", encoding="utf-8")
    licenses = ROOT / "vendor" / "whisper" / "licenses"
    licenses.mkdir(parents=True, exist_ok=True)
    download(f"https://raw.githubusercontent.com/ggml-org/whisper.cpp/{WHISPER_BIN_TAG}/LICENSE", licenses / "whisper-LICENSE.txt")
    download("https://docs.nvidia.com/cuda/eula/index.html", licenses / "NVIDIA-CUDA-EULA.html")
    (licenses / "NOTICE.txt").write_text("Whisper CPU and CUDA runners: whisper.cpp " + WHISPER_BIN_TAG +
        "\nCUDA 12.4 redistributable libraries are licensed by NVIDIA under the included CUDA EULA.\n", encoding="utf-8")


def fetch_model(size: str) -> None:
    model = next(m for m in MODEL_CATALOG if m["id"] == size)
    dest = ROOT / "models" / model["filename"]
    licenses = ROOT / "models" / "licenses"
    licenses.mkdir(parents=True, exist_ok=True)
    (licenses / (model["id"] + "-NOTICE.txt")).write_text(
        model["name"] + "\nSource: " + model["source"] + "\nRevision: " + model["revision"] +
        "\nLicense: " + model["license"] + "\n", encoding="utf-8")
    license_name = "Apache-2.0.txt" if model["license"] == "Apache-2.0" else "Whisper-MIT.txt"
    (licenses / license_name).write_bytes((ROOT / "assets" / "licenses" / license_name).read_bytes())
    if dest.exists():
        with dest.open("rb") as stream:
            if dest.stat().st_size == model["size"] and hashlib.file_digest(stream, "sha256").hexdigest() == model["sha256"]:
                print(f"  = {model['name']} verified")
                return
    dest.parent.mkdir(parents=True, exist_ok=True)
    temporary = dest.with_suffix(".part")
    download(model["url"], temporary)
    with temporary.open("rb") as stream:
        verified = temporary.stat().st_size == model["size"] and hashlib.file_digest(stream, "sha256").hexdigest() == model["sha256"]
    if not verified:
        temporary.unlink(missing_ok=True)
        raise SystemExit(f"Checksum/size mismatch: {model['name']}")
    temporary.replace(dest)


def fetch_speakers() -> None:
    """Pinned CPU runtime, segmentation, embeddings, and upstream license notices."""
    base = "https://github.com/k2-fsa/sherpa-onnx/releases/download"
    models = ROOT / "models"
    notices = ROOT / "vendor" / "speakers" / "licenses"
    notices.mkdir(parents=True, exist_ok=True)
    segmentation = models / "speaker-segmentation.int8.onnx"
    if not segmentation.exists() or not (notices / "pyannote-LICENSE").exists():
        data = read_url(f"{base}/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2")
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:bz2") as archive:
            for member in archive.getmembers():
                name = Path(member.name).name
                if not member.isfile() or name not in ("model.int8.onnx", "LICENSE"):
                    continue
                target = segmentation if name == "model.int8.onnx" else notices / "pyannote-LICENSE"
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(archive.extractfile(member).read())
        if not segmentation.is_file():
            raise SystemExit("Speaker segmentation archive is missing model.int8.onnx")
    embedding = models / "nemo_en_titanet_small.onnx"
    if not embedding.exists():
        download(f"{base}/speaker-recongition-models/nemo_en_titanet_small.onnx", embedding)
    runtime = ROOT / "vendor" / "speakers" / "lib"
    required = ["sherpa-onnx-c-api.dll", "onnxruntime.dll", "onnxruntime_providers_shared.dll"]
    if not all((runtime / name).is_file() for name in required):
        data = read_url(f"{base}/v{SHERPA_VERSION}/sherpa-onnx-v{SHERPA_VERSION}-win-x64-shared-MT-Release-lib.tar.bz2")
        runtime.mkdir(parents=True, exist_ok=True)
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:bz2") as archive:
            for member in archive.getmembers():
                name = Path(member.name).name
                if member.isfile() and name.endswith((".dll", ".lib")):
                    (runtime / name).write_bytes(archive.extractfile(member).read())
    for url, name in [
        (f"https://raw.githubusercontent.com/k2-fsa/sherpa-onnx/v{SHERPA_VERSION}/LICENSE", "sherpa-onnx-LICENSE"),
        ("https://raw.githubusercontent.com/NVIDIA/NeMo/main/LICENSE", "NeMo-TitaNet-LICENSE"),
        ("https://raw.githubusercontent.com/microsoft/onnxruntime/main/LICENSE", "onnxruntime-LICENSE"),
    ]:
        if not (notices / name).exists():
            download(url, notices / name)
    (notices / "NOTICE.txt").write_text(
        "Speaker detection uses Sherpa-ONNX 1.13.8 and ONNX Runtime.\n"
        "Pyannote segmentation-3.0 was converted/quantized to ONNX by Sherpa-ONNX.\n"
        "TitaNet Small by NVIDIA was converted to ONNX by Sherpa-ONNX.\n"
        "The NVIDIA NGC titanet_small model card licenses this model under the NeMo toolkit license.\n"
        "Model card: https://api.ngc.nvidia.com/v2/models/nvidia/nemo/titanet_small\n"
        "Segmentation: https://github.com/k2-fsa/sherpa-onnx/releases/tag/speaker-segmentation-models\n"
        "Embeddings: https://github.com/k2-fsa/sherpa-onnx/releases/tag/speaker-recongition-models\n"
        "See the bundled upstream LICENSE files. No model training was performed by Depth.\n",
        encoding="utf-8",
    )
    print("Speaker models and CPU runtime ready. Transcription runs offline; optional ASR models download from Settings.")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("target", choices=["whistle", "whisper", "model", "speakers", "all"])
    ap.add_argument("--size", choices=sorted(WHISPER_SIZES), default="small-id",
                    help="whisper checkpoint size for the Indonesian path (default: small-id)")
    args = ap.parse_args()

    if args.target in ("whistle", "all"):
        fetch_whistle()
    if args.target in ("whisper", "all"):
        fetch_whisper_bin()
    if args.target in ("model", "all"):
        fetch_model(args.size)
        if args.target == "all":
            if args.size != "small-id":
                fetch_model("small-id")
            fetch_model("base")
    if args.target in ("speakers", "all"):
        fetch_speakers()
    return 0


if __name__ == "__main__":
    sys.exit(main())
