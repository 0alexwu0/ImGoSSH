//! Gives `imgossh.exe` a Windows icon of its own.
//!
//! The pixels come from `src/icon.rs` - the very same asset the window shrinks at
//! runtime - and are assembled into a multi-size `.ico`, compiled by the SDK's
//! `rc.exe`, and handed to the linker as a `.res`. Everything here is best
//! effort: a machine without the Windows SDK gets a warning and the stock icon
//! rather than a failed build.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/icon.rs"]
mod icon;

/// The sizes a `.ico` offers, from taskbar overlay to the large Explorer tile.
const SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=assets/icon.png");
    println!("cargo:rerun-if-env-changed=IMGOSSH_RC");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        warn(
            "only the MSVC target knows how to embed an .ico; the executable keeps the stock icon",
        );
        return;
    }

    let Some(compiler) = find_rc() else {
        warn("rc.exe was not found - point IMGOSSH_RC at it to give imgossh.exe its own icon");
        return;
    };

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo always sets OUT_DIR"));
    let (icon, script, resource) = (
        out.join("imgossh.ico"),
        out.join("imgossh.rc"),
        out.join("imgossh.res"),
    );
    if let Err(err) = std::fs::write(&icon, icon_file()) {
        warn(&format!("cannot write {}: {err}", icon.display()));
        return;
    }
    // A bare ICON statement pulls in no headers, so rc.exe also works outside a
    // developer prompt, where INCLUDE is empty.
    if let Err(err) = std::fs::write(&script, "1 ICON \"imgossh.ico\"\n") {
        warn(&format!("cannot write {}: {err}", script.display()));
        return;
    }

    match Command::new(&compiler)
        .current_dir(&out)
        .args(["/nologo", "/fo"])
        .arg(&resource)
        .arg(&script)
        .status()
    {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg-bin=imgossh={}", resource.display());
        }
        Ok(status) => warn(&format!(
            "{} exited with {status}; imgossh.exe keeps the stock icon",
            compiler.display()
        )),
        Err(err) => warn(&format!("cannot run {}: {err}", compiler.display())),
    }
}

// ---------------------------------------------------------------------------
// The .ico itself
// ---------------------------------------------------------------------------

/// Assemble every painted size into one icon resource.
fn icon_file() -> Vec<u8> {
    let images: Vec<Vec<u8>> = SIZES
        .iter()
        .copied()
        .map(|size| bitmap(size, &icon::paint(size)))
        .collect();

    let mut file = Vec::new();
    push_u16(&mut file, 0);
    push_u16(&mut file, 1);
    push_u16(&mut file, images.len() as u16);

    // The directory precedes the image data, so the offsets are counted first.
    let mut offset = 6 + 16 * images.len();
    for (size, data) in SIZES.iter().zip(&images) {
        // A 256px entry is spelled 0 in both width and height.
        let byte = if *size >= 256 { 0 } else { *size as u8 };
        file.extend_from_slice(&[byte, byte, 0, 0]);
        push_u16(&mut file, 1);
        push_u16(&mut file, 32);
        push_u32(&mut file, data.len() as u32);
        push_u32(&mut file, offset as u32);
        offset += data.len();
    }
    for data in &images {
        file.extend_from_slice(data);
    }
    file
}

/// One image: a 32bpp bottom-up bitmap followed by the all-zero AND mask.
fn bitmap(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mask = size as usize * size.div_ceil(32) as usize * 4;
    let mut out = Vec::with_capacity(40 + rgba.len() + mask);
    push_u32(&mut out, 40);
    push_u32(&mut out, size);
    // The header counts the image and the mask, hence the doubled height.
    push_u32(&mut out, size * 2);
    push_u16(&mut out, 1);
    push_u16(&mut out, 32);
    // BI_RGB, with the optional sizes left for the reader to work out.
    for _ in 0..6 {
        push_u32(&mut out, 0);
    }
    for row in (0..size).rev() {
        for column in 0..size {
            let at = ((row * size + column) * 4) as usize;
            out.extend_from_slice(&[rgba[at + 2], rgba[at + 1], rgba[at], rgba[at + 3]]);
        }
    }
    out.resize(out.len() + mask, 0);
    out
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

// ---------------------------------------------------------------------------
// Finding rc.exe
// ---------------------------------------------------------------------------

/// `rc.exe` from an override, from `PATH`, or from a usual install directory.
fn find_rc() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("IMGOSSH_RC") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(from_path) = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("rc.exe"))
            .find(|candidate| candidate.is_file())
    }) {
        return Some(from_path);
    }
    sdk_candidates()
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// Where the Windows SDK and the MSVC toolset keep `rc.exe`, newest first.
fn sdk_candidates() -> Vec<PathBuf> {
    let arch = host_arch();
    let mut candidates = Vec::new();
    for root in program_files() {
        let kits = root.join(r"Windows Kits\10\bin");
        for version in child_dirs(&kits) {
            candidates.push(version.join(arch).join("rc.exe"));
        }
        candidates.push(kits.join(arch).join("rc.exe"));

        for year in child_dirs(&root.join(r"Microsoft Visual Studio")) {
            for edition in child_dirs(&year) {
                let tools = edition.join(r"VC\Tools\MSVC");
                for msvc in child_dirs(&tools) {
                    candidates.push(msvc.join(r"bin\Host").join(arch).join(arch).join("rc.exe"));
                }
            }
        }
    }
    candidates
}

/// The directories inside `dir`, newest-sounding version first.
fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut children = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect::<Vec<PathBuf>>()
        })
        .unwrap_or_default();
    children.sort_by_cached_key(|path| std::cmp::Reverse(version_key(path)));
    children
}

/// A directory name read as a version, so `10.0.22621.0` outranks `10.0.14393.0`.
fn version_key(path: &Path) -> Vec<u64> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            name.split(['.', '_'])
                .map(|part| part.parse::<u64>().unwrap_or(0))
                .collect()
        })
        .unwrap_or_default()
}

fn program_files() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for key in ["ProgramFiles(x86)", "ProgramFiles", "ProgramW6432"] {
        if let Some(value) = std::env::var_os(key) {
            let root = PathBuf::from(value);
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
    }
    if roots.is_empty() {
        roots.push(PathBuf::from(r"C:\Program Files (x86)"));
        roots.push(PathBuf::from(r"C:\Program Files"));
    }
    roots
}

/// The bitness of the directory to look in: rc.exe runs on the build host.
fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86" => "x86",
        _ => "x64",
    }
}

fn warn(message: &str) {
    println!("cargo:warning={message}");
}
