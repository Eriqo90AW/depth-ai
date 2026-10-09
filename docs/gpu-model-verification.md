# GPU inference and ASR model verification

Implemented and measured on 2026-10-09. NVIDIA GeForce RTX 2060 Mobile, 6 GiB VRAM; Ryzen 7 4800H, 16 logical processors; NVIDIA driver 616.92; Windows x64.

## Runtime and model policy

CPU and CUDA 12.4 runners use [whisper.cpp b5130](https://github.com/ggml-org/whisper.cpp/tree/b5130). They have separate DLL directories. Asset tooling verifies release archive SHA-256 before extracting them. Application GPU discovery loads the installed CUDA driver API from System32, initializes candidate devices, and chooses the device with the most free memory. No toolkit or `nvidia-smi` is required by the app.

Auto/NVIDIA attempt CUDA, then retry the same audio once with bundled Maleo small-id on CPU if initialization, allocation, launch, inference, or timeout fails. Failed children are killed/reaped; their partial text is withheld. CPU remains active for the recording, and the saved preference is retried next time. Live drafts always disable GPU. English still uses Whistle.

Turbo stays available manually. Recommended uses the starter until turbo demonstrates at least 30 seconds of successful CUDA inference faster than incoming audio, including per-chunk model loading, with no dropped final chunks. Qualification is bound to the GPU name, checkpoint revision, and runtime revision; CUDA failure or queue loss revokes it. At least 4 GiB total / 2 GiB free VRAM is also required. This device failed that gate.

## Measured comparison

Each profile received the same existing six-second Indonesian playback fixture, repeated to 30 seconds and delivered in 100 ms blocks. Final chunks were six seconds. CPU base drafts and local speaker detection ran concurrently. Per-chunk inference time includes WAV serialization, process launch, checkpoint loading, decoding, and timed JSON parsing. The benchmark-loop wall time below also includes capture pacing, draft draining, and speaker analysis, but excludes the engine's initial checksum verification. The harness buffers up to eight draft jobs; the application coalesces previews, so these wall times do not predict its exact Stop latency. Models and runtime checksums were verified beforehand. Stock small kept the existing full encoder context and beam defaults; Maleo used its documented short context and greedy decoding. This compares the application profiles, including those decoding choices.

| Final profile | Final inference for 30 s audio | Final RTF | Recording loop + drain | Peak final queue | Final chunks dropped | Peak combined process RAM | Peak GPU allocation* |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CPU stock small Q5_1 | 26.786 s | 0.893 | 49.823 s | 1 | 0 | 868.6 MiB | 0 MiB |
| CPU Maleo small-id Q8_0 | 9.888 s | 0.330 | 46.315 s | 1 | 0 | 778.8 MiB | 0 MiB |
| CUDA turbo Q5_0, then CPU fallback | 37.219 s | 1.241 | 44.932 s | 4 | 0 | 1000.2 MiB | 889 MiB |

RTF is inference seconds / audio seconds; below 1 means final inference kept pace. The CUDA row measures the failed attempt plus CPU recovery, rather than successful turbo throughput. Its first six-second chunk took 30.157 seconds including the 20-second timeout and retry; subsequent CPU chunks took 1.55–1.94 seconds. One fallback notice was emitted. CUDA model/backend allocation succeeded before inference stalled. A second complete CUDA 12 runtime from v1.8.7 also timed out on the six-second fixture; it is not shipped. The underlying CUDA slowdown remains unresolved.

*GPU memory was sampled with `nvidia-smi` every 500 ms only for this benchmark; values are device-wide, not model-only. RAM samples cover the benchmark and its direct ASR children. Speaker windows kept pace in all profiles, with maximum analysis times of 0.251–0.291 seconds, no skipped windows, and one identified voice. Final output contained nonempty word timings. Draft draining extended Stop completion beyond 30 seconds; these are final-inference measurements, not a claim of low overall Stop latency.

## Accuracy limits

The repeated fixture contains one voice and has no independently annotated reference transcript. WER/CER and meeting accuracy were not measured. Stock small recognized “ekspor tir master besar”; Maleo produced “ekspor tirmas terbesar” in the same phrase. Both need correction there. Turbo produced no accepted GPU result, so there is no local turbo accuracy comparison.

[Maleo's model card](https://huggingface.co/maleo-ai/whisper-small-id) documents lowercase output without punctuation and its short encoder-context configuration. The implementation uses that configuration only for the matching Maleo checkpoint. The catalog also includes the [GGML conversion of Cahya medium-id](https://huggingface.co/rafdiraf/whisper-medium-id-ggml), along with stock base, small, medium, turbo, and large-v3. Upstream read-speech scores are not meeting accuracy guarantees. A longer annotated Indonesian meeting corpus and a CUDA environment that completes turbo inference are still needed for the full accuracy/throughput acceptance comparison.

## Regression checks

Configuration tests cover legacy custom paths, typed choices, invalid values, CPU preference, and Advanced settings ownership. Resolver tests cover installed explicit/custom models, missing NVIDIA/runtime, insufficient/occupied VRAM, qualification, and Recommended fallback. Download tests cover correct bytes, progress, cancellation, truncation/network failure, HTTP failure, checksum mismatch, retry after an interrupted `.part`, atomic publication, and preserving the previous verified copy on failure. Downloading never changes saved model selection.

The full all-target suite passed (147 tests, two opt-in checks excluded). A further focused regression passed for preserved custom CPU executables. The final optimized release built successfully, and both NSIS and Inno installer definitions compiled. The NSIS setup at `dist/depth-setup.exe` is the release artifact (about 999 MiB, including CUDA libraries); the Inno output is a separate definition-validation artifact. The real HTTPS download check then passed cancellation, retry, integrity verification, license publication, and unchanged saved model selection. The ignored hardware integration test was also run explicitly: simulated launch failure, allocation failure, crash, and timeout each retried once on real CPU small-id, discarded the fake partial transcript, produced final text, emitted one notice, and kept a second chunk on CPU. Existing transcript, export, speaker, queue-pressure, and Stop-draining regression tests remain part of the full suite.

Settings was rendered with the software test backend at 690 × 1040 and 560 × 760. Device/model controls, download progress, cancellation, and the fixed Save action fit; compact Settings scrolls to the remaining controls. Network and GPU detection use background threads; the UI timer only consumes their results. Native keyboard/screen-reader interaction and a physical clean Windows installation without NVIDIA hardware still require manual verification. An isolated starter-only bundle passed offline `--check`, Indonesian CPU transcription, and English Whistle transcription using the JFK fixture (22 timed words). Its working directory contained no checkout assets or optional checkpoints. The installer bundle includes every starter model and its required runner.

## Reproduce

```powershell
python scripts/fetch_assets.py all
python scripts/fetch_assets.py model --size small
python scripts/fetch_assets.py model --size turbo
.\scripts\dev-shell.ps1 cmd /c 'cargo test --all-targets --locked'
.\scripts\dev-shell.ps1 cmd /c 'cargo build --example speaker_benchmark --example settings_snapshot'
.\target\debug\examples\speaker_benchmark.exe id <16k-mono.wav> 30 cpu-stock
.\target\debug\examples\speaker_benchmark.exe id <16k-mono.wav> 30 cpu-id
.\target\debug\examples\speaker_benchmark.exe id <16k-mono.wav> 30 cuda-turbo
.\target\debug\examples\settings_snapshot.exe
```

Build fault fixtures, provide a six-second Indonesian 16 kHz mono WAV, and run the opt-in checks:

```powershell
New-Item -ItemType Directory -Force .scratch/fake-runners | Out-Null
.\scripts\dev-shell.ps1 cmd /c 'cl /nologo tests\fixtures\fake_whisper.c /Fe:.scratch\fake-runners\allocation.exe /Fo:.scratch\fake-runners\fake_whisper.obj'
Copy-Item .scratch/fake-runners/allocation.exe .scratch/fake-runners/crash.exe
Copy-Item .scratch/fake-runners/allocation.exe .scratch/fake-runners/timeout.exe
.\scripts\dev-shell.ps1 cmd /c 'cargo test --lib gpu_failures_retry_once_and_remain_on_cpu -- --ignored --nocapture'
.\scripts\dev-shell.ps1 cmd /c 'cargo test --lib https_background_download -- --ignored --nocapture'
```

The failure integration test additionally expects `.scratch/fake-runners/{allocation,crash,timeout}.exe` and `.scratch/youtube-id-six.wav`; see its source for simulated outputs and timeout override. Benchmark qualification files are stored in the benchmark's own data directory and do not change your normal app preferences. Application qualifications are local to its configured data directory.
