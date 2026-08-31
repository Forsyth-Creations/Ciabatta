//! JSON-RPC over stdio, with the Language Server Protocol's framing.
//!
//! The protocol is small enough that a dependency would cost more than it
//! saves: messages are `Content-Length: <n>\r\n\r\n<json>`, and the handful of
//! shapes this server speaks are read straight out of `serde_json::Value`
//! rather than through a generated model of the whole LSP surface. What we do
//! not understand, we ignore — which is also what the protocol asks of us.

use std::io::{BufRead, Read, Write};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// One decoded message from the client.
#[derive(Debug)]
pub struct Message {
    /// Present on requests, absent on notifications. Requests must be answered.
    pub id: Option<Value>,
    /// The method name, e.g. `textDocument/completion`.
    pub method: String,
    /// The `params` object, defaulted to `null` when the client sent none.
    pub params: Value,
}

impl Message {
    /// Whether this message expects a response.
    pub fn is_request(&self) -> bool {
        self.id.is_some()
    }
}

/// Read one LSP message from `input`, or `None` at clean end of stream.
///
/// Headers other than `Content-Length` are skipped: `Content-Type` is the only
/// other one the spec defines, and it has exactly one legal value.
pub fn read<R: BufRead>(input: &mut R) -> Result<Option<Message>> {
    let mut len: Option<usize> = None;

    loop {
        let mut line = String::new();
        if input.read_line(&mut line).context("reading LSP header")? == 0 {
            return Ok(None); // client closed the pipe
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break; // blank line ends the headers
        }
        if let Some(rest) = line
            .split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
            .map(|(_, v)| v)
        {
            len = Some(
                rest.trim()
                    .parse()
                    .context("Content-Length is not a number")?,
            );
        }
    }

    let Some(len) = len else {
        bail!("LSP message had no Content-Length header");
    };

    let mut buf = vec![0u8; len];
    input.read_exact(&mut buf).context("reading LSP body")?;
    let value: Value = serde_json::from_slice(&buf).context("LSP body is not JSON")?;

    let Some(method) = value.get("method").and_then(Value::as_str) else {
        // A response to something we sent. This server initiates no requests,
        // so there is nothing to correlate it with.
        return Ok(Some(Message {
            id: None,
            method: String::new(),
            params: Value::Null,
        }));
    };

    Ok(Some(Message {
        id: value.get("id").cloned(),
        method: method.to_string(),
        params: value.get("params").cloned().unwrap_or(Value::Null),
    }))
}

/// Write one message to `output`, framed as the protocol requires.
fn send<W: Write>(output: &mut W, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(&body)?;
    output.flush()?;
    Ok(())
}

/// Answer a request with its result.
pub fn respond<W: Write>(output: &mut W, id: &Value, result: Value) -> Result<()> {
    send(
        output,
        &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    )
}

/// Answer a request with an error. Used for the one case that can happen in
/// practice — a method we don't implement that the client still asked for.
pub fn respond_error<W: Write>(output: &mut W, id: &Value, code: i64, message: &str) -> Result<()> {
    send(
        output,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }),
    )
}

/// Send a notification the client did not ask for — diagnostics, log messages.
pub fn notify<W: Write>(output: &mut W, method: &str, params: Value) -> Result<()> {
    send(
        output,
        &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
    )
}

/// `MethodNotFound`, the one JSON-RPC error code this server returns.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// Turn a `file://` URI into a path.
///
/// Percent-decoding is the whole job: editors send `file:///home/me/my%20repo`.
/// Windows' `file:///C:/…` form drops its leading slash.
pub fn uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // An authority component (`file://host/path`) is not something an editor
    // sends for a local file; everything up to the next `/` is empty.
    let rest = rest
        .strip_prefix('/')
        .map(|r| format!("/{r}"))
        .unwrap_or_else(|| rest.to_string());

    let bytes = rest.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }

    let decoded = String::from_utf8(out).ok()?;
    // `file:///C:/x` — a drive letter means the leading slash is framing.
    let decoded = match decoded.strip_prefix('/') {
        Some(tail) if tail.len() > 2 && tail.as_bytes()[1] == b':' => tail.to_string(),
        _ => decoded,
    };
    Some(std::path::PathBuf::from(decoded))
}

/// Drain and discard stdin. Used when the client sends `exit` without a
/// preceding `shutdown` and we want to leave the pipe in a sane state.
pub fn drain<R: Read>(input: &mut R) {
    let _ = std::io::copy(input, &mut std::io::sink());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_a_framed_request() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"x":2}}"#;
        let raw = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let msg = read(&mut Cursor::new(raw)).unwrap().unwrap();
        assert_eq!(msg.method, "initialize");
        assert!(msg.is_request());
        assert_eq!(msg.params["x"], 2);
    }

    #[test]
    fn tolerates_extra_headers_and_notifications() {
        let body = r#"{"jsonrpc":"2.0","method":"exit"}"#;
        let raw = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let msg = read(&mut Cursor::new(raw)).unwrap().unwrap();
        assert_eq!(msg.method, "exit");
        assert!(!msg.is_request());
    }

    #[test]
    fn end_of_stream_is_not_an_error() {
        assert!(read(&mut Cursor::new("")).unwrap().is_none());
    }

    #[test]
    fn decodes_percent_escapes_in_uris() {
        assert_eq!(
            uri_to_path("file:///home/me/my%20repo/.ciabatta/ciabatta.yaml").unwrap(),
            std::path::PathBuf::from("/home/me/my repo/.ciabatta/ciabatta.yaml")
        );
    }
}
