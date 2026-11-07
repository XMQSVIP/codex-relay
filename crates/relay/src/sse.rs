//! Incremental SSE framing shared by strict filtering and transparent observation.

#[derive(Default)]
pub(crate) struct Frames {
    line: Vec<u8>,
    frame: Vec<u8>,
    started: bool,
    skip_lf: bool,
    bytes: usize,
}

impl Frames {
    pub(crate) fn push(&mut self, bytes: &[u8], limit: usize) -> Result<Vec<Vec<u8>>, ()> {
        let mut frames = Vec::new();
        for &byte in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    if self.bytes > 0 {
                        self.bytes += 1;
                        if self.bytes > limit {
                            return Err(());
                        }
                    }
                    continue;
                }
            }
            self.bytes += 1;
            if self.bytes > limit {
                return Err(());
            }
            if matches!(byte, b'\r' | b'\n') {
                self.skip_lf = byte == b'\r';
                if !self.started {
                    self.started = true;
                    if self.line.starts_with(b"\xef\xbb\xbf") {
                        self.line.drain(..3);
                    }
                }
                if self.line.is_empty() {
                    self.bytes = 0;
                    if !self.frame.is_empty() {
                        frames.push(std::mem::take(&mut self.frame));
                    }
                } else {
                    self.frame.append(&mut self.line);
                    self.frame.push(b'\n');
                }
            } else {
                self.line.push(byte);
            }
        }
        Ok(frames)
    }

    pub(crate) fn pending_is_whitespace(&self) -> bool {
        self.line
            .iter()
            .chain(&self.frame)
            .all(u8::is_ascii_whitespace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_byte_preserves_utf8_newlines_and_only_one_leading_bom() {
        let wire = "\u{feff}data: 中文🌍\r\n\r\ndata: \u{feff}kept\r\rdata: tail\n\n";
        let mut parser = Frames::default();
        let mut frames = Vec::new();
        for byte in wire.as_bytes() {
            frames.extend(parser.push(&[*byte], 1024).unwrap());
        }
        assert_eq!(
            frames,
            ["data: 中文🌍\n", "data: \u{feff}kept\n", "data: tail\n"]
                .map(|s| s.as_bytes().to_vec())
        );
        assert!(parser.pending_is_whitespace());
    }

    #[test]
    fn incomplete_events_stay_pending_and_frame_limits_apply_across_chunks() {
        let mut parser = Frames::default();
        assert!(parser.push(b"data: abc\r", 16).unwrap().is_empty());
        assert!(!parser.pending_is_whitespace());
        assert!(parser.push(b"\ndata: def", 16).is_err());
        let mut parser = Frames::default();
        assert!(parser.push(b"\n\r\r\n", 16).unwrap().is_empty());
        assert!(parser.pending_is_whitespace());
    }
}
