use std::collections::VecDeque;

pub(super) const HEAD_BYTES: usize = 4096;
pub(super) const TAIL_BYTES: usize = 4096;

#[derive(Default)]
pub(super) struct Retained {
    scan: bak_trace::StderrScan,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    dropped: u64,
    cut: bool,
    end_uncertain: bool,
}

impl Retained {
    pub(super) fn push(&mut self, bytes: &[u8]) {
        self.scan.push(bytes);
        let room = HEAD_BYTES.saturating_sub(self.head.len()).min(bytes.len());
        let (first, rest) = bytes.split_at(room);
        self.head.extend_from_slice(first);
        if rest.is_empty() {
            return;
        }
        self.tail.extend(rest.iter().copied());
        let excess = self.tail.len().saturating_sub(TAIL_BYTES);
        if excess > 0 {
            self.tail.drain(..excess);
            self.dropped = self.dropped.saturating_add(excess as u64);
        }
    }

    pub(super) fn scan_summary(&mut self) -> bak_trace::ScanSummary {
        std::mem::take(&mut self.scan).finish()
    }

    pub(super) fn mark_cut(&mut self) {
        self.cut = true;
    }

    pub(super) fn mark_end_uncertain(&mut self) {
        self.end_uncertain = true;
    }

    pub(super) fn render(&self, secret: &[u8]) -> String {
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        if self.dropped == 0 {
            let mut whole = self.head.clone();
            whole.extend_from_slice(&tail);
            return super::redact_diagnostic(&whole, secret, self.cut || self.end_uncertain);
        }
        let head = super::redact_cut(&self.head, secret, false, true);
        let tail = super::redact_cut(&tail, secret, true, self.cut || self.end_uncertain);
        let omitted = if self.cut {
            format!(
                "at least {} bytes of sqlcmd diagnostics omitted; the stream was not read to its end",
                self.dropped
            )
        } else {
            format!("{} bytes of sqlcmd diagnostics omitted", self.dropped)
        };
        format!("{head}\n[{omitted}]\n{tail}")
    }
}
