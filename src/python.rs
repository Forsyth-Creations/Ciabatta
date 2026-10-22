//! The Python library for `ciabatta input`, carried in the binary.
//!
//! `python/ciabatta/__init__.py` is compiled in, so whichever ciabatta a team
//! has installed can hand out the matching library: `ciabatta python wheel`
//! writes it as a wheel, `ciabatta python install` pip-installs it. The wheel
//! is pure Python (`py3-none-any`, standard library only, 3.9 and newer), so
//! the one file installs on Linux, macOS and Windows alike.
//!
//! It is distributed as `ciabatta-py` — the name pip installs and uninstalls —
//! and imported as `ciabatta`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// The module, as written in the repo. Its `__version__` placeholder is
/// replaced with this binary's version when it is packed.
const SOURCE: &str = include_str!("../python/ciabatta/__init__.py");
const VERSION_PLACEHOLDER: &str = "__version__ = \"0.0.0\"";

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The distribution's name as pip shows it, and as a wheel's file name and
/// `.dist-info` spell it (PEP 427: `-` becomes `_`).
const DIST_NAME: &str = "ciabatta-py";
const DIST_FILE_NAME: &str = "ciabatta_py";

/// The wheel's file name, as pip expects it.
pub fn wheel_name() -> String {
    format!("{DIST_FILE_NAME}-{VERSION}-py3-none-any.whl")
}

/// The module with its version filled in.
fn module_source() -> String {
    SOURCE.replacen(
        VERSION_PLACEHOLDER,
        &format!("__version__ = \"{VERSION}\""),
        1,
    )
}

/// The wheel's bytes: the module, its `py.typed` marker, and the
/// `.dist-info` pip reads — `METADATA`, `WHEEL`, and a `RECORD` hashing the
/// rest.
pub fn wheel() -> Vec<u8> {
    let info = format!("{DIST_FILE_NAME}-{VERSION}.dist-info");
    let metadata = format!(
        "Metadata-Version: 2.1\n\
         Name: {DIST_NAME}\n\
         Version: {VERSION}\n\
         Summary: Ask the person running a ciabatta workflow a question, from Python\n\
         Home-page: https://forsyth-creations.github.io/Ciabatta/\n\
         License: MIT\n\
         Requires-Python: >=3.9\n"
    );
    let wheel = format!(
        "Wheel-Version: 1.0\n\
         Generator: ciabatta {VERSION}\n\
         Root-Is-Purelib: true\n\
         Tag: py3-none-any\n"
    );
    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("ciabatta/__init__.py".into(), module_source().into_bytes()),
        ("ciabatta/py.typed".into(), Vec::new()),
        (format!("{info}/METADATA"), metadata.into_bytes()),
        (format!("{info}/WHEEL"), wheel.into_bytes()),
    ];
    let mut record = String::new();
    for (name, data) in &files {
        let digest = base64_url(&Sha256::digest(data));
        record.push_str(&format!("{name},sha256={digest},{}\n", data.len()));
    }
    record.push_str(&format!("{info}/RECORD,,\n"));
    files.push((format!("{info}/RECORD"), record.into_bytes()));
    zip_stored(&files)
}

/// Write the wheel into `dir` and say where it went.
pub fn write_wheel(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(wheel_name());
    std::fs::write(&path, wheel()).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// The Python to install into when none is named: `python` on Windows, where
/// that is what the installer puts on PATH, `python3` everywhere else.
pub fn default_python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

/// `<python> -m pip install --force-reinstall <wheel> <pip_args…>`, with the
/// wheel written to a temporary directory first. Reinstalling is what makes a
/// newer ciabatta's library replace an older one of the same version.
pub fn install(python: &str, pip_args: &[String]) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ciabatta-python-{}", std::process::id()));
    let wheel = write_wheel(&dir)?;
    let status = Command::new(python)
        .args(["-m", "pip", "install", "--force-reinstall", "--no-deps"])
        .arg(&wheel)
        .args(pip_args)
        .status()
        .with_context(|| format!("running {python} — is it installed and on PATH?"));
    let _ = std::fs::remove_dir_all(&dir);
    let status = status?;
    anyhow::ensure!(
        status.success(),
        "{python} -m pip install failed ({status}). `ciabatta python wheel` writes the \
         wheel so you can install it yourself."
    );
    Ok(())
}

/// A zip of uncompressed entries — all a wheel needs, and nothing to depend
/// on. Timestamps are pinned to 1980-01-01, so the same version always packs
/// to the same bytes.
fn zip_stored(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    const TIME: u16 = 0;
    const DATE: u16 = (1 << 5) | 1; // 1980-01-01
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        for field in [20u16, 0, 0, TIME, DATE] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        for field in [crc, size, size] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        // Its central directory entry.
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        for field in [(3u16 << 8) | 20, 20, 0, 0, TIME, DATE] {
            central.extend_from_slice(&field.to_le_bytes());
        }
        for field in [crc, size, size] {
            central.extend_from_slice(&field.to_le_bytes());
        }
        for field in [name.len() as u16, 0, 0, 0, 0] {
            central.extend_from_slice(&field.to_le_bytes());
        }
        // A regular file, rw-r--r--.
        central.extend_from_slice(&(0o100644u32 << 16).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let start = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    let count = files.len() as u16;
    for field in [0u16, 0, count, count] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// The zip format's CRC-32 (IEEE, reflected).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// URL-safe base64 without padding, as a wheel's `RECORD` writes hashes.
fn base64_url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let bits = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | u32::from(b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(bits >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksums_match_the_formats() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(base64_url(b"\xfb\xff"), "-_8");
        assert_eq!(base64_url(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_url(b"fooba"), "Zm9vYmE");
    }

    #[test]
    fn the_wheel_carries_the_module_at_this_version() {
        assert!(
            SOURCE.contains(VERSION_PLACEHOLDER),
            "the placeholder moved"
        );
        let source = module_source();
        assert!(source.contains(&format!("__version__ = \"{VERSION}\"")));

        let bytes = wheel();
        let text = String::from_utf8_lossy(&bytes);
        for name in [
            "ciabatta/__init__.py".to_string(),
            format!("ciabatta_py-{VERSION}.dist-info/METADATA"),
            format!("ciabatta_py-{VERSION}.dist-info/WHEEL"),
            format!("ciabatta_py-{VERSION}.dist-info/RECORD"),
        ] {
            assert!(text.contains(&name), "{name} missing");
        }
        assert!(text.contains("Name: ciabatta-py\n"));
        assert_eq!(
            wheel_name(),
            format!("ciabatta_py-{VERSION}-py3-none-any.whl")
        );
        assert!(text.contains("Tag: py3-none-any"));
        // Ends with the end-of-central-directory record, no comment.
        assert_eq!(&bytes[bytes.len() - 22..bytes.len() - 18], b"PK\x05\x06");
        // Packing is reproducible.
        assert_eq!(bytes, wheel());
    }
}
