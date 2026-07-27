//! "Check" mode: validate that a freshly-captured session matches an earlier
//! one, and show a byte-level diff when it doesn't.
//!
//! The workflow this backs is: sniff a command once with
//! [`super::capture`], then re-run the *same* command through the MITM relay
//! with the first capture pinned as the **baseline**. When the new capture is
//! stopped, the two are compared and the user is told either "identical" or
//! exactly where they diverged.
//!
//! Comparison is done on the **concatenated byte stream per direction**, not
//! frame by frame. Where a serial driver splits a burst into read chunks is an
//! artifact of timing and buffer sizes — the same command replayed twice can
//! easily produce a different number of [`Frame`]s with identical bytes — so
//! frame boundaries are deliberately ignored. Timing is ignored for the same
//! reason.

use serde::Serialize;

use super::capture::{CaptureSession, Direction};

/// Largest LCS table (in cells) worth building. Beyond this the diff falls
/// back to reporting the whole differing region as one removal + one addition,
/// which is still correct, just less precise. 2M cells ≈ 8 MB of `u32`s.
const LCS_CELL_LIMIT: usize = 2_000_000;

/// Most ops returned for one direction. A wildly different pair of captures
/// can otherwise produce a diff with an op per byte, which is neither useful
/// nor cheap to ship to the browser.
const MAX_OPS: usize = 400;

/// Most bytes carried per op. Longer runs report their true `len` and send
/// only this much data, so the UI can render a head and say how much it
/// elided.
const MAX_OP_BYTES: usize = 2048;

/// What a [`DiffOp`] says about a run of bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    /// Present in both streams.
    Equal,
    /// In the baseline but not in the new run.
    Removed,
    /// In the new run but not in the baseline.
    Added,
}

/// One run of bytes in a direction's diff.
#[derive(Debug, Clone, Serialize)]
pub struct DiffOp {
    pub kind: OpKind,
    /// Byte offset of this run in the baseline stream.
    pub baseline_offset: usize,
    /// Byte offset of this run in the new run's stream.
    pub actual_offset: usize,
    /// The run's true length, even when `data` was truncated for transport.
    pub len: usize,
    /// The run's bytes, capped at [`MAX_OP_BYTES`].
    #[serde(with = "super::capture::hex_data")]
    pub data: Vec<u8>,
}

/// The result of comparing one direction of two captures.
#[derive(Debug, Clone, Serialize)]
pub struct DirectionDiff {
    pub dir: Direction,
    /// Whether the two byte streams are identical.
    pub matches: bool,
    pub baseline_len: usize,
    pub actual_len: usize,
    /// Byte offset of the first divergence, or `None` when they match.
    pub first_diff_offset: Option<usize>,
    /// The diff itself (a single `Equal` op when the streams match).
    pub ops: Vec<DiffOp>,
    /// The differing region was too large to align precisely, so it's reported
    /// as one bulk removal + addition instead of an interleaved diff.
    pub coarse: bool,
    /// `ops` was cut off at [`MAX_OPS`]; the diff continues past the last one.
    pub ops_truncated: bool,
}

/// The result of comparing two captures across one or more directions.
#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    pub baseline_id: String,
    pub actual_id: String,
    /// True only when every compared direction matched.
    pub matches: bool,
    pub directions: Vec<DirectionDiff>,
}

/// Every byte `session` carried on `dir`, in order, with frame boundaries
/// dissolved (see the module docs for why).
pub fn stream(session: &CaptureSession, dir: Direction) -> Vec<u8> {
    session
        .frames
        .iter()
        .filter(|f| f.dir == dir)
        .flat_map(|f| f.data.iter().copied())
        .collect()
}

/// Compare `actual` against `baseline` on each of `dirs`.
pub fn compare(
    baseline: &CaptureSession,
    actual: &CaptureSession,
    dirs: &[Direction],
) -> Comparison {
    let directions: Vec<DirectionDiff> = dirs
        .iter()
        .map(|&dir| diff_direction(baseline, actual, dir))
        .collect();

    Comparison {
        baseline_id: baseline.id.clone(),
        actual_id: actual.id.clone(),
        matches: directions.iter().all(|d| d.matches),
        directions,
    }
}

