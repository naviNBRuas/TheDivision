//! Hand-rolled LSP wire framing: `Content-Length: <n>\r\n\r\n<n bytes of JSON>`,
//! read/written on both sides of this proxy (Claude Code ↔ proxy, and
//! proxy ↔ each real backend language server) — the same framing every
//! LSP implementation uses. Kept as raw `serde_json::Value` rather than
//! typed `lsp-types` structs: this proxy only needs to read `id` and
//! `params.textDocument.uri` to route, never the full message shape, so a
//! typed dependency would buy nothing here.

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, Write};

/// Reads one framed message, or `Ok(None)` at a clean end of stream.
pub fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).context("reading LSP header line")?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let line = line.trim_end();
        if line.is_empty() {
            break; // blank line ends the header block
        }
        // Split on the first colon rather than matching `"Content-Length: "`
        // verbatim, so a sender that omits the space (or varies the header's
        // case) is still understood. Any other header — `Content-Type` is the
        // only one in practice — is read and discarded; this proxy never sends
        // one.
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = Some(value.trim().parse().context("parsing Content-Length")?);
            }
        }
    }
    let content_length = content_length.context("LSP message had no Content-Length header")?;
    let mut buf = vec![0u8; content_length];
    reader.read_exact(&mut buf).context("reading LSP message body")?;
    let value: Value = serde_json::from_slice(&buf).context("parsing LSP message body as JSON")?;
    Ok(Some(value))
}

/// Writes one framed message and flushes it, so the peer sees a whole
/// message rather than a header stranded in a buffer.
pub fn write_message<W: Write>(writer: &mut W, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message).context("serializing LSP message")?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len()).context("writing LSP header")?;
    writer.write_all(&body).context("writing LSP message body")?;
    writer.flush().context("flushing LSP message")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::BufReader;

    #[test]
    fn round_trips_a_message() {
        let mut buf: Vec<u8> = Vec::new();
        let message = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
        write_message(&mut buf, &message).unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let read_back = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(read_back, message);
    }

    #[test]
    fn round_trips_back_to_back_messages() {
        let mut buf: Vec<u8> = Vec::new();
        let first = json!({ "id": 1, "method": "a" });
        let second = json!({ "id": 2, "method": "b" });
        write_message(&mut buf, &first).unwrap();
        write_message(&mut buf, &second).unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        assert_eq!(read_message(&mut reader).unwrap().unwrap(), first);
        assert_eq!(read_message(&mut reader).unwrap().unwrap(), second);
        assert!(read_message(&mut reader).unwrap().is_none());
    }

    #[test]
    fn returns_none_at_eof() {
        let mut reader = BufReader::new([].as_slice());
        assert!(read_message(&mut reader).unwrap().is_none());
    }

    #[test]
    fn ignores_unrelated_headers() {
        let mut buf: Vec<u8> = Vec::new();
        let body = serde_json::to_vec(&json!({ "ok": true })).unwrap();
        write!(
            &mut buf,
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .unwrap();
        buf.extend_from_slice(&body);

        let mut reader = BufReader::new(buf.as_slice());
        let read_back = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(read_back, json!({ "ok": true }));
    }

    #[test]
    fn accepts_header_without_a_space_after_the_colon() {
        let mut buf: Vec<u8> = Vec::new();
        let body = serde_json::to_vec(&json!({ "ok": true })).unwrap();
        write!(&mut buf, "content-length:{}\r\n\r\n", body.len()).unwrap();
        buf.extend_from_slice(&body);

        let mut reader = BufReader::new(buf.as_slice());
        assert_eq!(read_message(&mut reader).unwrap().unwrap(), json!({ "ok": true }));
    }

    #[test]
    fn errors_when_content_length_is_missing() {
        let mut reader = BufReader::new("Content-Type: text/plain\r\n\r\n{}".as_bytes());
        assert!(read_message(&mut reader).is_err());
    }
}
