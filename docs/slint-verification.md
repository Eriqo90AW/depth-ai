# Slint redesign verification

The redesign is implemented in the current workspace. Visual acceptance remains pending after
Computer Use was stopped with Escape. Desktop interaction stopped at that point.

## Implemented

- Slint 1.18.1 main window, settings, interactive overlay and completion notification, sharing one
  winit event loop. Skia and software rendering are compiled in. The tray has Open Depth, Start/Stop
  and Quit; closing the main window hides it. Startup shortcuts pass `--background`.
- Main-window English/Indonesian selector saves immediately, synchronizes with Settings and applies
  to the next recording. The main window is frameless, with custom keyboard-accessible minimize,
  maximize/restore and close buttons, a draggable/double-clickable title bar and resize edges.
- Recording IDs created before capture, capture-clock segment timing, draining Stop/Shutdown,
  queued-result retention and visible engine, capture, queue and save warnings.
- Newest-first library, display-title changes, paragraph/timed views, Unicode selection and Find,
  selection copying, whole-recording copying, Markdown/plain-text export and scroll preservation.
- Flushed JSONL journals, debounced atomic Markdown replacement, crash recovery, lazy saved-content
  loading and complete in-memory unsaved recordings. Legacy daily files retain their originals and
  clock times. Legacy title changes use separate metadata.
- Validated settings, preserved existing preferences, System appearance and Ctrl+Alt+Space defaults,
  immediate appearance changes and next-recording capture/engine/autosave changes.
- Isolated live-preview helper at `scripts/dev-ui.ps1`, with empty, recording, long, settings and
  error sample modes. Preview modes do not capture audio or register a hotkey.

## Verified

- `cargo test`: 69 passing tests, including 2,501-utterance Unicode documents, selection and scroll
  preservation, following the document end, stop during blocked inference, queued and flushed
  results, midnight identity, empty recordings, legacy import and renaming, damaged trailing events,
  atomic replacement failures, configuration compatibility and failed-autosave memory retention.
- `cargo fmt --check` passes.
- `cargo check --features slint/live-preview` passes.
- `cargo check --no-default-features --features whistle-sidecar,whisper-sidecar` passes.
- Debug and release diagnostic launches with isolated homes exit successfully and find both bundled models.
- The final optimized release build passes without live preview.
- A debug long-document preview launched on Windows and was inspected through an actual native
  screenshot. That review led to a cursor/scroll correction, covered by the Slint regression test.

## Remaining manual acceptance

Computer Use was stopped before the empty, recording, settings and error screenshots were reviewed.
The long-document screenshot also predates the final visual changes. Check all five final states,
Light/Dark/System contrast, keyboard navigation, resizing, 100–200% scaling, mixed-DPI monitors,
software rendering, overlay focus behavior and Windows 10 solid fallback. The new title bar also
needs native drag, double-click, resize, minimize, maximize/restore and close-to-tray checks.

The release build uses the normal optimized profile without live preview. A release GUI launch and
real audio capture/inference session still need interactive verification. Automated pipeline tests
use a controlled engine so Stop can be tested while inference is blocked. No installer was created.

## Development commands

```powershell
.\scripts\dev-ui.ps1 -Preview long
.\scripts\dev-ui.ps1 -Preview empty
.\scripts\dev-ui.ps1 -Preview recording
.\scripts\dev-ui.ps1 -Preview settings
.\scripts\dev-ui.ps1 -Preview error
.\scripts\dev-ui.ps1 -Software
```

Quit any installed instance before testing actual capture to avoid competing hotkeys. Rust and
shared property/callback changes require restarting the development app.

