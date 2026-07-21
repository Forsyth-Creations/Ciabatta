//! `ciabatta usb` — pick a serial (USB) device in the browser and send it a
//! file as a raw byte stream, with an optional hex prefix prepended.
//!
//! The heavy lifting lives here: enumerating serial ports and performing the
//! blocking write. [`server`] wraps this in a tiny embedded web app (same shape
//! as [`crate::todo::server`]) so the port, baud rate, prefix, and file are all
//! chosen from a single page.
//!
//! The file is uploaded from the browser as a hex string (see [`server`]), so
//! both the prefix and the file body arrive as hex and are decoded here. That
//! keeps everything on one safe text code path and needs no extra dependency.

use std::time::Duration;

use anyhow::{Result, bail};
use serde::Serialize;

pub mod server;

/// How long a single serial write is allowed to block before timing out.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// A serial port offered in the picker.
#[derive(Debug, Clone, Serialize)]
pub struct PortInfo {
    /// The OS name used to open the port (`/dev/ttyACM0`, `COM3`, …).
    pub name: String,
    /// A human label for USB ports (manufacturer / product), empty otherwise.
    pub label: String,
    /// `vid:pid` in hex for USB ports (e.g. `2341:0043`), empty otherwise.
    pub usb_id: String,
    /// Whether this is a USB port (vs. a native/PCI/Bluetooth serial port).
    pub is_usb: bool,
}

/// List the serial ports currently present on the system, USB ports first and
/// then everything else, each group sorted by name. Hotplugged devices appear
/// on the next call, so the UI can simply re-request this to refresh.
pub fn list_ports() -> Result<Vec<PortInfo>> {
    use serialport::SerialPortType;

    let mut ports: Vec<PortInfo> = serialport::available_ports()
        .map_err(|e| anyhow::anyhow!("Could not list serial ports: {e}"))?
        .into_iter()
        .map(|p| match p.port_type {
            SerialPortType::UsbPort(info) => {
                let label = match (info.manufacturer.as_deref(), info.product.as_deref()) {
                    (Some(m), Some(pr)) => format!("{m} {pr}"),
                    (Some(m), None) => m.to_string(),
                    (None, Some(pr)) => pr.to_string(),
                    (None, None) => String::new(),
                };
                PortInfo {
                    name: p.port_name,
                    label,
                    usb_id: format!("{:04x}:{:04x}", info.vid, info.pid),
                    is_usb: true,
                }
            }
            _ => PortInfo {
                name: p.port_name,
                label: String::new(),
                usb_id: String::new(),
                is_usb: false,
            },
        })
        .collect();

    // USB devices first (the common case for this tool), then by name.
    ports.sort_by(|a, b| b.is_usb.cmp(&a.is_usb).then_with(|| a.name.cmp(&b.name)));
    Ok(ports)
}

/// Decode `prefix` (hex) and `data` (hex), concatenate `prefix ++ data`, and
/// write the result to the named serial port at `baud`. Returns the number of
/// bytes written on the wire (prefix + file).
///
/// `prefix` may be empty. Both strings tolerate spaces and an optional `0x`
/// and are case-insensitive; an odd number of hex digits is rejected.
pub fn send(port: &str, baud: u32, prefix: &str, data: &str) -> Result<usize> {
    use std::io::Write;

    let mut bytes = decode_hex(prefix).map_err(|e| anyhow::anyhow!("prefix: {e}"))?;
    let file_bytes = decode_hex(data).map_err(|e| anyhow::anyhow!("file: {e}"))?;
    bytes.extend_from_slice(&file_bytes);

    if bytes.is_empty() {
        bail!("Nothing to send: both the prefix and the file are empty.");
    }

    let mut handle = serialport::new(port, baud)
        .timeout(WRITE_TIMEOUT)
        .open()
        .map_err(|e| anyhow::anyhow!("Could not open {port} at {baud} baud: {e}"))?;

    handle
        .write_all(&bytes)
        .map_err(|e| anyhow::anyhow!("Write to {port} failed: {e}"))?;
    handle
        .flush()
        .map_err(|e| anyhow::anyhow!("Flushing {port} failed: {e}"))?;

    Ok(bytes.len())
}

/// Decode a hex string to bytes. Spaces are ignored, an optional leading `0x`
/// is stripped, and case is irrelevant. An odd number of hex digits, or any
/// non-hex character, is an error.
pub fn decode_hex(s: &str) -> Result<Vec<u8>> {
    // Drop whitespace and an optional 0x/0X prefix so "0x DE AD" works.
    let cleaned: String = s.split_whitespace().collect();
    let cleaned = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
        .unwrap_or(&cleaned);

    if cleaned.is_empty() {
        return Ok(Vec::new());
    }
    if !cleaned.len().is_multiple_of(2) {
        bail!("odd number of hex digits ({} nibbles)", cleaned.len());
    }

    let mut out = Vec::with_capacity(cleaned.len() / 2);
    let bytes = cleaned.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = nibble(pair[0])?;
        let lo = nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

/// Convert a single ASCII hex digit to its 0–15 value.
fn nibble(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => bail!("invalid hex character '{}'", c as char),
    }
}

#[cfg(test)]
mod tests {
    use super::decode_hex;

    #[test]
    fn decodes_plain_hex() {
        assert_eq!(
            decode_hex("deadbeef").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
        assert_eq!(
            decode_hex("DEADBEEF").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
    }

    #[test]
    fn tolerates_spaces_and_0x_prefix() {
        assert_eq!(decode_hex("0x DE AD").unwrap(), vec![0xde, 0xad]);
        assert_eq!(decode_hex("  48 65 6c 6c 6f ").unwrap(), b"Hello".to_vec());
    }

    #[test]
    fn empty_is_empty() {
        assert!(decode_hex("").unwrap().is_empty());
        assert!(decode_hex("   ").unwrap().is_empty());
        assert!(decode_hex("0x").unwrap().is_empty());
    }

    #[test]
    fn rejects_odd_length_and_non_hex() {
        assert!(decode_hex("abc").is_err());
        assert!(decode_hex("zz").is_err());
        assert!(decode_hex("de ad f").is_err());
    }
}
