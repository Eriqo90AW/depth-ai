# Depth

<img src="assets/icon/png/depth-256.png" alt="Depth" width="96" align="right">

A Windows desktop app that transcribes desktop audio into a recordings library and readable
documents. Everything runs on-device: no cloud, no account, no
network at runtime.

## Quick start

1. Run `dist\depth-setup.exe` — about 215 MB: the app, the Cactus engine with the Whistle
    model, and Whisper small for Indonesian. Windows SmartScreen will warn (the installer is
   unsigned): **More info → Run anyway**. The wizard runs as administrator and installs
   machine-wide; leave *Create a desktop shortcut* ticked.
2. Launch **Depth** from the desktop shortcut. The main window opens with a recordings library.
3. Play the audio you want transcribed, then click **Start recording** or press **Ctrl+Alt+Space**.
4. Click **Stop recording**. Depth flushes capture and finishes queued transcription before another
   recording can start. The completed document stays in the library.
5. Select a finished recording and choose **Continue recording** to append more audio to the
   same transcript. Its timestamps continue from the previous duration, excluding time spent
   stopped. Choose **New recording** for a separate session.
6. Select text and use Ctrl+C, or use **Copy text**, **Copy timestamps**, and **Export**.
   New recordings have one Markdown file each in `Documents\Depth\transcripts`.
7. To remove a finished recording, choose **Delete** beside its title, then confirm. Imported
   sessions are removed from the library while their shared source file stays on disk.
8. Closing the main window keeps Depth in the tray. Choose **Open Depth** to return or **Quit** to exit.

> **It listens to what your PC plays, not to your microphone.** Nothing is recorded while it
> is idle, and nothing ever leaves your machine.

Common follow-ups:

| I want to… | Do this |
| --- | --- |
| Use a different key | Open Settings and change the global hotkey. The registered combination is shown there. |
| Transcribe Indonesian | Settings → Recording → Indonesian. Applies to the next recording. |
| Check everything is wired up | `depth.exe --check` |
| Stop it starting with Windows | Uninstall, or untick that task; the shortcut lives in `shell:startup` |
| Remove it | Settings → Apps, or *Uninstall Depth* in the Start Menu |

