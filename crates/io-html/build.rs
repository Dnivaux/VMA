//! `vma-html` finds `libcef.so` next to itself (Linux): CEF's files are copied beside the binary
//! (by `cef-dll-sys` in `target/`, by the package later). Windows looks there already.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let cef = std::env::var_os("CARGO_FEATURE_CEF").is_some();
    let linux = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "linux");
    if cef && linux {
        println!("cargo::rustc-link-arg-bin=vma-html=-Wl,-rpath,$ORIGIN");
    }
}
