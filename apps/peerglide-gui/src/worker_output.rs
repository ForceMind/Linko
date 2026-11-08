//! Bound pipe records before allocation; oversized records are never IPC prefixes.
use std::io::{self, BufRead};

pub(super) const MAX_LINE_BYTES: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Line {
    Text(String),
    Oversized,
}

/// The byte limit includes line endings. Drain excess through the next newline
/// so a long diagnostic cannot block the child or impersonate a later IPC record.
pub(super) fn read_line(reader: &mut impl BufRead) -> io::Result<Option<Line>> {
    let mut bytes = Vec::with_capacity(MAX_LINE_BYTES);
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if bytes.is_empty() && !oversized {
                return Ok(None);
            }
            break;
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        let ended = available[count - 1] == b'\n';
        if !oversized {
            if count > MAX_LINE_BYTES - bytes.len() {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&available[..count]);
            }
        }
        reader.consume(count);
        if ended {
            break;
        }
    }
    if oversized {
        return Ok(Some(Line::Oversized));
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    String::from_utf8(bytes)
        .map(|line| Some(Line::Text(line)))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid worker output encoding"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor, Read};

    #[test]
    fn exact_limit_crlf_and_unterminated_tail_match_line_semantics() -> io::Result<()> {
        let source = format!("{}\r\nlast\r", "x".repeat(MAX_LINE_BYTES - 2));
        let mut reader = BufReader::with_capacity(7, Cursor::new(source));
        assert_eq!(
            read_line(&mut reader)?,
            Some(Line::Text("x".repeat(MAX_LINE_BYTES - 2)))
        );
        assert_eq!(read_line(&mut reader)?, Some(Line::Text("last\r".into())));
        assert_eq!(read_line(&mut reader)?, None);
        Ok(())
    }

    #[test]
    fn oversized_stream_is_drained_and_next_record_is_preserved() -> io::Result<()> {
        // Lazy input: neither this fixture nor the reader owns the 16 MiB line.
        let source = io::repeat(b'x')
            .take(16 * 1024 * 1024)
            .chain(Cursor::new(b"\nnext\n"));
        let mut reader = BufReader::with_capacity(37, source);
        assert_eq!(read_line(&mut reader)?, Some(Line::Oversized));
        assert_eq!(read_line(&mut reader)?, Some(Line::Text("next".into())));
        assert_eq!(read_line(&mut reader)?, None);
        Ok(())
    }

    #[test]
    fn oversize_at_eof_and_invalid_encoding_are_not_partial_records() -> io::Result<()> {
        let mut reader = BufReader::new(io::repeat(b'x').take((MAX_LINE_BYTES + 1) as u64));
        assert_eq!(read_line(&mut reader)?, Some(Line::Oversized));
        assert_eq!(read_line(&mut reader)?, None);
        let mut invalid = Cursor::new(b"\xff\nnext\n");
        assert_eq!(
            read_line(&mut invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(read_line(&mut invalid)?, Some(Line::Text("next".into())));
        Ok(())
    }
}