/// Compare one direction's byte streams.
fn diff_direction(
    baseline: &CaptureSession,
    actual: &CaptureSession,
    dir: Direction,
) -> DirectionDiff {
    let a = stream(baseline, dir);
    let b = stream(actual, dir);
    let (mut ops, coarse) = diff_bytes(&a, &b);

    let first_diff_offset = ops
        .iter()
        .find(|o| o.kind != OpKind::Equal)
        .map(|o| o.baseline_offset);

    let ops_truncated = ops.len() > MAX_OPS;
    ops.truncate(MAX_OPS);
    for op in &mut ops {
        op.data.truncate(MAX_OP_BYTES);
    }

    DirectionDiff {
        dir,
        matches: a == b,
        baseline_len: a.len(),
        actual_len: b.len(),
        first_diff_offset,
        ops,
        coarse,
        ops_truncated,
    }
}

/// Diff two byte streams. Returns the ops and whether the differing region had
/// to be reported coarsely (see [`LCS_CELL_LIMIT`]).
///
/// A shared prefix and suffix are peeled off first, which makes the common
/// "identical except for one field in the middle" case both exact and cheap.
fn diff_bytes(a: &[u8], b: &[u8]) -> (Vec<DiffOp>, bool) {
    let prefix = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    // Don't let the suffix scan run back past the prefix into bytes already
    // claimed, which would double-count them in short/repetitive streams.
    let remaining = a.len().min(b.len()) - prefix;
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count()
        .min(remaining);

    let a_mid = &a[prefix..a.len() - suffix];
    let b_mid = &b[prefix..b.len() - suffix];

    let mut raw: Vec<(OpKind, Vec<u8>)> = Vec::new();
    if prefix > 0 {
        raw.push((OpKind::Equal, a[..prefix].to_vec()));
    }

    let mut coarse = false;
    if a_mid.is_empty() && b_mid.is_empty() {
        // Fully covered by the prefix/suffix.
    } else if a_mid.is_empty() {
        raw.push((OpKind::Added, b_mid.to_vec()));
    } else if b_mid.is_empty() {
        raw.push((OpKind::Removed, a_mid.to_vec()));
    } else if a_mid.len().saturating_mul(b_mid.len()) <= LCS_CELL_LIMIT {
        raw.extend(lcs_ops(a_mid, b_mid));
    } else {
        coarse = true;
        raw.push((OpKind::Removed, a_mid.to_vec()));
        raw.push((OpKind::Added, b_mid.to_vec()));
    }

    if suffix > 0 {
        raw.push((OpKind::Equal, a[a.len() - suffix..].to_vec()));
    }

    (coalesce(raw), coarse)
}

/// A longest-common-subsequence alignment of two (already prefix/suffix
/// trimmed) byte slices, emitted one byte at a time — [`coalesce`] turns the
/// per-byte decisions back into runs.
fn lcs_ops(a: &[u8], b: &[u8]) -> Vec<(OpKind, Vec<u8>)> {
    let (n, m) = (a.len(), b.len());
    let w = m + 1;

    // table[i][j] = length of the LCS of a[i..] and b[j..].
    let mut table = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * w + j] = if a[i] == b[j] {
                table[(i + 1) * w + j + 1] + 1
            } else {
                table[(i + 1) * w + j].max(table[i * w + j + 1])
            };
        }
    }

    let mut ops = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push((OpKind::Equal, vec![a[i]]));
            i += 1;
            j += 1;
        } else if table[(i + 1) * w + j] >= table[i * w + j + 1] {
            ops.push((OpKind::Removed, vec![a[i]]));
            i += 1;
        } else {
            ops.push((OpKind::Added, vec![b[j]]));
            j += 1;
        }
    }
    if i < n {
        ops.push((OpKind::Removed, a[i..].to_vec()));
    }
    if j < m {
        ops.push((OpKind::Added, b[j..].to_vec()));
    }
    ops
}

