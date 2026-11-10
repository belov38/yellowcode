use std::fmt;

#[derive(Clone, PartialEq, Eq)]
pub struct Hashed(String);

impl Hashed {
    pub(crate) fn new(hex: String) -> Self {
        Hashed(hex)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Hashed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hashed({})", self.0)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Redacted(String);

impl Redacted {
    pub(crate) fn new(text: String) -> Self {
        Redacted(text)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Redacted({:?})", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Num(i64),
    Bool(bool),
    Tag(&'static str),
    Hash(Hashed),
    Text(Redacted),
    Null,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    kind: &'static str,
    fields: Vec<(&'static str, Value)>,
}

impl Event {
    pub fn new(kind: &'static str) -> Self {
        Event {
            kind,
            fields: Vec::new(),
        }
    }

    pub fn kind(&self) -> &'static str {
        self.kind
    }

    pub fn fields(&self) -> &[(&'static str, Value)] {
        &self.fields
    }

    pub fn num(mut self, key: &'static str, value: i64) -> Self {
        self.fields.push((key, Value::Num(value)));
        self
    }

    pub fn unum(self, key: &'static str, value: u64) -> Self {
        self.num(key, i64::try_from(value).unwrap_or(i64::MAX))
    }

    pub fn opt_unum(self, key: &'static str, value: Option<u64>) -> Self {
        match value {
            Some(value) => self.unum(key, value),
            None => self.null(key),
        }
    }

    pub fn flag(mut self, key: &'static str, value: bool) -> Self {
        self.fields.push((key, Value::Bool(value)));
        self
    }

    pub fn tag(mut self, key: &'static str, value: &'static str) -> Self {
        self.fields.push((key, Value::Tag(value)));
        self
    }

    pub fn hash(mut self, key: &'static str, value: Hashed) -> Self {
        self.fields.push((key, Value::Hash(value)));
        self
    }

    pub fn text(mut self, key: &'static str, value: Redacted) -> Self {
        debug_assert!(
            crate::render::TEXT_KEYS.contains(&key),
            "text key {key:?} is not in render::TEXT_KEYS, so the final scan would not search it"
        );
        self.fields.push((key, Value::Text(value)));
        self
    }

    pub fn null(mut self, key: &'static str) -> Self {
        self.fields.push((key, Value::Null));
        self
    }

    pub fn io_error(self, error: &std::io::Error) -> Self {
        let event = match error.raw_os_error() {
            Some(number) => self.num("os_error", i64::from(number)),
            None => self.null("os_error"),
        };
        event.tag("io_kind", crate::sink::io_kind_tag(error.kind()))
    }

    pub fn opt_num(self, key: &'static str, value: Option<i64>) -> Self {
        match value {
            Some(value) => self.num(key, value),
            None => self.null(key),
        }
    }

    pub fn opt_flag(self, key: &'static str, value: Option<bool>) -> Self {
        match value {
            Some(value) => self.flag(key, value),
            None => self.null(key),
        }
    }

    pub fn opt_tag(self, key: &'static str, value: Option<&'static str>) -> Self {
        match value {
            Some(value) => self.tag(key, value),
            None => self.null(key),
        }
    }

    pub fn render(&self, seq: u64, t_ms: u64) -> String {
        self.render_with(Some((seq, t_ms)))
    }

    pub fn render_bare(&self) -> String {
        self.render_with(None)
    }

    fn render_with(&self, stamp: Option<(u64, u64)>) -> String {
        let mut line = String::with_capacity(96);
        line.push_str("{\"event\":");
        line.push_str(&json_string(self.kind));
        if let Some((seq, t_ms)) = stamp {
            line.push_str(",\"seq\":");
            line.push_str(&seq.to_string());
            line.push_str(",\"t\":");
            line.push_str(&t_ms.to_string());
        }
        for (key, value) in &self.fields {
            line.push(',');
            line.push_str(&json_string(key));
            line.push(':');
            match value {
                Value::Num(number) => line.push_str(&number.to_string()),
                Value::Bool(flag) => line.push_str(if *flag { "true" } else { "false" }),
                Value::Tag(tag) => line.push_str(&json_string(tag)),
                Value::Hash(hash) => line.push_str(&json_string(hash.as_str())),
                Value::Text(text) => line.push_str(&json_string(text.as_str())),
                Value::Null => line.push_str("null"),
            }
        }
        line.push('}');
        line
    }
}

pub(crate) fn json_string(text: &str) -> String {
    let encoded = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned());
    let mut out = String::with_capacity(encoded.len());
    for c in encoded.chars() {
        match c {
            '\u{85}' => out.push_str("\\u0085"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            other => out.push(other),
        }
    }
    out
}
