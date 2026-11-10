use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingLimits {
    pub budget_bytes: usize,
    pub head_events: usize,
    pub head_bytes: usize,
    pub max_events: usize,
}

#[derive(Debug)]
pub struct Ring {
    limits: RingLimits,
    head: Vec<String>,
    head_bytes: usize,
    head_open: bool,
    tail: VecDeque<String>,
    tail_bytes: usize,
    dropped_events: u64,
    dropped_bytes: u64,
}

impl Ring {
    pub fn new(limits: RingLimits) -> Self {
        Ring {
            limits,
            head: Vec::new(),
            head_bytes: 0,
            head_open: true,
            tail: VecDeque::new(),
            tail_bytes: 0,
            dropped_events: 0,
            dropped_bytes: 0,
        }
    }

    fn drop_counted(&mut self, size: usize) {
        self.dropped_events = self.dropped_events.saturating_add(1);
        self.dropped_bytes = self.dropped_bytes.saturating_add(size as u64);
    }

    pub fn push(&mut self, line: String) {
        let size = line.len().saturating_add(1);
        let limits = self.limits;
        if self.head_open {
            let head_events = limits.head_events.min(limits.max_events);
            let head_bytes = limits.head_bytes.min(limits.budget_bytes);
            if self.head.len() < head_events && self.head_bytes.saturating_add(size) <= head_bytes {
                self.head_bytes += size;
                self.head.push(line);
                return;
            }
            self.head_open = false;
        }
        if size > limits.budget_bytes.saturating_sub(self.head_bytes) {
            self.drop_counted(size);
            return;
        }
        self.tail_bytes += size;
        self.tail.push_back(line);
        while self.head_bytes + self.tail_bytes > limits.budget_bytes
            || self.head.len() + self.tail.len() > limits.max_events
        {
            let Some(old) = self.tail.pop_front() else {
                break;
            };
            let old_size = old.len().saturating_add(1);
            self.tail_bytes = self.tail_bytes.saturating_sub(old_size);
            self.drop_counted(old_size);
        }
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.head
            .iter()
            .map(String::as_str)
            .chain(self.tail.iter().map(String::as_str))
    }

    pub fn head(&self) -> impl Iterator<Item = &str> {
        self.head.iter().map(String::as_str)
    }

    pub fn tail(&self) -> impl Iterator<Item = &str> {
        self.tail.iter().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.head.len() + self.tail.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bytes(&self) -> usize {
        self.head_bytes + self.tail_bytes
    }

    pub fn dropped(&self) -> (u64, u64) {
        (self.dropped_events, self.dropped_bytes)
    }

    pub fn head_len(&self) -> usize {
        self.head.len()
    }
}