/// Merge adjacent same-kind runs and stamp each resulting op with its offset
/// in both streams.
fn coalesce(raw: Vec<(OpKind, Vec<u8>)>) -> Vec<DiffOp> {
    let mut merged: Vec<(OpKind, Vec<u8>)> = Vec::new();
    for (kind, data) in raw {
        if data.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some((k, d)) if *k == kind => d.extend_from_slice(&data),
            _ => merged.push((kind, data)),
        }
    }

    let mut ops = Vec::with_capacity(merged.len());
    let (mut baseline_offset, mut actual_offset) = (0usize, 0usize);
    for (kind, data) in merged {
        let len = data.len();
        ops.push(DiffOp {
            kind,
            baseline_offset,
            actual_offset,
            len,
            data,
        });
        match kind {
            OpKind::Equal => {
                baseline_offset += len;
                actual_offset += len;
            }
            OpKind::Removed => baseline_offset += len,
            OpKind::Added => actual_offset += len,
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usb::capture::Frame;
    use std::path::PathBuf;

    fn session(id: &str, frames: Vec<(Direction, &[u8])>) -> CaptureSession {
        CaptureSession {
            id: id.to_string(),
            created_at: "2026-07-26T00:00:00Z".to_string(),
            device_port: "COM5".to_string(),
            app_port: "COM7".to_string(),
            baud: 115200,
            baseline_id: None,
            frames: frames
                .into_iter()
                .enumerate()
                .map(|(i, (dir, data))| Frame {
                    t_ms: i as u64 * 10,
                    dir,
                    data: data.to_vec(),
                })
                .collect(),
            path: PathBuf::new(),
        }
    }

    /// Rebuild both streams from a diff; any correct diff must reproduce them.
    fn rebuild(ops: &[DiffOp]) -> (Vec<u8>, Vec<u8>) {
        let mut a = Vec::new();
        let mut b = Vec::new();
        for op in ops {
            match op.kind {
                OpKind::Equal => {
                    a.extend_from_slice(&op.data);
                    b.extend_from_slice(&op.data);
                }
                OpKind::Removed => a.extend_from_slice(&op.data),
                OpKind::Added => b.extend_from_slice(&op.data),
            }
        }
        (a, b)
    }

    #[test]
    fn identical_streams_match_with_one_equal_op() {
        let (ops, coarse) = diff_bytes(&[1, 2, 3, 4], &[1, 2, 3, 4]);
        assert!(!coarse);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].kind, OpKind::Equal);
        assert_eq!(ops[0].data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn frame_boundaries_do_not_affect_the_comparison() {
        // Same bytes, chunked differently by the serial driver.
        let baseline = session(
            "base",
            vec![
                (Direction::AppToDevice, &[0xaa, 0x01, 0x02]),
                (Direction::AppToDevice, &[0x03]),
            ],
        );
        let actual = session(
            "run",
            vec![
                (Direction::AppToDevice, &[0xaa]),
                (Direction::AppToDevice, &[0x01, 0x02, 0x03]),
            ],
        );

        let cmp = compare(&baseline, &actual, &[Direction::AppToDevice]);
        assert!(cmp.matches);
        assert_eq!(cmp.directions[0].first_diff_offset, None);
        assert_eq!(cmp.directions[0].baseline_len, 4);
    }

    #[test]
    fn a_changed_byte_in_the_middle_is_pinpointed() {
        let baseline = session("base", vec![(Direction::AppToDevice, &[1, 2, 3, 4, 5])]);
        let actual = session("run", vec![(Direction::AppToDevice, &[1, 2, 9, 4, 5])]);

        let cmp = compare(&baseline, &actual, &[Direction::AppToDevice]);
        assert!(!cmp.matches);

        let d = &cmp.directions[0];
        assert_eq!(d.first_diff_offset, Some(2));
        assert_eq!(d.baseline_len, 5);
        assert_eq!(d.actual_len, 5);

        let (rebuilt_baseline, rebuilt_actual) = rebuild(&d.ops);
        assert_eq!(rebuilt_baseline, vec![1, 2, 3, 4, 5]);
        assert_eq!(rebuilt_actual, vec![1, 2, 9, 4, 5]);
    }

    #[test]
    fn extra_trailing_bytes_report_as_an_addition() {
        let (ops, _) = diff_bytes(&[1, 2, 3], &[1, 2, 3, 4, 5]);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].kind, OpKind::Equal);
        assert_eq!(ops[1].kind, OpKind::Added);
        assert_eq!(ops[1].data, vec![4, 5]);
        assert_eq!(ops[1].baseline_offset, 3);
        assert_eq!(ops[1].actual_offset, 3);
    }

    #[test]
    fn missing_leading_bytes_report_as_a_removal() {
        let (ops, _) = diff_bytes(&[0xff, 1, 2], &[1, 2]);
        assert_eq!(ops[0].kind, OpKind::Removed);
        assert_eq!(ops[0].data, vec![0xff]);
        assert_eq!(ops[1].kind, OpKind::Equal);
        assert_eq!(ops[1].data, vec![1, 2]);
    }

    #[test]
    fn repeated_bytes_are_not_double_counted_by_prefix_and_suffix() {
        // Every byte is the same value, so a naive suffix scan would claim
        // bytes the prefix scan already took.
        let (ops, _) = diff_bytes(&[7, 7, 7], &[7, 7, 7, 7, 7]);
        let (a, b) = rebuild(&ops);
        assert_eq!(a, vec![7, 7, 7]);
        assert_eq!(b, vec![7, 7, 7, 7, 7]);
    }

    #[test]
    fn diff_always_reconstructs_both_streams() {
        let cases: [(&[u8], &[u8]); 6] = [
            (&[], &[]),
            (&[], &[1, 2, 3]),
            (&[1, 2, 3], &[]),
            (&[1, 2, 3, 4, 5, 6], &[1, 9, 3, 4, 7, 6]),
            (&[0xaa, 0xbb], &[0xcc, 0xdd, 0xee]),
            (&[1, 1, 2, 1, 1], &[1, 2, 1]),
        ];
        for (a, b) in cases {
            let (ops, _) = diff_bytes(a, b);
            let (ra, rb) = rebuild(&ops);
            assert_eq!(ra, a.to_vec(), "baseline mismatch for {a:?} vs {b:?}");
            assert_eq!(rb, b.to_vec(), "actual mismatch for {a:?} vs {b:?}");
        }
    }

    #[test]
    fn oversized_differing_regions_fall_back_to_a_coarse_diff() {
        // Shared first/last byte so the trimming runs, with a middle far past
        // the LCS budget on both sides.
        let side = LCS_CELL_LIMIT / 1000;
        let mut a = vec![0u8; side];
        let mut b = vec![1u8; side];
        a[0] = 0xaa;
        b[0] = 0xaa;

        let (ops, coarse) = diff_bytes(&a, &b);
        assert!(coarse);
        let (ra, rb) = rebuild(&ops);
        assert_eq!(ra, a);
        assert_eq!(rb, b);
    }

    #[test]
    fn directions_are_compared_independently() {
        let baseline = session(
            "base",
            vec![
                (Direction::AppToDevice, &[0x01]),
                (Direction::DeviceToApp, &[0x55]),
            ],
        );
        let actual = session(
            "run",
            vec![
                (Direction::AppToDevice, &[0x01]),
                (Direction::DeviceToApp, &[0x66]),
            ],
        );

        // The command the app sent is identical...
        let a2d = compare(&baseline, &actual, &[Direction::AppToDevice]);
        assert!(a2d.matches);

        // ...but the device answered differently, so checking both fails.
        let both = compare(
            &baseline,
            &actual,
            &[Direction::AppToDevice, Direction::DeviceToApp],
        );
        assert!(!both.matches);
        assert!(both.directions[0].matches);
        assert!(!both.directions[1].matches);
        assert_eq!(both.baseline_id, "base");
        assert_eq!(both.actual_id, "run");
    }

    #[test]
    fn long_runs_report_their_true_length_but_ship_capped_data() {
        let big: Vec<u8> = (0..MAX_OP_BYTES * 2).map(|i| i as u8).collect();
        let baseline = session("base", vec![(Direction::AppToDevice, &big)]);
        let actual = session("run", vec![(Direction::AppToDevice, &big)]);

        let cmp = compare(&baseline, &actual, &[Direction::AppToDevice]);
        let op = &cmp.directions[0].ops[0];
        assert_eq!(op.len, big.len());
        assert_eq!(op.data.len(), MAX_OP_BYTES);
    }
}
