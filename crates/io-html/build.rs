//! `vma-html` finds CEF's files next to itself: copied beside the binary (in `target/` here, by
//! the package later). Linux also needs `$ORIGIN` in its search path; Windows looks there already.
//!
//! `cef-dll-sys` copies them three levels above its `OUT_DIR`, which is the profile folder
//! (`target/release`) only in Cargo's former layout: since Rust 1.100 that lands in
//! `target/release/build` (CI of 2026-11-12, `libcef.so` not found). Copied here as well, into
//! the profile folder found by name.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_CEF").is_none() {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "linux") {
        println!("cargo::rustc-link-arg-bin=vma-html=-Wl,-rpath,$ORIGIN");
    }
    let Some(cef_dir) = std::env::var_os("DEP_CEF_DLL_WRAPPER_CEF_DIR").map(PathBuf::from) else {
        println!("cargo::warning=CEF directory unknown: its files are not copied next to vma-html");
        return;
    };
    println!("cargo::rerun-if-changed={}", cef_dir.display());
    let Some(profile_dir) = profile_dir() else {
        println!("cargo::warning=profile folder not found: CEF is not copied next to vma-html");
        return;
    };
    for (from, to) in [
        (cef_dir.clone(), profile_dir.clone()),
        (cef_dir.join("locales"), profile_dir.join("locales")),
    ] {
        if let Err(e) = copy_files(&from, &to) {
            println!(
                "cargo::warning=CEF not copied from {} to {}: {e}",
                from.display(),
                to.display()
            );
        }
    }
}

/// The nearest folder above `OUT_DIR` named after the profile (`release`, `debug`).
fn profile_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR")?);
    let profile = std::env::var("PROFILE").ok()?;
    out_dir
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == profile.as_str()))
        .map(Path::to_path_buf)
}

/// Copies the files of `from` (not its folders) into `to`, but those already there with the same
/// size (a copy of CEF is about a gigabyte).
fn copy_files(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        let dest = to.join(entry.file_name());
        let same = std::fs::metadata(&dest).is_ok_and(|d| d.len() == metadata.len());
        if !same {
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}
