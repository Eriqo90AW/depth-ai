# Speaker verification

Speaker detection is opt-in and starts disabled. The default build includes official `sherpa-onnx` 1.13.8 Rust bindings and its matching Windows x64 shared CPU runtime. The models are Pyannote INT8 segmentation and NeMo TitaNet small. No external inference API is used. Downloads occur in the developer asset helper, before packaging.

## Automated checks

Run from the workspace root:

```powershell
.\scripts\dev-shell.ps1 cargo test -j 1
.\scripts\dev-shell.ps1 cargo run --example speaker_snapshot
.\scripts\dev-shell.ps1 cargo run --example speaker_validation -- testdata\jfk.wav .scratch\youtube-original-id-muted.wav
```

Regression coverage includes mixed speakers within one ASR segment; first-appearance IDs independent of window cluster numbers; returning voices; conservative matching of similar profiles; ambiguous/overlapping/untimed speech; bounded overlapping PCM windows and stop tails; queue pressure; injected backend errors; missing models while ASR is delayed; stale recording events; label completion; Unicode/subword alignment; rename/merge/manual reassignment; selection and Find updates; autosave off; old recordings; restart/recovery; and continuation with merged exemplars and manual overrides. All export formats resolve the same speaker registry.

GUI snapshots at 1180×780 and 800×600, plus the Speakers panel at 620×680, are rendered using Slint's software backend. They use fixture text rather than a captured recording.

## Windows throughput gate

```powershell
.\scripts\dev-shell.ps1 cargo build --example speaker_benchmark
.\target\debug\examples\speaker_benchmark.exe en testdata\jfk.wav 20
.\target\debug\examples\speaker_benchmark.exe id .scratch\youtube-original-id-muted.wav 20
```

The benchmark paces 16 kHz mono PCM in real time, submits six-second final ASR chunks, and runs Indonesian live-draft ASR alongside detection. It exits unsuccessfully if speaker inference reaches five seconds per window, any speaker windows are dropped, or final ASR supplies no usable timings. Run both languages on the intended Windows hardware before enabling detection in a release. Models are loaded before measurement.

A 100 ms process sampler measures the benchmark and its direct ASR child processes. CPU includes retained handles to completed children; peak memory is the sampled sum of working sets, not private allocations. Results are hardware-specific. Window-label delay measures publication after the analyzed window's end; first-label time includes the initial ten-second audio collection. GUI refresh can add its polling interval.

## Measured results

Windows x64, AMD Ryzen 7 4800H (8 cores / 16 logical processors), debug build, 20 seconds of paced audio per language:

| Language / final engine | Max speaker window | Mean speaker window | First label | Max label delay after window | Combined CPU seconds | Average cores | Peak combined working set | Usable timed tokens |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| English / Whistle | 0.220 s | 0.164 s | 10.091 s | 0.218 s | 17.406 | 0.850 | 244.1 MiB | 37 |
| Indonesian / Whisper small, with base drafts | 0.186 s | 0.168 s | 10.068 s | 0.162 s | 153.641 | 5.715 | 870.4 MiB | 58 |

Both runs processed three overlapping windows without dropping any, preserved one speaker ID, and passed the five-second gate. Indonesian final ASR took 3.26–4.00 seconds per chunk; draining final and draft ASR after the twenty-second input brought total wall time to 26.88 seconds. Speaker inference continued independently. Logs: `target/speaker-benchmark-en.log` and `target/speaker-benchmark-id.log`.

## Accuracy limits and assets

Automated tests exercise identity matching and ambiguity handling with controlled embeddings. Throughput tests do not establish a diarization error rate. The English/Indonesian throughput clips test single-speaker stability. A separate real-model A/B/A fixture distinguished two voices and reused the first ID when that voice returned; one ambiguous interval remained Unknown. Silence and a synthetic three-note chord produced no turns/profiles. Real meetings and annotated audio are still needed to measure accuracy for short replies, similar voices, music, and overlap. Detection never guarantees an assignment for these cases. The panel supplies manual correction.

Developer test audio and snapshots live under `target/`; user recording audio is not added to persisted speaker metadata. Voice profiles are recording-specific, and names/corrections are journal events. Continued sessions reuse the same IDs; global speaker recognition is outside this feature. If a stopped journal has a damaged tail, the next speaker correction preserves the original bytes in a local `.recovery-backup`, atomically saves the intact events plus the correction, and keeps the recording marked Incomplete. Deleting that recording removes its backup as well.

The asset helper bundles license files under `vendor/speakers/licenses`: Pyannote MIT, NeMo Apache-2.0, Sherpa-ONNX Apache-2.0, and ONNX Runtime MIT, plus a notice identifying the shipped assets. See the [official Sherpa model guide](https://k2-fsa.github.io/sherpa/onnx/speaker-diarization/models.html), [official Rust APIs](https://docs.rs/sherpa-onnx/1.13.8/sherpa_onnx/struct.OfflineSpeakerDiarization.html), and [NVIDIA TitaNet small model metadata](https://api.ngc.nvidia.com/v2/models/nvidia/nemo/titanet_small).
