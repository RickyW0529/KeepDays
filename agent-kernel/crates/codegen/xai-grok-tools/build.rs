//! Offline-hermetic build script for `xai-grok-tools`.
//!
//! Bundles optional static search binaries (`rg`, `fd`, `bfs`, `ugrep`) ONLY when
//! `GROK_TOOLS_BUNDLE_<NAME>_PATH` is explicitly set by the caller. Otherwise emits
//! `rustc-check-cfg` and lets `xai-grok-tools` fall back cleanly to `rg`/`fd` on PATH
//! or native Rust directory/regex search without requiring network access at build time.
use std::env;
use std::fs;
use std::path::PathBuf;

const RG_VER: &str = "15.0.0";
const BFS_VER: &str = "4.1";
const UGREP_VER: &str = "7.7.0";
const FD_VER: &str = "10.4.2";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    bundle_search_tool("rg", "RG", RG_VER)?;
    bundle_search_tool("fd", "FD", FD_VER)?;
    bundle_search_tool("bfs", "BFS", BFS_VER)?;
    bundle_search_tool("ugrep", "UGREP", UGREP_VER)?;
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn compress_and_pin(
    dest: &std::path::Path,
    name_uc: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read(dest)?;
    let sha = {
        use sha2::Digest as _;
        hex_encode(&sha2::Sha256::digest(&bytes))
    };

    let compressed = zstd::encode_all(bytes.as_slice(), 19)?;
    let mut zst = dest.to_path_buf().into_os_string();
    zst.push(".zst");
    fs::write(&zst, &compressed)?;

    println!("cargo:rustc-env=GROK_TOOLS_{name_uc}_SHA256={sha}");
    Ok(())
}

fn bundle_search_tool(
    name: &str,
    name_uc: &str,
    ver: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let override_env = format!("GROK_TOOLS_BUNDLE_{name_uc}_PATH");
    println!("cargo:rerun-if-env-changed={override_env}");
    println!("cargo:rustc-check-cfg=cfg(bundle_{name})");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        return Ok(());
    }

    let Some(src) = env::var(&override_env).ok().filter(|s| !s.is_empty()) else {
        return Ok(());
    };

    let gen_dir = PathBuf::from(env::var("OUT_DIR")?).join(format!("bundle-{name}"));
    fs::create_dir_all(&gen_dir)?;
    let dest = gen_dir.join(format!("{name}-{ver}-override.bin"));
    let _ = fs::remove_file(&dest);
    fs::copy(&src, &dest)
        .map_err(|e| format!("copy {override_env} from {src} to {}: {e}", dest.display()))?;

    println!("cargo:rustc-cfg=bundle_{name}");
    println!("cargo:rustc-env=GROK_TOOLS_{name_uc}_VER={ver}");
    println!("cargo:rustc-env=GROK_TOOLS_{name_uc}_TARGET=override");
    compress_and_pin(&dest, name_uc)?;
    Ok(())
}
