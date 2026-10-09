# Teams output fallback verification

Automatic detection keeps default capture running. After five seconds without a sample above -60 dBFS, it probes active render endpoints without saving or forwarding their audio. Two consecutive one-second windows must exceed -60 dBFS while default capture remains silent. Attempts start at least ten seconds apart. Candidates are ranked by communications default, regular default, sustained qualifying level, and endpoint ID.

The verified WASAPI stream becomes the only transcription input. Probe queues and the old resampler tail are discarded. The QPC recording origin survives the switch, and emitted timestamps cannot move backward. The output stays selected until recording ends or the endpoint fails. Explicit source preferences and Windows routing, volume, and mute are never changed.

## Automated checks

Run through the workspace MSVC wrapper, with TEMP and TMP set to a writable directory:

```powershell
.\scripts\dev-shell.ps1 cargo test -j 1 --lib --tests --bins
```

The capture tests cover the five-second silence delay, the ten-second retry limit, true silence, isolated transients, endpoint ranking, failed reads, stream retention and cleanup, disconnect timing, Stop cancellation, discarded resampler tails, increasing timestamps, and non-overlapping segmenter results. Live-status tests check endpoint identity, fallback reason, the Realtek message, unmuted guidance, recovery, and explicit source labels. Existing pipeline tests cover Stop draining and recording continuation.

On October 9, 2026, the default-feature library, binary, and integration suite passed all 139 tests. The headless suite passed 127 tests. Formatting and diff whitespace checks passed.

Formatting:

```powershell
.\toolchain\cargo\bin\cargo.exe fmt --all -- --check
```

## Live checks on October 9, 2026

Teams desktop had active playback sessions on Speaker (Realtek(R) Audio). Speakers were already unmuted. No diagnostic changed volume, mute, or routing.

- A nine-second capture probe measured default capture near -90.3 dBFS. It switched to Realtek after about seven seconds and measured output audio near -35.7 dBFS. No probe WAV was saved.
- The rebuilt Indonesian pipeline probe ran for forty seconds. It switched after qualifying output activity, logged speech detection and submitted segments, and produced two recognizable Indonesian final results. It completed after Stop with increasing, non-overlapping segment times and no duplicate chunk sequence or saved drafts.
- An eighteen-second earlier run rejected its first candidate because the second window measured -60.2 dBFS, then switched after the retry qualified. That recording ended immediately after handover and produced no final text.
- Explicit Realtek capture received audio immediately near -34 dBFS, with no fallback reason.
- A six-second default capture was stopped during endpoint verification. It exited without switching to output capture or saving probe audio.

The live diagnostic recordings are in the ignored `.scratch/live-verification` directory. The existing Indonesian models and speech thresholds were used. Recognition still includes errors; this fix restores capture and does not tune ASR.

## Remaining manual coverage

A controlled browser playback check, a physical endpoint disconnect/reconnect, and a visual muted-output check were not performed during the active Teams playback. Disconnect recovery, silence behavior, and unmuted guidance have deterministic coverage. Check these with the default source, confirm the output message after fallback, and confirm Continue starts a fresh detection session without changing the saved source preference.
