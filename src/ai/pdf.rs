//! A tiny, dependency-free PDF writer for change reports.
//!
//! Generating a real PDF normally means a heavy crate or an external tool
//! (pandoc, headless Chrome). We only need to lay plain text onto pages, so we
//! emit a minimal but valid PDF by hand: monospaced Courier (one of PDF's
//! built-in base-14 fonts, so nothing to embed), hard-wrapped to a fixed column
//! count, paginated. The result opens in any PDF viewer.

use std::path::Path;

use anyhow::{Context, Result};

/// Characters per line and lines per page, tuned for Courier 10pt on US Letter
/// (612×792 pt) with comfortable margins.
const COLS: usize = 80;
const ROWS_PER_PAGE: usize = 56;

/// Write a change report to `path` as a PDF: a title, timestamp, the assistant's
/// summary, and (when non-empty) the raw git activity as an appendix.
pub fn write_report(path: &Path, days: u64, summary: &str, activity: &str) -> Result<()> {
    let mut lines: Vec<String> = Vec::new();
    push_wrapped(
        &mut lines,
        &format!("Repository change report — past {days} day(s)"),
    );
    push_wrapped(
        &mut lines,
        &format!(
            "Generated {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        ),
    );
    lines.push(String::new());
    lines.push("== Summary ==".to_string());
    for line in summary.lines() {
        push_wrapped(&mut lines, line);
    }
    if !activity.trim().is_empty() {
        lines.push(String::new());
        lines.push("== Git activity ==".to_string());
        for line in activity.lines() {
            push_wrapped(&mut lines, line);
        }
    }

    let bytes = build_pdf(&lines);
    std::fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

/// Hard-wrap one logical line to `COLS` columns, pushing each piece. Non-ASCII
/// and control characters are handled at render time; here we only split.
fn push_wrapped(out: &mut Vec<String>, line: &str) {
    let line = line.replace('\t', "    ");
    if line.is_empty() {
        out.push(String::new());
        return;
    }
    let chars: Vec<char> = line.chars().collect();
    for chunk in chars.chunks(COLS) {
        out.push(chunk.iter().collect());
    }
}

/// Assemble the PDF byte stream from already-wrapped text lines.
fn build_pdf(text_lines: &[String]) -> Vec<u8> {
    let pages: Vec<&[String]> = if text_lines.is_empty() {
        vec![&[]]
    } else {
        text_lines.chunks(ROWS_PER_PAGE).collect()
    };
    let page_count = pages.len();

    // Object 1: catalog, 2: pages, 3: font, then a (page, content) pair each.
    let mut objects: Vec<String> = Vec::new();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
    let kids: String = (0..page_count)
        .map(|p| format!("{} 0 R", 4 + 2 * p))
        .collect::<Vec<_>>()
        .join(" ");
    objects.push(format!(
        "<< /Type /Pages /Kids [{kids}] /Count {page_count} >>"
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>".to_string());

    for page in &pages {
        let content_obj = 5 + 2 * objects_pages_so_far(&objects);
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 3 0 R >> >> /Contents {content_obj} 0 R >>"
        ));

        // Start near the top; T* steps down by the leading (TL) each line.
        let mut stream = String::from("BT\n/F1 10 Tf\n12 TL\n50 760 Td\n");
        for line in page.iter() {
            stream.push('(');
            stream.push_str(&escape(line));
            stream.push_str(") Tj\nT*\n");
        }
        stream.push_str("ET");
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
    }

    // Serialize with a cross-reference table.
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = vec![0usize; objects.len() + 1];
    for (i, body) in objects.iter().enumerate() {
        let num = i + 1;
        offsets[num] = out.len();
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body.as_bytes());
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = out.len();
    let size = objects.len() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for num in 1..size {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[num]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// How many page objects have been pushed so far (each page adds two objects,
/// after the first three fixed ones), so we can compute the next content ref.
fn objects_pages_so_far(objects: &[String]) -> usize {
    // objects: [catalog, pages, font, page0, content0, page1, content1, ...]
    objects.len().saturating_sub(3) / 2
}

/// Escape a line for a PDF literal string and drop anything that isn't printable
/// ASCII (Courier's default encoding), so the stream stays valid.
fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '(' => o.push_str("\\("),
            ')' => o.push_str("\\)"),
            c if (0x20..=0x7e).contains(&(c as u32)) => o.push(c),
            // Map the common non-ASCII punctuation that shows up in prose to a
            // sensible ASCII equivalent, so it reads right in Courier.
            '—' | '–' => o.push('-'),
            '“' | '”' | '„' => o.push('"'),
            '‘' | '’' | '‚' => o.push('\''),
            '…' => o.push_str("..."),
            '•' => o.push('*'),
            '→' => o.push_str("->"),
            _ => o.push('?'),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_valid_looking_pdf() {
        let dir = std::env::temp_dir().join(format!("ciabatta-pdf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.pdf");

        // A long summary forces multiple pages.
        let summary = (0..200)
            .map(|i| format!("change number {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        write_report(&path, 7, &summary, "abc123 fix: thing\n src/x.rs | 2 +-").unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF-1.4"), "missing PDF header");
        assert!(bytes.ends_with(b"%%EOF\n"), "missing EOF marker");
        // Header, xref, trailer present and multi-page.
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("startxref"));
        assert!(text.contains("/Count "));
        // Parentheses in content are escaped, not raw-broken.
        write_report(&path, 1, "value is (parenthesized) here", "").unwrap();
        let text2 = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).to_string();
        assert!(text2.contains("\\(parenthesized\\)"));

        // Common non-ASCII punctuation is mapped to ASCII, not dropped to '?'.
        assert_eq!(escape("a — b … c"), "a - b ... c");
        assert_eq!(escape("“q” ‘r’"), "\"q\" 'r'");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
