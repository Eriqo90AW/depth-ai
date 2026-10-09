<div align="center">
  <img src="assets/icon/png/depth-256.png" alt="Depth logo" width="96">
  <h1>Depth</h1>
  <p><strong>Turn desktop audio into notes you can keep.</strong></p>
  <p>v0.1.1 &middot; Windows x64 &middot; English &amp; Indonesian &middot; On-device transcription</p>
  <p>
    <a href="#get-started">Get started</a> &middot;
    <a href="#using-depth">Using Depth</a> &middot;
    <a href="#settings">Settings</a> &middot;
    <a href="#developer-guide">Developer guide</a>
  </p>
</div>

Depth captures the audio your PC plays and turns it into a searchable recordings library. Follow a lecture, revisit a meeting, or collect notes from a video, then copy the text or export a document. Speech recognition runs locally, with no account or cloud upload.

<p align="center">
  <img src="docs/images/live-transcription.png" alt="Depth's dark desktop interface showing an Indonesian transcript, a revisable live draft, and recording controls" width="1180">
  <br>
  <sub>Desktop interface with sample Indonesian text. Final results and live drafts appear separately.</sub>
</p>

## What you can do

| Feature | How it helps |
| --- | --- |
| English and Indonesian | English uses Whistle; Indonesian uses Whisper with separate live-draft and final-transcription workers. |
| Live Indonesian drafts | Read revisable text while speech continues. Only final results enter saved transcripts, copy, and export. |
| Speaker labels | Opt into offline voice grouping, then name speakers, merge duplicate groups, or correct individual turns. |
| A recordings library | Rename sessions, search their text, switch between paragraph and timed views, and delete recordings with confirmation. |
| Continue a recording | Append more audio to a completed session. Timestamps continue from its recorded duration, excluding time spent stopped. |
| Edit finished transcripts | Correct passages in the app, save or discard changes, and retain timestamps and speaker labels. |
| Copy and export | Copy selected text or an entire recording, include timestamps, and export Markdown or plain text. |
| Controls wherever you work | Start or stop with `Ctrl+Alt+Space`, the tray menu, or a floating overlay with elapsed time and an Open app button. |
| Local storage and recovery | Autosave readable Markdown and recovery journals, or keep recordings in memory until you export. |
| A desktop that fits | Use System, Light, or Dark appearance, reduced motion/transparency, and a compact layout down to 800 × 600. |

Depth captures **desktop playback**. Microphone recording is not supported. It starts idle by default; recording begins when you choose Start recording or use the hotkey.

With **Windows default** selected as the audio source, supported Windows builds capture application playback before speaker mute and across output devices. On Windows builds older than 20348, Depth falls back to endpoint capture and shows a notice to keep speakers unmuted. Selecting a specific output device also uses endpoint capture, so that device's mute or volume can suppress the signal.

## Get started

This README describes the current **0.1.1 source version**. Build and package from the same checkout to include the latest changes.

### Windows installer

