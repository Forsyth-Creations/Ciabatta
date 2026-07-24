//! Man-in-the-middle capture of a serial conversation, plus replay and Rust
//! export of what was captured.
//!
//! A single physical UART can't be tapped in software, so this works as a
//! relay: [`CaptureManager::start`] opens two ports — the real `device_port`
//! and `app_port`, ciabatta's end of a virtual null-modem pair (e.g. com0com
//! on Windows, socat/tty0tty on Linux) — and forwards every byte between them
//! on two background threads, logging each direction with a timestamp
//! relative to the capture's start.
//!
//! `app_port` is **not** the port the third-party tool connects to — a
//! serial port only accepts one open handle at a time, and ciabatta needs
//! exclusive access to `app_port` for the duration of the capture. The
//! third-party tool must instead be pointed at the *other* port of the
//! virtual pair (e.g. com0com creates two linked port names, such as
//! `COM8`/`COM9`; ciabatta opens one, the third-party tool opens the other).
//!
//! Finished captures are persisted as JSON under
//! `<project root>/.ciabatta/usb/captures/<id>.json`, one file per session,
//! mirroring how `ciabatta ai` stores conversations
//! (see [`crate::ai::session`]).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serialport::SerialPort;

use super::WRITE_TIMEOUT;

/// How long a relay thread's blocking read may wait before it re-checks
/// whether the capture has been stopped.
const RELAY_READ_TIMEOUT: Duration = Duration::from_millis(100);

/// Which side of the relay a [`Frame`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Bytes the third-party app sent, on their way to the real device.
    AppToDevice,
    /// Bytes the real device sent, on their way to the third-party app.
    DeviceToApp,
}

/// One relayed chunk of bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    /// Milliseconds since the capture started.
    pub t_ms: u64,
    pub dir: Direction,
    #[serde(with = "hex_data")]
    pub data: Vec<u8>,
}

/// A finished, persisted capture session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureSession {
    pub id: String,
    pub created_at: String,
    pub device_port: String,
    pub app_port: String,
    pub baud: u32,
    #[serde(default)]
    pub frames: Vec<Frame>,
    /// Absolute path this session persists to (not serialized).
    #[serde(skip)]
    path: PathBuf,
}

/// A one-line summary of a saved capture, for listings/pickers.
pub struct CaptureSummary {
    pub id: String,
    pub created_at: String,
    pub device_port: String,
    pub app_port: String,
    pub baud: u32,
    pub frame_count: usize,
}

impl CaptureSession {
    /// Load a saved capture by id from a project's store.
    pub fn load(root: &Path, id: &str) -> Result<Self> {
        let path = captures_dir(root).join(format!("{id}.json"));
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("No saved capture '{id}' ({})", path.display()))?;
        let mut session: CaptureSession = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        session.path = path;
        Ok(session)
    }

    /// Persist the session to disk.
    pub fn save(&self) -> Result<()> {
        let dir = self
            .path
            .parent()
            .context("capture path has no parent directory")?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("Failed to write {}", self.path.display()))
    }
}

/// Directory holding every saved capture for a project.
pub fn captures_dir(root: &Path) -> PathBuf {
    root.join(crate::config::CIABATTA_DIR)
        .join("usb")
        .join("captures")
}

/// Every saved capture for a project, newest first.
pub fn list(root: &Path) -> Result<Vec<CaptureSummary>> {
    let dir = captures_dir(root);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", dir.display())),
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(session) = serde_json::from_str::<CaptureSession>(&raw) else {
            continue;
        };
        out.push(CaptureSummary {
            id: session.id,
            created_at: session.created_at,
            device_port: session.device_port,
            app_port: session.app_port,
            baud: session.baud,
            frame_count: session.frames.len(),
        });
    }
    // created_at is an RFC3339 string; lexical sort is chronological.
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

