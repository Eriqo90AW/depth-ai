//! Build script for depth.
//!
//! Two jobs:
//!
//! 1. Embed the application icon and its version information, so the executable, its shortcuts
//!    and the entry in Apps & features all show the Depth mark. See `build/winres.rs`:
//!    the resource file is written directly, which needs no resource compiler and no extra crate.
//! 2. Link the Cactus "needle" engine that runs the Whistle speech model, when the `whistle`
//!    feature is on.
//!
//! About the engine: it ships as an MSVC-format static library
//! (`vendor/needle/windows-x86_64/libneedle.a`) built from C++, so a Windows MSVC toolchain
//! (`link.exe`) is required to link it. `needle_transcribe` etc. are declared `extern "C"` in
//! `src/engine/whistle.rs`.
//!
//! If linking that library ever fails in a way you cannot resolve, build with
//! `--no-default-features --features whistle-sidecar,tray` instead: that path shells out to
//! the bundled `needle.exe` and needs no static library at all.

use std::path::{Path, PathBuf};

#[path = "build/winres.rs"]
mod winres;

fn main() {
    embed_icon_and_version();
    if std::env::var_os("CARGO_FEATURE_TRAY").is_some() {
        // Element metadata lets UI tests exercise the actual confirmation controls.
        let config = slint_build::CompilerConfiguration::new().with_debug_info(true);
        slint_build::compile_with_config("ui/app.slint", config).expect("compile Depth UI");
    }

    let engine_dir = Path::new("vendor/needle/windows-x86_64");
    let static_lib = engine_dir.join("libneedle.a");

    let whistle = std::env::var_os("CARGO_FEATURE_WHISTLE").is_some();
    let sidecar = std::env::var_os("CARGO_FEATURE_WHISTLE_SIDECAR").is_some();

    if !whistle {
        if sidecar {
            // The sidecar path runs needle.exe; make sure it exists next to the build.
            println!(
                "cargo:warning=whistle-sidecar: expecting {} at runtime",
                engine_dir.join("needle.exe").display()
            );
        }
        return;
    }

    if !static_lib.exists() {
        panic!(
            "{} is missing. Run `python scripts/fetch_assets.py whistle` to download the engine.",
            static_lib.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", engine_dir.display());
    println!("cargo:rustc-link-lib=static=needle");
    println!("cargo:rerun-if-changed={}", static_lib.display());
    println!(
        "cargo:rerun-if-changed={}",
        engine_dir.join("needle.h").display()
    );

    // libneedle.a is C++, so the C++ runtime must be linked alongside it. `msvcprt` is the
    // dynamic MSVC C++ runtime; switch to `libcpmt` if the library was built with /MT.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        println!("cargo:rustc-link-lib=dylib=msvcprt");
        println!("cargo:rustc-link-lib=dylib=advapi32");
    }
}

/// Compile `assets/icon/depth.ico` and the crate's version strings into a `.res` file
/// and add it to the binary's link line.
///
/// Regenerate the icon with `python scripts/make_icon.py` after changing the artwork; this
/// script only reads it. Only the MSVC linker accepts resource files, so other targets skip
/// this step rather than failing.
fn embed_icon_and_version() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }

    let icon = Path::new("assets/icon/depth.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed=build/winres.rs");

    let bytes = std::fs::read(icon).unwrap_or_else(|err| {
        panic!(
            "{} could not be read ({err}). Run `python scripts/make_icon.py` to generate it.",
            icon.display()
        )
    });
    let images = winres::parse_ico(&bytes).unwrap_or_else(|err| {
        panic!(
            "{} is not a usable icon ({err}). Run `python scripts/make_icon.py` to regenerate it.",
            icon.display()
        )
    });

    let version = parse_version(env!("CARGO_PKG_VERSION"));
    let info = winres::VersionInfo {
        product: "Depth",
        description: "Depth — on-device transcription of desktop audio",
        company: "Depth",
        internal_name: "depth",
        original_filename: "depth.exe",
        copyright: "Apache-2.0",
        version,
    };

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is always set");
    let out = PathBuf::from(out_dir).join("depth.res");
    std::fs::write(&out, winres::write_res(&images, &info))
        .unwrap_or_else(|err| panic!("{} could not be written ({err})", out.display()));

    // Positional link input: the MSVC linker picks the resources up from the `.res` file.
    // `-bins` keeps it away from test and example link lines, which have no icon.
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// `"0.1.0"` → `(0, 1, 0, 0)`; the version block wants four numbers.
fn parse_version(version: &str) -> (u16, u16, u16, u16) {
    let mut parts = version
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u16>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}
