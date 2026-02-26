//! OSC (Operating System Command) sequence parser.
//!
//! Handles parsing after the router has consumed `ESC ]`.
//! Accumulates payload bytes until BEL (0x07) terminates.
//! ST (ESC \) termination is handled by the router.
//! Only produces events for query sequences (payload ends with `?`).

const MAX_PAYLOAD: usize = 256;

pub enum OscResult {
    Pending,
    /// Payload is a query (ends with `?`).
    Complete,
    Reject,
}

pub struct OscParser {
    payload: Vec<u8>,
}

impl OscParser {
    pub fn new() -> Self {
        Self {
            payload: Vec::new(),
        }
    }

    pub fn feed(&mut self, byte: u8) -> OscResult {
        match byte {
            0x07 => self.finish(),
            0x00..=0x06 | 0x08..=0x1f | 0x7f => OscResult::Reject,
            _ => {
                if self.payload.len() >= MAX_PAYLOAD {
                    OscResult::Reject
                } else {
                    self.payload.push(byte);
                    OscResult::Pending
                }
            }
        }
    }

    /// Check if the accumulated payload is a query (ends with `?`).
    pub fn finish(&self) -> OscResult {
        if self.payload.last() == Some(&b'?') {
            OscResult::Complete
        } else {
            OscResult::Reject
        }
    }

    pub fn reset(&mut self) {
        self.payload.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_with_bel() {
        let mut p = OscParser::new();
        for &b in b"11;?" {
            assert!(matches!(p.feed(b), OscResult::Pending));
        }
        assert!(matches!(p.feed(0x07), OscResult::Complete));
    }

    #[test]
    fn non_query_rejected() {
        let mut p = OscParser::new();
        for &b in b"0;title" {
            assert!(matches!(p.feed(b), OscResult::Pending));
        }
        assert!(matches!(p.feed(0x07), OscResult::Reject));
    }

    #[test]
    fn c0_control_rejects() {
        let mut p = OscParser::new();
        p.feed(b'1');
        assert!(matches!(p.feed(0x00), OscResult::Reject));
    }

    #[test]
    fn del_rejects() {
        let mut p = OscParser::new();
        p.feed(b'1');
        assert!(matches!(p.feed(0x7f), OscResult::Reject));
    }

    #[test]
    fn max_payload_rejects() {
        let mut p = OscParser::new();
        for _ in 0..MAX_PAYLOAD {
            assert!(matches!(p.feed(b'x'), OscResult::Pending));
        }
        assert!(matches!(p.feed(b'x'), OscResult::Reject));
    }

    #[test]
    fn empty_payload_query_with_just_question_mark() {
        let mut p = OscParser::new();
        p.feed(b'?');
        assert!(matches!(p.feed(0x07), OscResult::Complete));
    }

    #[test]
    fn finish_checks_payload() {
        let mut p = OscParser::new();
        for &b in b"11;?" {
            p.feed(b);
        }
        assert!(matches!(p.finish(), OscResult::Complete));
    }

    #[test]
    fn finish_rejects_non_query() {
        let mut p = OscParser::new();
        for &b in b"0;title" {
            p.feed(b);
        }
        assert!(matches!(p.finish(), OscResult::Reject));
    }
}