/// Delete one saved capture by id. Returns whether a file was removed.
pub fn delete(root: &Path, id: &str) -> Result<bool> {
    let path = captures_dir(root).join(format!("{id}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("Failed to delete {}", path.display())),
    }
}

/// The frames of `session` on direction `dir`, optionally restricted to
/// specific frame indices (indices are into `session.frames`, unfiltered).
pub fn select_frames<'a>(
    session: &'a CaptureSession,
    dir: Direction,
    indices: Option<&[usize]>,
) -> Vec<&'a Frame> {
    match indices {
        Some(idx) => idx
            .iter()
            .filter_map(|&i| session.frames.get(i))
            .filter(|f| f.dir == dir)
            .collect(),
        None => session.frames.iter().filter(|f| f.dir == dir).collect(),
    }
}

/// Write `frames`' bytes to `port` at `baud`, in order. When `honor_timing`
/// is set, sleeps between writes for the delay recorded between consecutive
/// frames (the first frame is sent immediately). Returns the total bytes
/// written.
pub fn replay(port: &str, baud: u32, frames: &[Frame], honor_timing: bool) -> Result<usize> {
    if frames.is_empty() {
        bail!("No frames selected to replay.");
    }

    let mut handle = serialport::new(port, baud)
        .timeout(WRITE_TIMEOUT)
        .open()
        .map_err(|e| anyhow!("Could not open {port} at {baud} baud: {e}"))?;

    let mut total = 0usize;
    let mut last_t = frames[0].t_ms;
    for f in frames {
        if honor_timing {
            let delta = f.t_ms.saturating_sub(last_t);
            if delta > 0 {
                std::thread::sleep(Duration::from_millis(delta));
            }
        }
        last_t = f.t_ms;

        handle
            .write_all(&f.data)
            .map_err(|e| anyhow!("Write to {port} failed: {e}"))?;
        handle
            .flush()
            .map_err(|e| anyhow!("Flushing {port} failed: {e}"))?;
        total += f.data.len();
    }
    Ok(total)
}

