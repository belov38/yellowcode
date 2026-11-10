use std::{collections::VecDeque, fmt};

pub const MAX_MESSAGES: usize = 64;
pub const HEAD_MESSAGES: usize = 16;
pub const MAX_TEXT_CHARS: usize = crate::redact::MAX_INPUT_CHARS + 1;
const MAX_CARRY: usize = 4 * MAX_TEXT_CHARS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageSource {
    Server,
    Client,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RawMessage {
    pub source: MessageSource,
    pub msg: Option<u32>,
    pub level: Option<u32>,
    pub state: Option<u32>,
    pub line: Option<u32>,
    pub os_error: Option<i64>,
    pub text: String,
}

impl fmt::Debug for RawMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawMessage")
            .field("source", &self.source)
            .field("msg", &self.msg)
            .field("level", &self.level)
            .field("state", &self.state)
            .field("line", &self.line)
            .field("os_error", &self.os_error)
            .field("text_chars", &self.text.chars().count())
            .finish()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanSummary {
    pub lines: u64,
    pub bytes: u64,
    pub notice_lines: u64,
    pub percent: Option<u32>,
    pub messages: Vec<RawMessage>,
    pub omitted_messages: u64,
}

#[derive(Default)]
pub struct StderrScan {
    carry: Vec<u8>,
    lines: u64,
    bytes: u64,
    notice_lines: u64,
    percent: Option<u32>,
    current: Option<RawMessage>,
    head: Vec<RawMessage>,
    tail: VecDeque<RawMessage>,
    omitted: u64,
}

impl fmt::Debug for StderrScan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StderrScan")
            .field("lines", &self.lines)
            .field("bytes", &self.bytes)
            .field("notice_lines", &self.notice_lines)
            .field("percent", &self.percent)
            .field("messages", &(self.head.len() + self.tail.len()))
            .field("omitted", &self.omitted)
            .finish_non_exhaustive()
    }
}

struct Header {
    source: MessageSource,
    msg: Option<u32>,
    level: u32,
    state: u32,
    line: Option<u32>,
}

