# Indonesian live transcription — dev verification

Verified on 2026-10-08 on this Windows machine: Ryzen 7 4800H, 8 cores / 16 logical processors, Windows build 26300. Installer packages were not rebuilt.

## Capture cause and repair

The original 107-second recording had no detected audio and never invoked Whisper. Reproducing playback from https://www.youtube.com/watch?v=pEz-9pJG6pI showed that the default Realtek endpoint delivered packets containing zeros while Windows speakers were muted. Unmuting produced a signal around −14.5 dBFS. Browser/player audio was unmuted and routed to the default speaker. The missing transcript was a capture problem, rather than missing Indonesian model assets.

“Windows default” now uses application loopback, excluding Depth and its child processes. This records desktop playback before speaker mute and across output endpoints. The supplied video produced nonzero audio with speakers both muted and unmuted. The temporary mute test restored the previous mute setting without changing volume.

Selecting a specific output retains endpoint loopback. Its mute/volume can suppress capture. Default-device changes reconnect the stream, retaining the recording clock. Only one active output endpoint was available, so a physical device switch was not exercised on this machine. Missing-device failure was checked directly and returned immediately.

Windows application loopback requires build 20348 or later. Older systems fall back to endpoint capture with a visible notice to keep speakers unmuted. See Microsoft's [application loopback sample](https://learn.microsoft.com/en-us/samples/microsoft/windows-classic-samples/applicationloopbackaudio-sample/).

## Live drafts and measured performance

Drafts use multilingual `ggml-base-q5_1.bin`; finals retain `ggml-small-q5_1.bin` and its existing beam-search decoding. The base checkpoint was downloaded into `models/`. Both engines run locally, with separate workers and scratch directories. Indonesian snapshots start after two seconds and repeat every two seconds. Final chunks close on silence or at six seconds. Pending preview requests coalesce.

The original four-thread CLI default accumulated four pending final chunks over two minutes. Automatic thread allocation now uses eight threads for final inference and two for drafts on this CPU; explicit `whisper_threads` preferences remain honored.

| Measurement | Result |
| --- | --- |
| First draft after warm-up, simultaneous engine benchmark | 4.139 seconds from speech start |
| Continuous two-minute benchmark | 20 final chunks; pending final queue 0–1, ending at 0 |
| Final inference in that benchmark | Mean 4.991 seconds; range 3.580–7.855 seconds |
| Benchmark including final drain | 125.612 seconds for 120 seconds of input |
| First draft in the final real capture run | 4.317 seconds from Start |
| Final real capture run | 21 final segments; no overflow; temporary backlog cleared |
| Stop to completion in that run | About 4.3 seconds |

The benchmark replays captured audio at real-time pace; it is a throughput measurement, not a recognition-accuracy score. YouTube inserted English/music advertisements during browser testing. The actual Indonesian speech also generated revisable drafts and final results. Individual drafts can take longer under contention; obsolete pending requests are dropped rather than queued indefinitely.

The real capture test exposed a short-packet-gap bug that discarded an open utterance and left its draft behind. Short gaps now insert silence while retaining the utterance and chunk identity. A regression test verifies that both sides of the gap reach the final result.

## Verification and delivery

- 70 library tests passed, including stale revisions, recording identity, final retirement, continuous timestamps, short packet gaps, queue pressure, missing preview models, child-output streaming without newlines, split UTF-8, engine failure, and Stop during inference.
- The final complete `cargo test` run passed 84 tests: 74 library tests, one executable test, and nine filesystem tests. This includes the four caption-button checks added and fixed concurrently in the separate “Fix top-right action button” task.
- Real pipeline assertions verified ordered nonoverlapping final chunks, unique sequences, completed Stop drain, empty drafts at completion, and saved Markdown matching final recording text.
- English desktop recording produced a completed transcript through the existing Whistle sidecar.
- Slint tests preserved selection and reader scroll during draft/final updates, recording switches, and timed view. Rendered GUI snapshots were reviewed at 1180×780 and 800×600.
- Drafts remain outside recording events, saved Markdown, copy, and export. Capture failure and five-second silence guidance are visible through transient GUI state.

Reproducible tools: `examples/capture_probe.rs`, `examples/live_benchmark.rs`, `examples/pipeline_probe.rs`, and `examples/gui_snapshot.rs`. Raw measurements and rendered snapshots are in `.scratch/`.

Run `scripts/dev-ui.ps1` for the updated app. It builds a separate `depth-live.exe`, uses `.scratch/dev`, and retains the existing final-model and language preferences. The old dev instance was replaced after confirming its recording was completed. The new GUI is running with Ctrl+Alt+Space registered; the supplied YouTube tab is paused for playback testing.
