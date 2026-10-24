//! Links the runtime against Bun's JavaScriptCore build (oven-sh/WebKit).
//!
//! The prebuilt package is expected at `vendor/bun-webkit` (see
//! `scripts/fetch-webkit.ps1`), or at the path given by `BUN_WEBKIT_DIR`.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=BUN_WEBKIT_DIR");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let webkit_dir = env::var_os("BUN_WEBKIT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("vendor").join("bun-webkit"));
    let lib_dir = webkit_dir.join("lib");

    if !lib_dir.join("JavaScriptCore.lib").exists() && !lib_dir.join("libJavaScriptCore.a").exists() {
        panic!(
            "\n\nBun's JavaScriptCore build was not found at `{}`.\n\
             Run `./scripts/fetch-webkit.ps1` to download it, or set BUN_WEBKIT_DIR.\n\n",
            webkit_dir.display()
        );
    }

    println!("cargo:rerun-if-changed={}", lib_dir.display());
    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    // JavaScriptCore and its support libraries (static).
    for lib in ["JavaScriptCore", "WTF", "bmalloc"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target_os == "windows" {
        // Bundled static ICU.
        for lib in ["sicuin", "sicuuc", "sicudt"] {
            println!("cargo:rustc-link-lib=static={lib}");
        }
        // Win32 system libraries required by WTF/JSC.
        for lib in [
            "winmm", "ws2_32", "advapi32", "user32", "shell32", "ole32", "oleaut32",
            "dbghelp", "bcrypt", "ntdll", "synchronization", "psapi", "version",
        ] {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
    } else {
        println!("cargo:rustc-link-lib=dylib=icuuc");
        println!("cargo:rustc-link-lib=dylib=icui18n");
        println!("cargo:rustc-link-lib=dylib=icudata");
        println!("cargo:rustc-link-lib=dylib=stdc++");
        println!("cargo:rustc-link-lib=dylib=atomic");
    }
}