- **English** is transcribed by [Whistle](https://cactuscompute.com/blog/whistle) — a 16.9 MB
  speech model from Cactus Compute, run through their C engine.
- **Indonesian** is transcribed by a Whisper checkpoint, because Whistle covers English, German,
  French, Spanish, Italian, Dutch and Polish but not Indonesian.

Only the engine for the selected language is loaded, so an English session costs about 17 MB of
model rather than 180 MB.

## How it works

```
default output device
        │  WASAPI loopback (shared mode, event driven)
        ▼
  downmix to mono → resample to 16 kHz          src/capture.rs
        │
        ▼
  energy gate + utterance segmenter             src/vad.rs
        │  closes on 1.0 s of silence, hard-splits at 25 s
        ▼
  bounded queue (reports overflow when behind)      src/pipeline.rs
        │
        ▼
  Whistle (en)  |  Whisper (id)                 src/engine/
        │
        ▼
  transcripts/<start-ID>.md + .events/*.jsonl    src/recording.rs
```

Two threads do the work: a capture thread that also runs the cheap energy gate, and an engine
worker that owns the model. When nothing is playing, the gate sees digital silence, no utterance
is ever opened, and no inference runs at all.

Desktop loopback capture records the machine's own output, so no virtual audio cable or driver
is required.

## Requirements

- Windows 10 or 11, x64
- **Visual Studio Build Tools** with the C++ workload, plus CMake. `libneedle.a` is an MSVC-format
  C++ static library and `whisper-rs` compiles whisper.cpp, so a C++ toolchain is unavoidable:

  ```powershell
  # Run in an elevated PowerShell (Administrator)
  winget install --id Microsoft.VisualStudio.2022.BuildTools --accept-package-agreements `
    --accept-source-agreements --override "--quiet --wait --nocache `
    --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
  ```

  `--includeRecommended` brings in CMake and the Windows SDK along with the compiler.

- Rust. If you do not have it, a workspace-local toolchain is enough:

  ```powershell
  # Installs into .toolchain\ so nothing outside this folder is touched
  $env:CARGO_HOME = "$PWD\.toolchain\cargo"; $env:RUSTUP_HOME = "$PWD\.toolchain\rustup"
  Invoke-WebRequest https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe `
    -OutFile rustup-init.exe
  .\rustup-init.exe -y --profile minimal --default-toolchain stable --no-modify-path
  ```

- Python 3 only to fetch the models (the app itself never uses Python):

  ```powershell
  python scripts/fetch_assets.py whistle                     # 16.9 MB model + 3.9 MB engine
  python scripts/fetch_assets.py whisper --size small        # 181 MB, Indonesian path only
  ```

## Build and run

MSVC's `link.exe` is not on `PATH` by default, so build through the helper script, which imports
the Visual Studio environment first:

```powershell
.\scripts\dev-shell.ps1                        # cargo build --release
.\scripts\dev-shell.ps1 cargo test             # run the test suite
.\scripts\dev-shell.ps1 cargo run -- --check   # verify config + models, then exit
```

Then run the app:

```powershell
.\target\release\depth.exe             # opens the main window and tray
```

The app sits in the tray. **It idles at launch and only listens once you press the hotkey**, so an
app that is always running is not always transcribing; press the same key again to stop. The tray
icon is the Depth mark: grey while idle, green while listening, red when something went
wrong.

Release builds are GUI-subsystem binaries: starting the app from a shortcut, the Start Menu or the
Startup folder opens **no console window**, and there is no terminal to keep open. The diagnostic
modes below reattach to the terminal they are launched from, so they still print.

### Icon

The icon is generated rather than hand-drawn: `scripts/make_icon.py` (Pillow only) renders the
mark from the geometry constants at the top of that file, so restyling it means editing a few
fractions and re-running.

```powershell
python scripts/make_icon.py             # rewrite assets\icon
python scripts/make_icon.py --all-bmp   # store the 256 px ICO entry uncompressed, too
```

| File | Used by |
| --- | --- |
| `assets\icon\depth.ico` | embedded in `depth.exe`, and read by both installers |
| `assets\icon\depth.svg`, `assets\icon\png\*.png` | this README, docs and anything needing a still image |
| `assets\icon\tray-32.mask` | the tray icon, tinted grey/green/red for the current status |
| `assets\icon\preview.png` | contact sheet of every size on light and dark backgrounds, for review |

`build.rs` turns the `.ico` and the crate's version strings into a Windows resource file by itself
(`build\winres.rs`), so no resource compiler (`rc.exe`, `windres`) and no extra crate is involved:
`cargo build --release` is enough to put the icon on the executable, its shortcuts, its taskbar
button, the Properties → Details tab and the Apps & features entry. `--all-bmp` is only a fallback
for a tool that dislikes PNG-compressed icon entries.

### Installers

One full setup is `dist\depth-setup.exe` (about 215 MB): English on the 16.9 MB Whistle model
plus the 181 MB Whisper checkpoint for Indonesian. It is branded with
`assets\icon\depth.ico` — the wizard, the setup executable, the uninstaller and every
shortcut they create — and names the product **Depth**, while the executable and the
install folder keep the lowercase program name. Each user's transcripts live in their
`Documents\Depth` folder. Both wizards install machine-wide into Program Files:

| Artifact | Built from | Contents |
| --- | --- | --- |
| `dist\depth-setup.exe` | `installer\depth.nsi` (shipped) | app, Cactus `needle` engine, Whistle model, Whisper small for Indonesian |

```powershell
..\.toolchain\nsis\makensis.exe depth.nsi   # from the installer folder; prompts for admin at launch
```

The NSIS build carries a `requireAdministrator` manifest, so Windows shows the UAC prompt as
soon as it starts and everything — including temp extraction — runs elevated. The Inno
equivalent (`installer\depth.iss`, via `.toolchain\innosetup\ISCC.exe`) installs the same
files, but Inno Setup 7 always launches its loader unelevated and only elevates partway
through, so on machines where even temp creation is locked down it can fail before ever
prompting. Prefer the NSIS build; both scripts write the same output name, so building one
overwrites the other. The shipped `dist\depth-setup.exe` is NSIS-built. Do not ship an
Inno build: the bundled Inno 7.1.0 stamps Setup.exe `asInvoker` even with
`PrivilegesRequired=admin` (verified against a minimal script), so its wizard never
elevates at launch — and re-stamping the manifest afterwards with mt.exe truncates the
200 MB payload, destroying the installer. `installer\depth.iss` is kept for reference only.

Uninstalling while the app is still running shows a dialog — "The app is still running. Please
close the app." — with Retry/Cancel until it is closed, so no locked files are left behind.

### Desktop interface

The resizable Slint window has a recordings sidebar, a selectable transcript document, and
persistent Start/Stop controls. Below 1000 logical pixels the sidebar collapses into a Library
button. The window supports sizes down to 800 × 600.

Each Start/Stop run has a stable ID. Recording, finishing, completed and incomplete states appear
in the library. Press Enter in the title field to rename a recording. Legacy daily Markdown
sessions are read through a compatibility adapter; renaming a legacy display title saves separate
metadata and leaves the original file unchanged. Their timed view retains original clock times.

Copy text copies the entire selected recording. Ctrl+C copies the selected text. Timed view,
Copy timestamps, Markdown/plain-text export and previous/next Find matches use that same recording.
While reading earlier text, new results preserve the selection and scroll position. Jump to latest
returns to the end.

Settings includes language, autosave, launch recording, hotkey, Light/Dark/System appearance,
reduced transparency/motion, overlay placement and completion notifications. Technical engine and
capture values are under Advanced. Appearance applies immediately; capture, engine and autosave
changes apply to the next recording. Existing preferences are preserved.

The interactive overlay has elapsed time, Stop and Open app. It appears without taking focus and
uses the selected monitor's scaling and taskbar work area. Windows 11 supports a Mica backdrop;
older Windows versions and reduced-transparency mode use a solid background.

The tray menu has Open Depth, Start/Stop and Quit. Windows startup shortcuts pass `--background`.
Unsaved recordings remain in memory, and quitting offers time to export them.

### Developing the UI

```powershell
.\scripts\dev-ui.ps1                    # isolated config and Slint live updates
.\scripts\dev-ui.ps1 -Preview long      # 2,501 sample utterances, no capture or hotkey
.\scripts\dev-ui.ps1 -Preview empty
.\scripts\dev-ui.ps1 -Preview recording
.\scripts\dev-ui.ps1 -Preview settings
.\scripts\dev-ui.ps1 -Preview error
.\scripts\dev-ui.ps1 -Software          # winit software renderer
```

The helper uses `.scratch/dev`. Close an installed Depth instance before testing capture so the
global hotkeys do not compete. Layout/style edits update a compatible debug build while it runs.
Rust or shared property/callback changes require a restart. Release builds use ordinary
`cargo build --release` without the live-preview feature. Installer packaging is separate.

### Indonesian

Indonesian runs on a Whisper checkpoint through whisper.cpp's **prebuilt** command line runner,
so there is nothing to compile and no native toolchain involved:

```powershell
python scripts/fetch_assets.py whisper                # 8.5 MB prebuilt whisper.cpp CLI
python scripts/fetch_assets.py model --size small     # 181 MB checkpoint
python scripts/fetch_assets.py model --size base      # 57 MB live preview checkpoint
```

Then choose **Language ▸ Indonesian** in the tray. Nothing else to build — the same
`cargo build --release` covers both languages.

Pick the checkpoint by speed versus accuracy:

| `--size` | File | Size | Character |
| --- | --- | --- | --- |
| `tiny` | `ggml-tiny-q5_1.bin` | 31 MB | fastest, roughest |
| `base` | `ggml-base-q5_1.bin` | 57 MB | quick |
| `small` | `ggml-small-q5_1.bin` | 181 MB | the default balance |
| `medium` | `ggml-medium-q5_0.bin` | 539 MB | most accurate, slow on CPU |

Set `whisper_model` in `config.toml` to the final checkpoint. Indonesian live previews use
`whisper_draft_model` (default `ggml-base-q5_1.bin`) on a separate worker. Drafts start after
two seconds of speech and update every two seconds; final chunks close on silence or at six
seconds. “Live draft” text is revisable and stays out of saved recordings, copy, and export.
If the preview checkpoint is missing, final transcription continues with a notice in the GUI.

Settings → Recording → Desktop audio defaults to **Windows default**. On Windows build 20348
and later this captures application playback before speaker mute, across output devices.
Earlier versions fall back to the default output and require speakers to remain unmuted.
Choosing a specific device captures only that endpoint, so its output mute can suppress audio.
The optional `audio_output_device` setting stores the selected Windows endpoint ID; omit it
for the default. The status bar identifies the active source, and five seconds without a
signal shows guidance to check playback and routing. Default-device changes reconnect capture.

With `whisper_threads = 0`, CPU threads are allocated separately for preview and final work.
An explicit positive value continues to apply to both workers. Stop drains final chunks while
the GUI shows “Finishing transcription,” replacing pending drafts as results arrive.

> **Why a sidecar?** `whisper-rs` compiles whisper.cpp in-process, but that build fails here
> (`whisper-rs-sys`'s CMake `install` target aborts with `Build FAILED` and **0 errors**, with both
> VS's CMake 3.31 and a standalone 4.4.4). The prebuilt CLI avoids the whole problem. Accuracy is
> identical because it is the same checkpoint and the same engine — only the process boundary
> differs. The in-process path is still available as the opt-in `whisper` feature for when that
> build works.

> **Not usable for this: Indonesian BERT models.** `cahya/NusaBert-v1.3.1` and friends are
> *fill-mask* text models (masked language modelling over text tokens). They have no audio encoder
> and cannot transcribe speech at all. They are, however, a good fit for *post-processing* a
> transcript — punctuation restoration, casing, or cleaning up names — which is a separate feature
> this project does not have yet.

## Where things are written

Depth defaults to `Documents\Depth`, or the directory passed to `--home`:

```text
config.toml                   # preferences
depth.log                    # diagnostics
transcripts\<start-ID>.md      # one readable document per recording
transcripts\.events\*.jsonl   # private append-and-flush recovery journals
transcripts\.events\titles.json # legacy display-title overrides
```

The ID includes start time, process ID and a sequence counter. Midnight does not split a recording.
Journal events are flushed as results arrive. Markdown is regenerated through atomic replacement
within a short debounce and at completion, with the transcript included once. An interrupted
recording recovers intact journal events and appears as incomplete. Saved contents load when opened;
the library holds metadata. Autosave-off recordings never create transcript files until explicit
export, and their complete text remains in memory.

## Configuration

`config.toml` is created with defaults on first run. The knobs that matter:

| Key | Default | Meaning |
| --- | --- | --- |
| `language` | `"en"` | `"en"` uses Whistle, `"id"` uses Whisper |
| `start_listening` | `false` | `false` idles until the hotkey is pressed |
| `show_indicator` | `true` | The state pill (see below) |
| `indicator_position` | `"bottom-right"` | Which corner it is anchored to: `bottom-right`, `bottom-left`, `top-right` or `top-left` |
| `indicator_margin` | `24` | Gap from that corner of the *usable* screen area, in 96-DPI pixels; the taskbar's band is excluded first |
| `indicator_opacity` | `225` | Indicator opacity, 0-255 |
| `hotkey` | `"Ctrl+Alt+Space"` | Global toggle; falls back automatically if Windows owns it |
| `hotkey_fallback` | `"Ctrl+Alt+Space"` | Used when `hotkey` cannot be registered |
| `whistle_model` | `whistle.cact` | Bare names are searched next to the exe, in the data folder, in `vendor\needle\windows-x86_64` and in a `models\` subfolder |
| `needle_exe` | `needle.exe` | The Cactus runner used by the sidecar build |
| `whisper_model` | `ggml-small-q5_1.bin` | `ggml-base-q5_1.bin` (57 MB) or `ggml-tiny-q5_1.bin` are lighter options |
| `whisper_exe` | `whisper-cli.exe` | The whisper.cpp runner used by the Indonesian sidecar |
| `keywords` | `[]` | Words and phrases to favour (names, jargon, product terms) |
| `vad_threshold_db` | `-45.0` | Raise toward `-35` if music opens empty utterances; lower if quiet speech is missed |
| `silence_close_ms` | `1000` | How much silence ends an utterance |
| `max_segment_secs` | `25.0` | Hard split; both engines cap a pass at 30 s |
| `queue_capacity` | `8` | Utterances buffered before the oldest is dropped |
| `save_transcript` | `true` | `false` keeps the session in memory only (tray ▸ Copy transcript, viewer ▸ Save .md as…); no file is created |
| `viewer_theme` | `"dark"` | `"light"`, or `"system"` to follow the Windows app theme |
| `engine_idle_unload_secs` | `120` | Drop the speech model after N paused seconds to save RAM (`0` = keep); reloads on demand |
| `whistle_decoder_depth` | unset | Speed/accuracy ladder for the speech decoder. **Only the `whistle-sidecar` build honours this**; the linked engine always runs full depth |

## Diagnostic modes

Each moving part can be checked on its own, which is also how this project was brought up:

```powershell
.\target\release\depth.exe --check                  # config, models, compiled engines
.\target\release\depth.exe --record 10 scratch.wav  # 10 s of desktop audio
.\target\release\depth.exe --once scratch.wav       # transcribe a WAV, print timings
.\target\release\depth.exe --headless               # pipeline without the tray
```

`--record` is the fastest way to confirm loopback capture works: play something, record 10 s, then
run `--once` on the result.

## Features

| Feature | Default | Purpose |
| --- | --- | --- |
| `whistle-sidecar` | **on** | English by running the bundled `needle.exe`; needs no C++ toolchain |
| `whisper-sidecar` | **on** | Indonesian by running the prebuilt `whisper-cli.exe`; needs no C++ toolchain |
| `tray` | on | Tray icon, menu, hotkey and shell integration |
| `whistle` | off | English linked in-process via `libneedle.a` (lowest latency; needs libc++) |
| `whisper` | off | Indonesian compiled in-process via `whisper-rs` (needs a working CMake build) |

### Why `whistle-sidecar` is the default

Cactus ship `libneedle.a` built for **clang + libc++**. Inspecting the object shows
`std::__1::basic_string`, `__cxa_begin_catch` and `operator new` — libc++ symbols, not MSVC's STL,
and the C++ runtime is not inside the archive. Consequences:

- MSVC's `link.exe` rejects the object outright: `LNK1143: invalid or corrupt file: no symbol for
  COMDAT section 0x5`.
- LLVM's `rust-lld` *does* read it, then fails with undefined `std::__1::…` and `__cxa_*` symbols,
  because libc++ is missing.

`needle.exe` from the same folder has libc++ statically linked, which is why the sidecar works
everywhere with no extra toolchain. It costs a process spawn per utterance (~1–2 s, observed), so
the tradeoff is latency rather than accuracy: both paths run the identical model and engine.

To try the in-process engine, supply libc++ for the MSVC target (a portable LLVM release provides
`libc++.lib`, `libc++abi.lib` and `libunwind.lib`), point `LIB` at them, and build with
`--no-default-features --features whistle,tray`.

Build without the Indonesian engine to skip the whisper.cpp compile entirely:

```powershell
.\scripts\dev-shell.ps1 cargo build --release --no-default-features --features whistle-sidecar,tray
```

## Model attribution

Whistle and the `needle` engine are by [Cactus Compute](https://cactuscompute.com), licensed
Apache-2.0 ([weights](https://huggingface.co/Cactus-Compute/whistle),
[engine](https://huggingface.co/Cactus-Compute/needle3), [source](https://github.com/cactus-compute/needle)).
Whisper checkpoints come from [whisper.cpp](https://github.com/ggml-org/whisper.cpp) (MIT).

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `linker link.exe not found` | Build through `scripts\dev-shell.ps1`, or install the VS C++ workload |
| `LNK1104 … lnk{GUID}.tmp`, or NSIS `error creating mmap` | The build tools cannot write to `%TEMP%` (some sandboxes redirect it). Point `TMP` and `TEMP` at a writable folder, e.g. `$env:TMP = "$PWD\target\buildtmp"`, and rebuild |
| `LNK1143 … COMDAT` or undefined `std::__1::…` | You built with the `whistle` feature; `libneedle.a` needs libc++. Use the default `whistle-sidecar` build |
| `cmake was not found` | Add the CMake tools component to the C++ workload |
| Tooltip shows "Error: Whistle model not found" | Run `python scripts/fetch_assets.py whistle` |
| The hotkey does nothing | Another app owns that combination; the log names the one actually registered |
| Transcript stays empty | Check the log for `capture is receiving audio`; if absent, nothing is playing on the logged-on device, or the stream is exclusive-mode/DRM-protected |
| Utterances appear during music | Raise `vad_threshold_db` toward `-35`, or add silence to the mix |
| Indonesian accuracy is poor | Use a larger checkpoint (`small` → `medium`), or accept that Whistle cannot cover Indonesian |