/// Render the frames on `dir` (optionally restricted to `indices`) as a
/// self-contained `.rs` snippet: a `FRAMES` byte/delay table plus a
/// `replay()` function using the `serialport` crate. Meant to be pasted
/// directly into another Rust project.
pub fn export_rust(session: &CaptureSession, dir: Direction, indices: Option<&[usize]>) -> String {
    let selected = select_frames(session, dir, indices);

    let dir_label = match dir {
        Direction::AppToDevice => "app_to_device",
        Direction::DeviceToApp => "device_to_app",
    };

    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! Captured from a `ciabatta usb capture` session ({}, {}).",
        session.id, session.created_at
    );
    let _ = writeln!(
        out,
        "//! device_port={} app_port={} baud={} direction={}",
        session.device_port, session.app_port, session.baud, dir_label
    );
    out.push_str(
        "//! Paste into your project; requires the `serialport` crate (e.g. `serialport = \"4\"`).\n\n",
    );
    out.push_str("use std::time::Duration;\n\n");
    out.push_str("/// (delay before this frame, in ms; raw bytes)\n");
    out.push_str("pub const FRAMES: &[(u64, &[u8])] = &[\n");

    let mut last_t = selected.first().map(|f| f.t_ms).unwrap_or(0);
    for f in &selected {
        let delay = f.t_ms.saturating_sub(last_t);
        last_t = f.t_ms;
        let bytes = f
            .data
            .iter()
            .map(|b| format!("0x{b:02X}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "    ({delay}, &[{bytes}]),");
    }
    out.push_str("];\n\n");
    out.push_str(
        "pub fn replay(port: &str, baud: u32) -> Result<(), Box<dyn std::error::Error>> {\n",
    );
    out.push_str("    let mut handle = serialport::new(port, baud)\n");
    out.push_str("        .timeout(Duration::from_secs(5))\n");
    out.push_str("        .open()?;\n");
    out.push_str("    for (delay_ms, bytes) in FRAMES {\n");
    out.push_str(
        "        if *delay_ms > 0 { std::thread::sleep(Duration::from_millis(*delay_ms)); }\n",
    );
    out.push_str("        handle.write_all(bytes)?;\n");
    out.push_str("        handle.flush()?;\n");
    out.push_str("    }\n");
    out.push_str("    Ok(())\n");
    out.push_str("}\n");
    out
}

/// A live snapshot of a running capture, for the polling UI.
pub struct StatusSnapshot {
    pub frame_count: usize,
    pub app_to_device_bytes: usize,
    pub device_to_app_bytes: usize,
    pub elapsed_ms: u64,
    /// The most recent frames (newest last), capped for a light response.
    pub recent: Vec<Frame>,
}

/// The most recent frames to include in a [`StatusSnapshot`].
const STATUS_RECENT_LIMIT: usize = 50;

struct RunningCapture {
    created_at: String,
    device_port: String,
    app_port: String,
    baud: u32,
    frames: Arc<Mutex<Vec<Frame>>>,
    stop_flag: Arc<AtomicBool>,
    started: Instant,
    handles: Vec<JoinHandle<()>>,
}

/// Shared server state: every currently-running capture, keyed by id.
#[derive(Default)]
pub struct CaptureManager {
    running: Mutex<HashMap<String, RunningCapture>>,
}

impl CaptureManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open `device_port` and `app_port` at `baud` and start relaying bytes
    /// between them on two background threads, logging every chunk. Returns
    /// the new capture's id.
    ///
    /// `app_port` must be ciabatta's own end of a virtual null-modem pair,
    /// not the port the third-party tool connects to (see the module docs).
    pub fn start(&self, device_port: &str, app_port: &str, baud: u32) -> Result<String> {
        let device_read = serialport::new(device_port, baud)
            .timeout(RELAY_READ_TIMEOUT)
            .open()
            .map_err(|e| anyhow!("Could not open device port {device_port} at {baud} baud: {e}"))?;
        let app_read = serialport::new(app_port, baud)
            .timeout(RELAY_READ_TIMEOUT)
            .open()
            .map_err(|e| anyhow!("Could not open app port {app_port} at {baud} baud: {e}"))?;

        let device_write = device_read
            .try_clone()
            .map_err(|e| anyhow!("Could not open a second handle to {device_port}: {e}"))?;
        let app_write = app_read
            .try_clone()
            .map_err(|e| anyhow!("Could not open a second handle to {app_port}: {e}"))?;

        let id = new_id();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let stop_flag = Arc::new(AtomicBool::new(true));
        let started = Instant::now();

        let h1 = spawn_relay(
            device_read,
            app_write,
            Direction::DeviceToApp,
            frames.clone(),
            stop_flag.clone(),
            started,
        );
        let h2 = spawn_relay(
            app_read,
            device_write,
            Direction::AppToDevice,
            frames.clone(),
            stop_flag.clone(),
            started,
        );

        self.running.lock().unwrap().insert(
            id.clone(),
            RunningCapture {
                created_at: now(),
                device_port: device_port.to_string(),
                app_port: app_port.to_string(),
                baud,
                frames,
                stop_flag,
                started,
                handles: vec![h1, h2],
            },
        );

        Ok(id)
    }

    /// A live snapshot of a running capture, or `None` if it's not running
    /// (never started, already stopped, or an unknown id).
    pub fn status(&self, id: &str) -> Option<StatusSnapshot> {
        let map = self.running.lock().unwrap();
        let running = map.get(id)?;
        let frames = running.frames.lock().unwrap();
        let app_to_device_bytes = frames
            .iter()
            .filter(|f| f.dir == Direction::AppToDevice)
            .map(|f| f.data.len())
            .sum();
        let device_to_app_bytes = frames
            .iter()
            .filter(|f| f.dir == Direction::DeviceToApp)
            .map(|f| f.data.len())
            .sum();
        let recent = frames
            .iter()
            .rev()
            .take(STATUS_RECENT_LIMIT)
            .rev()
            .cloned()
            .collect();
        Some(StatusSnapshot {
            frame_count: frames.len(),
            app_to_device_bytes,
            device_to_app_bytes,
            elapsed_ms: running.started.elapsed().as_millis() as u64,
            recent,
        })
    }

    /// Stop a running capture, sort its frames chronologically, persist it
    /// under `root`, and return the finished session.
    pub fn stop(&self, root: &Path, id: &str) -> Result<CaptureSession> {
        let running = {
            let mut map = self.running.lock().unwrap();
            map.remove(id)
                .ok_or_else(|| anyhow!("No running capture '{id}'"))?
        };

        running.stop_flag.store(false, Ordering::Relaxed);
        for h in running.handles {
            let _ = h.join();
        }

        let frames = finalize_frames(running.frames.lock().unwrap().clone());

        let session = CaptureSession {
            id: id.to_string(),
            created_at: running.created_at,
            device_port: running.device_port,
            app_port: running.app_port,
            baud: running.baud,
            frames,
            path: captures_dir(root).join(format!("{id}.json")),
        };
        session.save()?;
        Ok(session)
    }

    /// Best-effort: stop every running capture (e.g. on server shutdown).
    pub fn stop_all(&self, root: &Path) {
        let ids: Vec<String> = self.running.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let _ = self.stop(root, &id);
        }
    }
}

