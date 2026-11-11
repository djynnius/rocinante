//! Compile `shim/rcn_llama.cpp` against the llama.cpp sources llama-cpp-sys-2
//! builds and links, so the shim and the library always come from the same
//! llama.cpp commit.

use std::env;
use std::path::PathBuf;

/// Must equal the exact `llama-cpp-sys-2` pin in Cargo.toml.
const SYS_VERSION: &str = "0.1.160";

fn main() {
    println!("cargo:rerun-if-changed=shim/rcn_llama.cpp");
    println!("cargo:rerun-if-env-changed=ROCINANTE_LLAMA_SRC");

    let src = llama_src();
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .file("shim/rcn_llama.cpp")
        .include(src.join("include"))
        .include(src.join("common"))
        .include(src.join("ggml/include"))
        .include(src.join("vendor"))
        .warnings(false)
        // llama-cpp-sys-2 already links the C++ runtime the right way for
        // the target (static libstdc++ on musl, libc++ on Apple); a second
        // request from cc would ask for a dynamic one.
        .cpp_link_stdlib(None);
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        build.flag("/std:c++17").flag("/utf-8");
    } else {
        build.flag("-std=c++17");
    }
    build.compile("rcn_llama");
}

/// The `llama.cpp` directory inside the registry copy of llama-cpp-sys-2.
/// `ROCINANTE_LLAMA_SRC` overrides the search (vendored or offline builds).
fn llama_src() -> PathBuf {
    if let Some(dir) = env::var_os("ROCINANTE_LLAMA_SRC") {
        return PathBuf::from(dir);
    }
    let crate_dir = format!("llama-cpp-sys-2-{SYS_VERSION}");
    let mut homes: Vec<PathBuf> = Vec::new();
    if let Some(home) = env::var_os("CARGO_HOME") {
        homes.push(PathBuf::from(home));
    }
    for var in ["HOME", "USERPROFILE"] {
        if let Some(home) = env::var_os(var) {
            homes.push(PathBuf::from(home).join(".cargo"));
        }
    }
    for home in &homes {
        let Ok(indexes) = std::fs::read_dir(home.join("registry").join("src")) else {
            continue;
        };
        for index in indexes.flatten() {
            let candidate = index.path().join(&crate_dir).join("llama.cpp");
            if candidate.join("common").join("chat.h").is_file() {
                return candidate;
            }
        }
    }
    panic!(
        "rocinante-llama: could not find the llama.cpp sources of {crate_dir} under \
         {homes:?}/registry/src — set ROCINANTE_LLAMA_SRC to its llama.cpp directory"
    );
}