fn number(text: &str) -> Option<u32> {
    let text = text.trim();
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

struct HeaderWords {
    msg: &'static str,
    level: &'static str,
    state: &'static str,
    line: &'static str,
}

const ENGLISH: HeaderWords = HeaderWords {
    msg: "Msg ",
    level: "Level ",
    state: "State ",
    line: "Line ",
};

const RUSSIAN: HeaderWords = HeaderWords {
    msg: "Сообщение ",
    level: "уровень ",
    state: "состояние ",
    line: "строка ",
};

const HEADERS: [&HeaderWords; 2] = [&ENGLISH, &RUSSIAN];

fn parse_header(line: &str) -> Option<Header> {
    let line = line.trim_end();
    let mut found = None;
    for words in HEADERS {
        if let Some(rest) = line.strip_prefix(words.msg) {
            let mut parts = rest.split(", ");
            let msg = number(parts.next()?)?;
            let tail = parts.collect::<Vec<_>>().join(", ");
            found = Some((MessageSource::Server, tail, Some(msg), words));
            break;
        }
    }
    let (source, rest, msg, words) = match found {
        Some(found) => found,
        None => {
            let rest = line.strip_prefix("HResult 0x")?;
            let (code, tail) = rest.split_once(", ")?;
            if code.is_empty() || !code.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            (MessageSource::Client, tail.to_owned(), None, &ENGLISH)
        }
    };
    let (mut level, mut state, mut at) = (None, None, None);
    for part in rest.split(", ") {
        if let Some(value) = part.strip_prefix(words.level) {
            level = Some(number(value)?);
        } else if let Some(value) = part.strip_prefix(words.state) {
            state = Some(number(value)?);
        } else if let Some(value) = part.strip_prefix(words.line) {
            at = Some(number(value)?);
        }
    }
    Some(Header {
        source,
        msg,
        level: level?,
        state: state?,
        line: at,
    })
}

fn parse_percent(line: &str) -> Option<u32> {
    let value = line.trim().strip_suffix(" percent processed.")?;
    (value.len() <= 3).then(|| number(value)).flatten()
}

fn find_os_error(text: &str) -> Option<i64> {
    let lowered = text.to_lowercase();
    ["operating system error ", "(os error "]
        .iter()
        .filter_map(|marker| {
            let at = lowered.find(marker)?;
            let rest = lowered.get(at + marker.len()..)?;
            let digits: String = rest
                .chars()
                .take_while(char::is_ascii_digit)
                .take(18)
                .collect();
            digits.parse::<i64>().ok().map(|n| (at, n))
        })
        .min_by_key(|(at, _)| *at)
        .map(|(_, n)| n)
}

impl StderrScan {
    pub fn push(&mut self, bytes: &[u8]) {
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
        let mut rest = bytes;
        while !rest.is_empty() {
            match rest.iter().position(|b| *b == b'\n') {
                Some(at) => {
                    let (line, tail) = rest.split_at(at);
                    self.hold(line);
                    self.end_line();
                    rest = tail.get(1..).unwrap_or_default();
                }
                None => {
                    self.hold(rest);
                    break;
                }
            }
        }
    }

    fn hold(&mut self, piece: &[u8]) {
        let room = MAX_CARRY.saturating_sub(self.carry.len());
        self.carry
            .extend_from_slice(piece.get(..room).unwrap_or(piece));
    }

    fn end_line(&mut self) {
        self.lines += 1;
        let raw = std::mem::take(&mut self.carry);
        let text = String::from_utf8_lossy(&raw);
        self.read_line(text.trim_end_matches('\r'));
    }

    fn read_line(&mut self, line: &str) {
        if line.trim().is_empty() {
            self.close_message();
            return;
        }
        if let Some(header) = parse_header(line) {
            self.close_message();
            self.current = Some(RawMessage {
                source: header.source,
                msg: header.msg,
                level: Some(header.level),
                state: Some(header.state),
                line: header.line,
                os_error: None,
                text: String::new(),
            });
            return;
        }
        if let Some(percent) = parse_percent(line) {
            self.close_message();
            self.percent = Some(self.percent.map_or(percent, |seen| seen.max(percent)));
            self.notice_lines += 1;
            return;
        }
        if line.starts_with("Sqlcmd:") {
            self.close_message();
            self.keep(RawMessage {
                source: MessageSource::Client,
                msg: None,
                level: None,
                state: None,
                line: None,
                os_error: find_os_error(line),
                text: line.chars().take(MAX_TEXT_CHARS).collect(),
            });
            return;
        }
        match self.current.as_mut() {
            Some(message) => {
                if !message.text.is_empty() {
                    message.text.push('\n');
                }
                message.text.push_str(line);
                if message.text.chars().nth(MAX_TEXT_CHARS).is_some() {
                    message.text = message.text.chars().take(MAX_TEXT_CHARS).collect();
                }
            }
            None => self.notice_lines += 1,
        }
    }

    fn close_message(&mut self) {
        let Some(mut message) = self.current.take() else {
            return;
        };
        message.os_error = find_os_error(&message.text);
        if message.level.is_some_and(|level| level <= 10) {
            message.text.clear();
        }
        self.keep(message);
    }

    fn keep(&mut self, message: RawMessage) {
        if self.head.len() < HEAD_MESSAGES {
            self.head.push(message);
            return;
        }
        self.tail.push_back(message);
        if self.tail.len() > MAX_MESSAGES - HEAD_MESSAGES {
            self.tail.pop_front();
            self.omitted += 1;
        }
    }

    pub fn finish(mut self) -> ScanSummary {
        if !self.carry.is_empty() {
            self.end_line();
        }
        self.close_message();
        let mut messages = self.head;
        messages.extend(self.tail);
        ScanSummary {
            lines: self.lines,
            bytes: self.bytes,
            notice_lines: self.notice_lines,
            percent: self.percent,
            messages,
            omitted_messages: self.omitted,
        }
    }
}