/// Sort captured frames into chronological order. The two relay threads push
/// into the same list independently, so their arrival order isn't guaranteed
/// to match `t_ms` order.
fn finalize_frames(mut frames: Vec<Frame>) -> Vec<Frame> {
    frames.sort_by_key(|f| f.t_ms);
    frames
}

/// One relay direction: block-read from `reader`, forward every chunk to
/// `writer`, and log it as a timestamped [`Frame`]. Runs until `stop_flag`
/// is cleared (checked whenever a read times out) or either side errors.
fn spawn_relay(
    mut reader: Box<dyn SerialPort>,
    mut writer: Box<dyn SerialPort>,
    dir: Direction,
    frames: Arc<Mutex<Vec<Frame>>>,
    stop_flag: Arc<AtomicBool>,
    started: Instant,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while stop_flag.load(Ordering::Relaxed) {
            match reader.read(&mut buf) {
                Ok(0) => continue,
                Ok(n) => {
                    let data = buf[..n].to_vec();
                    if writer.write_all(&data).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                    frames.lock().unwrap().push(Frame {
                        t_ms: started.elapsed().as_millis() as u64,
                        dir,
                        data,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(_) => break,
            }
        }
    })
}

fn new_id() -> String {
    now().replace([':', '.'], "-")
}

fn now() -> String {
    chrono::Local::now().to_rfc3339()
}

/// (De)serialize `Vec<u8>` as a lowercase hex string, matching the rest of
/// `usb`'s hex convention (see [`super::decode_hex`]/[`super::encode_hex`]).
mod hex_data {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::super::encode_hex(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        super::super::decode_hex(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ciabatta-usb-capture-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn frame(t_ms: u64, dir: Direction, data: &[u8]) -> Frame {
        Frame {
            t_ms,
            dir,
            data: data.to_vec(),
        }
    }

    #[test]
    fn frame_hex_round_trips_through_json() {
        let f = frame(5, Direction::AppToDevice, &[0xde, 0xad, 0x00]);
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"dead00\""));
        assert!(json.contains("\"app_to_device\""));
        let back: Frame = serde_json::from_str(&json).unwrap();
        assert_eq!(back.data, vec![0xde, 0xad, 0x00]);
        assert_eq!(back.dir, Direction::AppToDevice);
    }

    #[test]
    fn finalize_frames_sorts_out_of_order_pushes() {
        let frames = vec![
            frame(30, Direction::DeviceToApp, &[3]),
            frame(0, Direction::AppToDevice, &[1]),
            frame(15, Direction::DeviceToApp, &[2]),
        ];
        let sorted = finalize_frames(frames);
        assert_eq!(
            sorted.iter().map(|f| f.t_ms).collect::<Vec<_>>(),
            vec![0, 15, 30]
        );
    }

    #[test]
    fn capture_session_save_load_and_list_round_trip() {
        let root = temp_root();

        let session = CaptureSession {
            id: "20260723-abc".to_string(),
            created_at: chrono::Local::now().to_rfc3339(),
            device_port: "COM5".to_string(),
            app_port: "COM7".to_string(),
            baud: 115200,
            frames: vec![
                frame(0, Direction::AppToDevice, &[0xaa, 0x01]),
                frame(10, Direction::DeviceToApp, &[0x55]),
            ],
            path: captures_dir(&root).join("20260723-abc.json"),
        };
        session.save().unwrap();

        let loaded = CaptureSession::load(&root, "20260723-abc").unwrap();
        assert_eq!(loaded.frames.len(), 2);
        assert_eq!(loaded.device_port, "COM5");
        assert_eq!(loaded.frames[1].data, vec![0x55]);

        let summaries = list(&root).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, "20260723-abc");
        assert_eq!(summaries[0].frame_count, 2);

        assert!(delete(&root, "20260723-abc").unwrap());
        assert!(list(&root).unwrap().is_empty());
        assert!(!delete(&root, "20260723-abc").unwrap());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn select_frames_filters_by_direction_and_indices() {
        let session = CaptureSession {
            id: "x".into(),
            created_at: "x".into(),
            device_port: "COM1".into(),
            app_port: "COM2".into(),
            baud: 9600,
            frames: vec![
                frame(0, Direction::AppToDevice, &[1]),
                frame(1, Direction::DeviceToApp, &[2]),
                frame(2, Direction::AppToDevice, &[3]),
            ],
            path: PathBuf::new(),
        };

        let all_app = select_frames(&session, Direction::AppToDevice, None);
        assert_eq!(all_app.len(), 2);

        let just_first = select_frames(&session, Direction::AppToDevice, Some(&[0]));
        assert_eq!(just_first.len(), 1);
        assert_eq!(just_first[0].data, vec![1]);

        // Index 1 is a DeviceToApp frame, so filtering for AppToDevice drops it.
        let mismatched = select_frames(&session, Direction::AppToDevice, Some(&[1]));
        assert!(mismatched.is_empty());
    }

    #[test]
    fn export_rust_includes_only_selected_direction() {
        let session = CaptureSession {
            id: "cap1".into(),
            created_at: "2026-07-23T00:00:00Z".into(),
            device_port: "COM5".into(),
            app_port: "COM7".into(),
            baud: 115200,
            frames: vec![
                frame(0, Direction::AppToDevice, &[0xaa]),
                frame(5, Direction::DeviceToApp, &[0xbb]),
                frame(20, Direction::AppToDevice, &[0xcc]),
            ],
            path: PathBuf::new(),
        };

        let code = export_rust(&session, Direction::AppToDevice, None);
        assert!(
            code.contains("device_port=COM5 app_port=COM7 baud=115200 direction=app_to_device")
        );
        assert!(code.contains("0xAA"));
        assert!(code.contains("0xCC"));
        assert!(!code.contains("0xBB"));
        assert!(code.contains("pub fn replay(port: &str, baud: u32)"));
        // First selected frame always has a zero delay, second retains its
        // gap from the first *selected* frame (20 - 0 = 20), not the raw t_ms
        // of the frame in between that got filtered out.
        assert!(code.contains("(0, &[0xAA]),"));
        assert!(code.contains("(20, &[0xCC]),"));
    }

    #[test]
    fn replay_rejects_empty_selection_without_touching_a_port() {
        let err = replay("definitely-not-a-real-port", 9600, &[], true).unwrap_err();
        assert!(err.to_string().contains("No frames selected"));
    }
}
