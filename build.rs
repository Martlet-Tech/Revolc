//! Embeds `assets/icon.ico` into the executable.
//!
//! Two consumers:
//!   * the PE resource (RT_GROUP_ICON id 1) — that is what Explorer, the taskbar
//!     and Alt-Tab read for the .exe itself;
//!   * `src/ui/icon.rs` loads the *same* resource id at run time for the window
//!     class and the tray icon, so there is exactly one source of truth.
//!
//! Dependency-free on purpose: it drives the `rc.exe` that ships with the MSVC
//! toolchain instead of adding a build-dependency that would have to be
//! fetched. If `rc.exe` cannot be found the build still succeeds, just without
//! an icon (the runtime loader then falls back to the stock application icon).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        println!("cargo:warning=app icon embedding skipped: msvc toolchain required");
        return;
    }

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = manifest.join("assets").join("icon.ico");
    if !ico.is_file() {
        println!("cargo:warning=assets/icon.ico not found; building without an app icon");
        return;
    }

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    // Copy the .ico next to the .rc and reference it by bare file name: the
    // resource compiler then never has to parse a path with backslashes/spaces.
    if let Err(e) = fs::copy(&ico, out.join("icon.ico")) {
        println!("cargo:warning=cannot stage icon.ico in OUT_DIR: {e}");
        return;
    }
    if let Err(e) = fs::write(out.join("app.rc"), "1 ICON \"icon.ico\"\n") {
        println!("cargo:warning=cannot write app.rc: {e}");
        return;
    }

    let Some(rc_exe) = find_rc() else {
        println!("cargo:warning=rc.exe not found; building without an app icon");
        return;
    };

    let ok = Command::new(&rc_exe)
        .current_dir(&out)
        .args(["/nologo", "/fo", "app.res", "app.rc"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let res = out.join("app.res");
    if !ok || !res.is_file() {
        println!("cargo:warning=rc.exe failed; building without an app icon");
        return;
    }
    println!("cargo:rustc-link-arg={}", res.display());
}

fn find_rc() -> Option<PathBuf> {
    for var in ["RC", "RC_PATH"] {
        if let Ok(p) = env::var(var) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
    }

    // PATH
    if let Some(paths) = env::var_os("PATH") {
        for dir in env::split_paths(&paths) {
            let p = dir.join("rc.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }

    // Windows SDK layouts: <Kits>\<ver>\bin\<arch>\rc.exe
    let mut kits: Vec<PathBuf> = Vec::new();
    for key in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Ok(root) = env::var(key) {
            let root = PathBuf::from(root);
            kits.push(root.join("Windows Kits").join("10").join("bin"));
            kits.push(root.join("Windows Kits").join("8.1").join("bin"));
        }
    }
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86") => "x86",
        _ => "x64",
    };
    let mut candidates: Vec<(Vec<u32>, PathBuf)> = Vec::new();
    for base in kits {
        collect(&base, arch, &mut candidates);
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates.pop().map(|(_, p)| p)
}

fn collect(base: &Path, arch: &str, out: &mut Vec<(Vec<u32>, PathBuf)>) {
    let Ok(rd) = fs::read_dir(base) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let ver: Vec<u32> = p
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.split('.').filter_map(|s| s.parse().ok()).collect())
            .unwrap_or_default();
        // <Kits>\10\bin\<ver>\<arch>\rc.exe
        let rc = p.join(arch).join("rc.exe");
        if rc.is_file() {
            out.push((ver, rc));
        }
    }
}