Check [GitHub Releases](https://github.com/Eriqo90AW/depth-ai/releases) for a packaged `depth-setup.exe`. If you already have a setup executable, run it and choose your shortcut and startup options. The NSIS installer requests administrator permission and installs into Program Files for all users. An unsigned build may show a Windows SmartScreen prompt.

Installers, downloaded models, and engine binaries are excluded from this repository. The installer scripts bundle Whistle for English, Maleo Whisper small-id Q8_0 for final Indonesian transcription, Whisper base Q5_1 for live drafts, and local speaker models. Matching CPU and NVIDIA CUDA whisper.cpp runners and their licenses are included. A clean installation can transcribe offline without NVIDIA hardware. Optional checkpoints download separately into each user's data folder.

### Build from source

You need Windows 10 or 11 x64, PowerShell, Python 3, and Visual Studio Build Tools with the **Desktop development with C++** workload. Include the recommended CMake tools if you plan to use the optional in-process Whisper engine. The helper scripts use a Rust toolchain stored in `.toolchain/` inside this checkout.

1. Clone the repository.

   ```powershell
   git clone https://github.com/Eriqo90AW/depth-ai.git
   cd depth-ai
   ```

2. Set up the build tools and workspace-local Rust toolchain.

   <details>
   <summary>First-time build setup</summary>

   Install Visual Studio Build Tools from an administrator PowerShell, or add the C++ workload to an existing installation:

   ```powershell
   winget install --id Microsoft.VisualStudio.2022.BuildTools `
     --accept-package-agreements --accept-source-agreements `
     --override "--quiet --wait --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
   ```

   From the repository root, install Rust into the directories expected by `scripts/dev-shell.ps1`:

   ```powershell
   $env:CARGO_HOME = Join-Path $PWD ".toolchain\cargo"
   $env:RUSTUP_HOME = Join-Path $PWD ".toolchain\rustup"
   Invoke-WebRequest https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe `
     -OutFile rustup-init.exe
   .\rustup-init.exe -y --profile minimal --default-toolchain stable --no-modify-path
   ```

   </details>

3. Download the engines and models, including the Indonesian draft checkpoint.

   ```powershell
   python scripts/fetch_assets.py all
   ```

   This downloads Whistle, the pinned CPU/CUDA runners, Maleo small-id, base drafts, and speaker models/runtime/licenses. Building the offline installer requires these assets. Optional final models can be downloaded later from Settings; transcription stays local.

4. Build, check the assets, and launch Depth.

   ```powershell
   .\scripts\dev-shell.ps1 -Command @('cargo', 'build', '--release', '--locked')
   .\target\release\depth.exe --check
   .\target\release\depth.exe
   ```

   The build helper imports Visual Studio's linker environment. `--check` reports the configuration, detected NVIDIA device, runtime availability, requested/effective model, and missing model downloads before you start recording.

## Using Depth

1. Open Depth and choose **English** or **Indonesian** from the language selector.
2. Play the audio you want to transcribe and click **Start recording**, or press **Ctrl+Alt+Space**.
3. Read final text as it arrives. Indonesian also shows a separate **Live draft** that can change as more speech is processed.
4. Click **Stop recording**. Depth shows **Finishing** while it processes queued final chunks, then marks the recording complete.
5. Rename the recording by editing its title and pressing Enter. After processing finishes, choose **Edit transcript** to correct passages, then **Save changes**. Saved edits update Find, Timed view, copy, and export. **Cancel** lets you keep editing or discard unsaved changes.
6. Use Find, Timed view, Copy text, Copy timestamps, or Export to work with the result.
6. Select a completed recording and choose **Continue recording** to append to it, or **New recording** for a separate session.

While you read earlier text, incoming results preserve your selection and scroll position. **Jump to latest** returns to the end. Closing the window keeps Depth in the tray; use **Quit Depth** or the tray's Quit action to exit.

<p align="center">
  <img src="docs/images/compact-recording.png" alt="Depth at 800 by 600 pixels, showing a transcript and the New recording and Continue recording buttons" width="800">
  <br>
  <sub>Compact layout with sample text. The Library button opens the recordings sidebar.</sub>
</p>

New recordings save to `Documents\Depth\transcripts` by default. With autosave off, recordings remain in memory until you export them, and quitting prompts you to review unsaved work. Deleting a saved recording removes its transcript; deleting an imported legacy session removes it from the library while keeping its shared source file.

Corrections to autosaved recordings survive restarts. With autosave off, edits stay in memory until you export. Imported legacy sessions store corrections separately without changing their shared source document. Continuing a recording retains saved corrections. Edited passages keep their original timing anchors; manually entered words do not have individual ASR timings.

### Speaker labels and names

Enable **Settings → Detect speakers** before starting a new recording. It is off by default. Speech recognition and speaker detection run on your computer; they use no external inference API. Speaker models load only for recordings with detection enabled.

Text appears immediately with **Identifying speaker** while analysis catches up. Detection analyzes overlapping ten-second windows every five seconds, so the first labels need about ten seconds of captured audio. Voices get recording-specific labels such as **Speaker 1**. Uncertain speech, missing timing evidence, and overlapping voices retain **Unknown speaker**.

After Stop finishes processing, choose **Speakers** to rename a voice, merge a duplicate group into a chosen speaker, or choose a transcript turn and assign its speaker. These changes update paragraph/timed views, Find, copy, Markdown, and plain-text exports. Corrections take precedence over automatic updates. Continuing the same recording reuses its voice profiles, names, and corrections; unrelated recordings start with fresh identities. Old recordings keep their existing appearance.

With autosave enabled, names and voice embeddings stay in the recording's local recovery journal. Deleting that recording removes its journal, any recovery backup, and saved Markdown. With autosave off, this metadata stays in memory until the app closes. Text exports contain resolved names, without voice embeddings. The speaker worker holds bounded audio windows in memory and does not save recording audio.

For a source checkout missing these assets, run `python scripts/fetch_assets.py speakers`. Missing models or detection failures show a notice and preserve the transcription. See [speaker verification](docs/speaker-verification.md) for tests, Windows throughput measurements, and accuracy limits.

## Settings

Open **Settings** for language, desktop audio source, speaker detection, autosave, recording on launch, the global hotkey, appearance, floating controls, and completion notifications. Appearance changes apply immediately. Capture, engine, and autosave changes apply to the next recording. Engine and capture tuning are available under **Advanced**.

Preferences are stored in `Documents\Depth\config.toml`. These defaults match the current source:

| Key | Default | Purpose |
| --- | --- | --- |
| `language` | `"en"` | `"en"` for English, `"id"` for Indonesian. |
| `start_listening` | `false` | Start idle until recording is requested. |
| `save_transcript` | `true` | Save recordings automatically. |
| `hotkey` | `"Ctrl+Alt+Space"` | Global start/stop shortcut. |
| `viewer_theme` | `"system"` | Follow Windows appearance; also accepts `"light"` or `"dark"`. |
| `show_indicator` | `true` | Show the floating recording controls. |
| `show_result_popup` | `true` | Notify when transcription finishes. |
| `indicator_position` | `"bottom-right"` | Overlay corner; also accepts `"bottom-left"`, `"top-right"`, or `"top-left"`. |
| `whistle_model` | `"whistle.cact"` | English speech model. |
| `indonesian_processing` | `"auto"` | Try NVIDIA CUDA, with CPU fallback; also accepts `"nvidia"` and `"cpu"`. |
| `indonesian_model` | `"recommended"` | Device recommendation, a catalog ID, or `"custom"`. |
| `whisper_model` | `"ggml-small-q5_1.bin"` | Preserved legacy/custom checkpoint path, used with Custom. |
| `whisper_draft_model` | `"ggml-base-q5_1.bin"` | Indonesian live-draft model. |
| `whisper_threads` | `0` | Allocate threads automatically for draft/final workers; a positive value applies to both. |
| `keywords` | `[]` | Names and phrases to favor during decoding. |
| `reduced_transparency` | `false` | Use a solid backdrop. |
| `reduced_motion` | `false` | Reduce interface animation. |

Omit `audio_output_device` to use **Windows default**. Selecting a specific device stores its Windows endpoint ID.

<details>
<summary>Advanced capture and engine defaults</summary>

| Key | Default | Purpose |
| --- | --- | --- |
| `vad_threshold_db` | `-45.0` | Energy threshold for opening an utterance. |
| `silence_close_ms` | `1000` | Silence that closes an utterance. |
| `max_segment_secs` | `25.0` | Maximum English chunk length. Indonesian caps this at six seconds. |
| `queue_capacity` | `8` | Final chunks buffered before the oldest is dropped. |
| `word_timestamps` | `false` | Request per-word timings from supported engines. |
| `engine_idle_unload_secs` | `120` | Unload an idle engine after this many seconds; `0` disables unloading. |
| `whistle_decoder_depth` | unset | Optional decoder depth, from 2 through 8, for the Whistle sidecar. |
| `indicator_margin` | `24` | Overlay distance from the usable screen corner, in logical pixels. |
| `indicator_opacity` | `225` | Overlay opacity, from 0 through 255. |

</details>

### Choosing Indonesian models

Choose **Settings → Speech recognition** to select Auto, NVIDIA GPU, or CPU and a final model. The detected GPU and the device actually processing the recording appear there. Drafts always use base on CPU; English stays on Whistle.

**Recommended for this device** starts with bundled Maleo small-id. Its output is lowercase without punctuation. On an NVIDIA GPU with at least 4 GiB VRAM, **Download recommended model** offers stock large-v3-turbo Q5_0. Downloads are opt-in. Select **Use downloaded model**, then **Save settings** to apply an explicit choice to the next recording. Downloading while recording does not switch the current model.

Turbo becomes eligible for Recommended only after a recording processes at least 30 seconds on CUDA, averages faster than incoming audio including per-chunk loading, and has no dropped final chunks. This RTX 2060's tested runtime did not pass; small-id remains its recommendation. See [GPU/model verification](docs/gpu-model-verification.md) for measurements and remaining validation limits.

| Catalog ID | Checkpoint | Download size | Output/use |
| --- | --- | --- | --- |
| `base` | Stock base Q5_1 | 57 MiB | Bundled CPU drafts; lighter final option. |
| `small` | Stock small Q5_1 | 181 MiB | Multilingual final results with punctuation. |
| `medium` | Stock medium Q5_0 | 515 MiB | Larger multilingual final model. |
| `turbo` | Stock large-v3-turbo Q5_0 | 547 MiB | GPU upgrade candidate; device qualification required for Recommended. |
| `large-v3` | Stock large-v3 Q5_0 | 1.01 GiB | Largest catalog option; heavier processing. |
| `small-id` | Maleo Indonesian small Q8_0 | 252 MiB | Bundled final starter; lowercase, no punctuation. |
| `medium-id` | Cahya Indonesian medium Q5_0 | 515 MiB | Indonesian fine-tune; heavier than the starter. |

Sizes describe checkpoint downloads, not inference memory. The shared [catalog](assets/asr-models.json) pins revisions, SHA-256 hashes, sizes, and license/source attribution. Downloads show progress, can be cancelled, and can be retried. Files become installed only after size/checksum verification and atomic publication; interrupted `.part` files are replaced on retry.

If CUDA initialization or inference fails, Depth reaps that runner, discards its partial text, and retries the same audio once with bundled small-id on CPU. One recording notice explains the reason and effective model. Remaining chunks use CPU; the next recording tries your saved preference again. Choose CPU to bypass GPU attempts. Custom executable paths are preserved and run with GPU disabled.

Configurations saved before model selection retain their previous `whisper_model`, draft, and executable paths as **Custom**. Set `whisper_model` in Advanced when using Custom. The processing/model dropdowns own their values; Advanced TOML cannot override them. Downloaded models live in `Documents\Depth\models` (or your `--home` folder), and survive application upgrades/uninstallation.

Indonesian drafts are requested after two seconds of speech and every two seconds afterward. These are scheduling intervals; inference time depends on the model and CPU. Final chunks close on silence or at six seconds. A missing draft checkpoint shows a notice while final transcription continues.

## Your files

The default data directory is `Documents\Depth`:

```text
Depth/
├── config.toml                  Preferences
├── depth.log                    Diagnostics
├── models/                      Verified optional ASR downloads
├── turbo-qualified.json         Device-specific turbo speed check
├── scratch/                     Engine working files, including audio chunks
└── transcripts/
    ├── <recording-ID>.md         Readable final transcript
    └── .events/
        ├── <recording-ID>.jsonl  Recovery journal
        ├── titles.json          Legacy title overrides
        └── deleted.json         Removed legacy sessions
```

Each recording has a stable ID. Midnight does not split a session. Journal events are flushed as results arrive, and Markdown is updated through atomic replacement. Interrupted recordings recover intact journal events and appear as incomplete. Live drafts stay out of the saved document and journal.

Use `--home <DIR>` or the `DEPTH_HOME` environment variable to choose another data directory. The older `TRANSCRIBE_AI_HOME` variable remains supported. Autosave controls transcript storage; engines can still write scratch files and diagnostics while processing audio.

## How it works

```mermaid
flowchart LR
    A[Desktop playback] --> B[Windows loopback capture]
    B --> C[16 kHz mono and speech segmentation]
    C --> D[Whistle: English]
    C --> E[Selected Whisper: Indonesian CPU/CUDA]
    C --> G[Whisper base: Indonesian drafts]
    D --> F[Final transcript]
    E --> F
    G --> H[Revisable live preview]
    F --> I[Recording library and export]
```

Only the selected language's final engine runs. Indonesian adds a separate draft worker. Capture, transcription, and the interface run independently; Stop drains pending final work before the next recording starts. The default build runs prebuilt engine executables, so it does not compile whisper.cpp or link the Cactus engine in-process.

## Developer guide

### Checks and diagnostics

```powershell
.\scripts\dev-shell.ps1 -Command @('cargo', 'test', '--locked')
.\scripts\dev-shell.ps1 -Command @('cargo', 'fmt', '--check')
.\target\release\depth.exe --check

# Play desktop audio while capturing this sample.
New-Item -ItemType Directory -Force .scratch | Out-Null
.\target\release\depth.exe --record 10 .scratch\capture.wav
.\target\release\depth.exe --lang id --once .scratch\capture.wav

# Run without the desktop interface or tray.
.\target\release\depth.exe --headless
```

`--once` accepts a 16 kHz mono WAV. The diagnostic examples cover [desktop capture](examples/capture_probe.rs), [engine throughput](examples/live_benchmark.rs), [the recording pipeline](examples/pipeline_probe.rs), and [GUI snapshots](examples/gui_snapshot.rs). The screenshots in this README are rendered from the app's Slint components with sample text, rather than a live recording.

### Develop the interface

```powershell
.\scripts\dev-ui.ps1                    # Isolated app home and Slint live updates
.\scripts\dev-ui.ps1 -Preview long      # Sample document with 2,501 utterances
.\scripts\dev-ui.ps1 -Preview empty
.\scripts\dev-ui.ps1 -Preview recording
.\scripts\dev-ui.ps1 -Preview settings
.\scripts\dev-ui.ps1 -Preview error
.\scripts\dev-ui.ps1 -Software          # Software renderer
```

The helper uses `.scratch/dev` and a separate executable. Sample preview modes do not capture audio or register a hotkey. Close other Depth instances before testing real capture so hotkeys do not compete. Compatible layout edits update live; Rust changes and shared property/callback changes require restarting the app. Use an ordinary release build for packaging and responsiveness checks.

### Build options

| Cargo feature | Default | Purpose |
| --- | --- | --- |
| `whistle-sidecar` | On | English via the prebuilt Cactus `needle.exe`. |
| `whisper-sidecar` | On | Indonesian via the prebuilt `whisper-cli.exe`. |
| `speakers` | On | Official Sherpa-ONNX CPU runtime; detection remains opt-in in Settings. |
| `tray` | On | Slint interface, tray, hotkey, and shell integration. |
| `whistle` | Off | In-process Cactus engine; requires a compatible libc++ toolchain. |
| `whisper` | Off | In-process Whisper through `whisper-rs`; requires CMake and C++ build tools. |

For an English-only desktop build:

```powershell
.\scripts\dev-shell.ps1 -Command @('cargo', 'build', '--release', '--locked', '--no-default-features', '--features', 'whistle-sidecar,tray')
```

### Packaging and artwork

The supported installer script is [installer/depth.nsi](installer/depth.nsi). With NSIS available in the workspace-local tool directory, build from the repository root:

```powershell
.\scripts\dev-shell.ps1 -Command @('cargo', 'build', '--release', '--locked')
New-Item -ItemType Directory -Force dist | Out-Null
Push-Location installer
try { ..\.toolchain\nsis\makensis.exe depth.nsi } finally { Pop-Location }
```

This produces `dist\depth-setup.exe` with Whistle, Whisper small, Whisper base, Pyannote segmentation, NeMo TitaNet small, and the matching Sherpa-ONNX runtime/license files. [installer/depth.iss](installer/depth.iss) is retained as an alternative/reference script; both scripts use the same output filename.

The icon assets are checked in. To regenerate them, install Pillow for Python and run `python scripts/make_icon.py`. The Rust build embeds the icon and version directly through [build/winres.rs](build/winres.rs).

## Troubleshooting

| Problem | What to check |
| --- | --- |
| `link.exe` not found | Install the Visual Studio C++ workload and build through `scripts/dev-shell.ps1`. |
| Rust toolchain missing | Run the workspace-local Rust setup above; the helper uses `.toolchain/cargo` and `.toolchain/rustup`. |
| Missing Whistle or Whisper assets | Run `python scripts/fetch_assets.py all --size small`, then `depth.exe --check`. |
| Speaker labels show Unknown or a model notice | Install `python scripts/fetch_assets.py speakers`; transcription remains available. Very short, similar, or overlapping voices may need manual correction. |
| Indonesian finals work, but live drafts are unavailable | Download `python scripts/fetch_assets.py model --size base` and check `whisper_draft_model`. |
| No text appears | Check playback/player mute and routing, then select Windows default. Older Windows or a specific-device source may require unmuted speakers. The status bar reports the active source and silence guidance. |
| The hotkey does nothing | Check the registered hotkey shown in Settings and the log for a conflict with another app. |
| Music produces unwanted segments | Try a less sensitive `vad_threshold_db`, such as `-35.0`, in Advanced settings. |
| Stop shows Finishing | Final chunks are still being transcribed. Let them drain before starting another recording. |
| Development UI switches recordings slowly | Compare a compiled release build; debug and Slint live-preview builds have additional layout/rendering cost. |
| Build or installer cannot write temporary files | Point `TEMP` and `TMP` at a writable directory and retry. |
| In-process engine fails to link or build | Use the default sidecar features; optional in-process engines need additional native toolchain support. |

## Credits and licensing

The application's package metadata declares **Apache-2.0**. Speech models and engine distributions have their own license terms.

- Whistle and the needle engine are by [Cactus Compute](https://github.com/cactus-compute/needle). The asset helper downloads [Whistle](https://huggingface.co/Cactus-Compute/whistle) and [needle3](https://huggingface.co/Cactus-Compute/needle3).
- Indonesian checkpoints and the command line runner come from [whisper.cpp](https://github.com/ggml-org/whisper.cpp).
- The desktop interface is built with [Slint](https://github.com/slint-ui/slint).
