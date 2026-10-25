use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlAttr {
    pub qname: String,
    pub prefix: String,
    pub local: String,
    pub ns: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XmlEvent {
    Declaration {
        version: String,
        encoding: Option<String>,
        standalone: Option<bool>,
    },
    Start {
        qname: String,
        prefix: String,
        local: String,
        ns: String,
        attrs: Vec<XmlAttr>,
        empty: bool,
    },
    End {
        qname: String,
        prefix: String,
        local: String,
        ns: String,
    },
    Text(String),
    Whitespace(String),
    CData(String),
    Comment(String),
    Pi {
        target: String,
        data: String,
    },
    Doctype(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlError {
    pub message: String,
    pub line: u32,
    pub col: u32,
    pub fatal: bool,
}

struct Open {
    qname: String,
    prefix: String,
    local: String,
    ns: String,
    line: u32,
    scope_mark: usize,
}

pub struct XmlReader {
    src: Arc<[char]>,
    i: usize,
    line: u32,
    col: u32,
    stack: Vec<Open>,
    scope_log: Vec<String>,
    scope: HashMap<String, Vec<String>>,
    pending_end: bool,
    seen_root: bool,
    seen_doctype: bool,
    seen_content: bool,
    done: bool,
    failed: Option<XmlError>,
}

pub(crate) fn is_name_start(c: char) -> bool {
    matches!(c,
        ':' | '_' | 'A'..='Z' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

pub(crate) fn is_name_char(c: char) -> bool {
    is_name_start(c)
        || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

type R<T> = Result<T, XmlError>;

impl XmlReader {
    pub fn new(text: String) -> XmlReader {
        let src: Arc<[char]> = text.chars().collect();
        XmlReader::over(src)
    }

    fn over(src: Arc<[char]>) -> XmlReader {
        let i = usize::from(src.first() == Some(&'\u{FEFF}'));
        XmlReader {
            src,
            i,
            line: 1,
            col: 1,
            stack: Vec::new(),
            scope_log: Vec::new(),
            scope: HashMap::new(),
            pending_end: false,
            seen_root: false,
            seen_doctype: false,
            seen_content: false,
            done: false,
            failed: None,
        }
    }

    pub fn check_ahead(&self, chars: usize) -> Result<bool, XmlError> {
        let mut ahead = XmlReader::over(self.src.clone());
        loop {
            if ahead.i >= chars && ahead.i < ahead.src.len() {
                return Ok(false);
            }
            if ahead.next()?.is_none() {
                return Ok(true);
            }
        }
    }

    pub fn position(&self) -> (u32, u32) {
        (self.line, self.col)
    }

    pub fn lookup_namespace(&self, prefix: &str) -> Option<&str> {
        if prefix == "xml" {
            return Some(XML_NS);
        }
        self.scope
            .get(prefix)
            .and_then(|v| v.last())
            .map(String::as_str)
    }

    fn bind(&mut self, prefix: &str, uri: &str) {
        self.scope_log.push(prefix.to_string());
        self.scope
            .entry(prefix.to_string())
            .or_default()
            .push(uri.to_string());
    }

    fn unwind(&mut self, mark: usize) {
        while self.scope_log.len() > mark {
            if let Some(p) = self.scope_log.pop()
                && let Some(v) = self.scope.get_mut(&p)
            {
                v.pop();
                if v.is_empty() {
                    self.scope.remove(&p);
                }
            }
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<XmlEvent>, XmlError> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        match self.step() {
            Err(e) => {
                self.failed = Some(e.clone());
                Err(e)
            }
            ok => ok,
        }
    }

    fn err<T>(&self, message: impl Into<String>) -> R<T> {
        Err(XmlError {
            message: message.into(),
            line: self.line,
            col: self.col,
            fatal: true,
        })
    }

    fn ns_err<T>(&self, message: impl Into<String>) -> R<T> {
        Err(XmlError {
            message: message.into(),
            line: self.line,
            col: self.col,
            fatal: false,
        })
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.i).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.src.get(self.i + n).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.src.get(self.i).copied()?;
        self.i += 1;
        if c == '\n' || (c == '\r' && self.src.get(self.i) != Some(&'\n')) {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn starts_with(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(k, c)| self.peek_at(k) == Some(c))
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.starts_with(s) {
            for _ in s.chars() {
                self.bump();
            }
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) -> bool {
        let mut any = false;
        while self.peek().is_some_and(is_ws) {
            self.bump();
            any = true;
        }
        any
    }

    fn name(&mut self) -> Option<String> {
        if !self.peek().is_some_and(is_name_start) {
            return None;
        }
        let mut s = String::new();
        while let Some(c) = self.peek().filter(|c| is_name_char(*c)) {
            s.push(c);
            self.bump();
        }
        Some(s)
    }

    fn step(&mut self) -> R<Option<XmlEvent>> {
        if self.pending_end {
            self.pending_end = false;
            return self.pop_end().map(Some);
        }
        loop {
            if self.done {
                return Ok(None);
            }
            match self.peek() {
                None => return self.at_eof(),
                Some('<') => {
                    if let Some(e) = self.markup()? {
                        return Ok(Some(e));
                    }
                }
                Some(_) if self.stack.is_empty() => {
                    if self.skip_ws() && self.peek().is_none_or(|c| c == '<') {
                        continue;
                    }
                    return if self.seen_root {
                        self.err("Extra content at the end of the document")
                    } else {
                        self.err("Document is empty")
                    };
                }
                Some(_) => return self.text().map(Some),
            }
        }
    }

    fn at_eof(&mut self) -> R<Option<XmlEvent>> {
        if !self.stack.is_empty() {
            return self.err("Extra content at the end of the document");
        }
        if !self.seen_root {
            return if self.seen_content {
                self.err("Start tag expected, '<' not found")
            } else {
                self.err("Extra content at the end of the document")
            };
        }
        self.done = true;
        Ok(None)
    }

    fn pop_end(&mut self) -> R<XmlEvent> {
        let Some(o) = self.stack.pop() else {
            return self.err("Extra content at the end of the document");
        };
        self.unwind(o.scope_mark);
        Ok(XmlEvent::End {
            qname: o.qname,
            prefix: o.prefix,
            local: o.local,
            ns: o.ns,
        })
    }

    fn text(&mut self) -> R<XmlEvent> {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            match c {
                '<' => break,
                '&' => self.reference(&mut s)?,
                ']' if self.starts_with("]]>") => {
                    return self.err("Sequence ']]>' not allowed in content");
                }
                c if !is_xml_char(c) => {
                    return self.err(format!("PCDATA invalid Char value {}", c as u32));
                }
                c => {
                    s.push(c);
                    self.bump();
                }
            }
        }
        self.seen_content = true;
        Ok(if s.chars().all(is_ws) {
            XmlEvent::Whitespace(s)
        } else {
            XmlEvent::Text(s)
        })
    }

    fn reference(&mut self, out: &mut String) -> R<()> {
        self.bump();
        if self.peek() == Some('#') {
            self.bump();
            let hex = self.peek() == Some('x');
            if hex {
                self.bump();
            }
            let radix = if hex { 16 } else { 10 };
            let mut v: u32 = 0;
            let mut digits = 0;
            while let Some(d) = self.peek().and_then(|c| c.to_digit(radix)) {
                v = v.saturating_mul(radix).saturating_add(d);
                digits += 1;
                self.bump();
            }
            if digits == 0 || self.peek() != Some(';') {
                return self.err(if hex {
                    "xmlParseCharRef: invalid hexadecimal value"
                } else {
                    "xmlParseCharRef: invalid decimal value"
                });
            }
            self.bump();
            return match char::from_u32(v).filter(|c| is_xml_char(*c)) {
                Some(c) => {
                    out.push(c);
                    Ok(())
                }
                None => self.err(format!("xmlParseCharRef: invalid xmlChar value {v}")),
            };
        }
        let Some(name) = self.name() else {
            return self.err("xmlParseEntityRef: no name");
        };
        if self.peek() != Some(';') {
            return self.err("EntityRef: expecting ';'");
        }
        self.bump();
        let c = match name.as_str() {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "apos" => '\'',
            "quot" => '"',
            _ => return self.err(format!("Entity '{name}' not defined")),
        };
        out.push(c);
        Ok(())
    }

    fn markup(&mut self) -> R<Option<XmlEvent>> {
        if self.starts_with("</") {
            return self.end_tag().map(Some);
        }
        if self.starts_with("<?") {
            return self.pi().map(Some);
        }
        if self.starts_with("<!--") {
            return self.comment().map(Some);
        }
        if self.starts_with("<![CDATA[") {
            if self.stack.is_empty() {
                return self.outside_root();
            }
            return self.cdata().map(Some);
        }
        if self.starts_with("<!DOCTYPE") {
            if self.seen_root || self.seen_doctype || !self.stack.is_empty() {
                return self.err("StartTag: invalid element name");
            }
            return self.doctype().map(Some);
        }
        if self.stack.is_empty() && self.seen_root {
            return self.outside_root();
        }
        self.start_tag().map(Some)
    }

    fn outside_root<T>(&self) -> R<T> {
        if self.seen_root {
            self.err("Extra content at the end of the document")
        } else {
            self.err("Start tag expected, '<' not found")
        }
    }

    fn comment(&mut self) -> R<XmlEvent> {
        self.eat("<!--");
        let mut s = String::new();
        loop {
            match self.peek() {
                None => return self.err("Comment not terminated"),
                Some('-') if self.starts_with("--") => {
                    if self.starts_with("-->") {
                        self.eat("-->");
                        self.seen_content = true;
                        return Ok(XmlEvent::Comment(s));
                    }
                    return self.err("Double hyphen within comment");
                }
                Some(c) if !is_xml_char(c) => {
                    return self.err(format!("PCDATA invalid Char value {}", c as u32));
                }
                Some(c) => {
                    s.push(c);
                    self.bump();
                }
            }
        }
    }

    fn cdata(&mut self) -> R<XmlEvent> {
        self.eat("<![CDATA[");
        let mut s = String::new();
        loop {
            match self.peek() {
                None => return self.err("CData section not finished"),
                Some(']') if self.starts_with("]]>") => {
                    self.eat("]]>");
                    return Ok(XmlEvent::CData(s));
                }
                Some(c) if !is_xml_char(c) => {
                    return self.err(format!("PCDATA invalid Char value {}", c as u32));
                }
                Some(c) => {
                    s.push(c);
                    self.bump();
                }
            }
        }
    }

    fn doctype(&mut self) -> R<XmlEvent> {
        self.eat("<!DOCTYPE");
        if !self.peek().is_some_and(is_ws) {
            return self.err("Space required after 'DOCTYPE'");
        }
        self.seen_doctype = true;
        let mut s = String::new();
        let mut quote: Option<char> = None;
        let mut depth = 0u32;
        loop {
            let Some(c) = self.peek() else {
                return self.err("DOCTYPE improperly terminated");
            };
            if quote.is_none() && depth > 0 && (self.starts_with("<!--") || self.starts_with("<?"))
            {
                let end = if self.starts_with("<!--") {
                    "-->"
                } else {
                    "?>"
                };
                loop {
                    if self.starts_with(end) {
                        for _ in end.chars() {
                            s.push(self.bump().unwrap_or(' '));
                        }
                        break;
                    }
                    match self.bump() {
                        Some(c) => s.push(c),
                        None => return self.err("DOCTYPE improperly terminated"),
                    }
                }
                continue;
            }
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"' | '\'') => quote = Some(c),
                (None, '[') => depth += 1,
                (None, ']') => depth = depth.saturating_sub(1),
                (None, '>') if depth == 0 => {
                    self.bump();
                    self.seen_content = true;
                    return Ok(XmlEvent::Doctype(s.trim().to_string()));
                }
                _ => {}
            }
            s.push(c);
            self.bump();
        }
    }

    fn pi(&mut self) -> R<XmlEvent> {
        let at_start = self.i == usize::from(self.src.first() == Some(&'\u{FEFF}'));
        self.eat("<?");
        let Some(target) = self.name() else {
            return self.err("xmlParsePI : no target name");
        };
        if target.eq_ignore_ascii_case("xml") {
            if target != "xml" || !at_start {
                return self.err("XML declaration allowed only at the start of the document");
            }
            return self.declaration();
        }
        let ws = self.skip_ws();
        let mut data = String::new();
        loop {
            match self.peek() {
                None => return self.err(format!("ParsePI: PI {target} never end ...")),
                Some('?') if self.starts_with("?>") => {
                    self.eat("?>");
                    self.seen_content = true;
                    return Ok(XmlEvent::Pi { target, data });
                }
                Some(_) if !ws && data.is_empty() => {
                    return self.err("ParsePI: PI target space expected");
                }
                Some(c) if !is_xml_char(c) => {
                    return self.err(format!("PCDATA invalid Char value {}", c as u32));
                }
                Some(c) => {
                    data.push(c);
                    self.bump();
                }
            }
        }
    }

    fn declaration(&mut self) -> R<XmlEvent> {
        let mut version = None;
        let mut encoding = None;
        let mut standalone = None;
        let mut stage = 0u8;
        loop {
            let ws = self.skip_ws();
            if self.eat("?>") {
                break;
            }
            if !ws && version.is_some() {
                return self.err("parsing XML declaration: '?>' expected");
            }
            let Some(key) = self.name() else {
                return if version.is_none() {
                    self.err("Malformed declaration expecting version")
                } else {
                    self.err("parsing XML declaration: '?>' expected")
                };
            };
            self.skip_ws();
            if self.peek() != Some('=') {
                return self.err("parsing XML declaration: '?>' expected");
            }
            self.bump();
            self.skip_ws();
            let Some(q) = self.peek().filter(|c| *c == '"' || *c == '\'') else {
                return self.err("String not started expecting ' or \"");
            };
            self.bump();
            let mut v = String::new();
            loop {
                match self.bump() {
                    None => return self.err("String not closed expecting \" or '"),
                    Some(c) if c == q => break,
                    Some(c) => v.push(c),
                }
            }
            match key.as_str() {
                "version" if stage == 0 => {
                    if !v.starts_with("1.") || v.len() < 3 {
                        return self.err(format!("Unsupported version '{v}'"));
                    }
                    version = Some(v);
                    stage = 1;
                }
                "encoding" if stage == 1 => {
                    encoding = Some(v);
                    stage = 2;
                }
                "standalone" if (1..=2).contains(&stage) => {
                    standalone = match v.as_str() {
                        "yes" => Some(true),
                        "no" => Some(false),
                        _ => return self.err("standalone accepts only 'yes' or 'no'"),
                    };
                    stage = 3;
                }
                _ if stage == 0 => {
                    return self.err("Malformed declaration expecting version");
                }
                _ => return self.err("parsing XML declaration: '?>' expected"),
            }
        }
        let Some(version) = version else {
            return self.err("Malformed declaration expecting version");
        };
        self.seen_content = true;
        Ok(XmlEvent::Declaration {
            version,
            encoding,
            standalone,
        })
    }

    fn split_qname(&self, q: &str) -> R<(String, String)> {
        match q.split_once(':') {
            None => Ok((String::new(), q.to_string())),
            Some((p, l)) if !p.is_empty() && !l.is_empty() && !l.contains(':') => {
                Ok((p.to_string(), l.to_string()))
            }
            _ => self.ns_err(format!("Failed to parse QName '{q}'")),
        }
    }

    fn premature(&self, name: &str, line: u32) -> XmlError {
        XmlError {
            message: format!("Premature end of data in tag {name} line {line}"),
            line: self.line,
            col: self.col,
            fatal: true,
        }
    }

    fn attr_value(&mut self, tag: &str, tag_line: u32) -> R<String> {
        let q = self.bump().unwrap_or('"');
        let mut v = String::new();
        loop {
            match self.peek() {
                None => return Err(self.premature(tag, tag_line)),
                Some(c) if c == q => {
                    self.bump();
                    return Ok(v);
                }
                Some('<') => return self.err("Unescaped '<' not allowed in attributes values"),
                Some('&') => self.reference(&mut v)?,
                Some(c) if !is_xml_char(c) => {
                    return self.err(format!("PCDATA invalid Char value {}", c as u32));
                }
                Some(c) => {
                    v.push(if matches!(c, '\t' | '\n' | '\r') {
                        ' '
                    } else {
                        c
                    });
                    self.bump();
                }
            }
        }
    }

    fn start_tag(&mut self) -> R<XmlEvent> {
        let tag_line = self.line;
        self.bump();
        let Some(qname) = self.name() else {
            return self.err("StartTag: invalid element name");
        };
        let mut raw: Vec<(String, String)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let empty;
        loop {
            let ws = self.skip_ws();
            match self.peek() {
                None if !ws && !raw.is_empty() => return self.err("attributes construct error"),
                None => return Err(self.premature(&qname, tag_line)),
                Some('>') => {
                    empty = false;
                    break;
                }
                Some('/') => {
                    if self.peek_at(1) == Some('>') {
                        empty = true;
                        break;
                    }
                    self.bump();
                    return match self.peek() {
                        None => Err(self.premature(&qname, tag_line)),
                        Some(_) => self.err("Couldn't find end of Start Tag"),
                    };
                }
                Some(_) => {}
            }
            if !ws {
                return self.err("attributes construct error");
            }
            let Some(an) = self.name() else {
                return self.err("error parsing attribute name");
            };
            self.skip_ws();
            match self.peek() {
                Some('=') => {
                    self.bump();
                }
                None => return Err(self.premature(&qname, tag_line)),
                Some(_) => {
                    return self.err(format!("Specification mandates value for attribute {an}"));
                }
            }
            self.skip_ws();
            match self.peek() {
                Some('"' | '\'') => {}
                None => return Err(self.premature(&qname, tag_line)),
                Some(_) => return self.err("AttValue: \" or ' expected"),
            }
            let v = self.attr_value(&qname, tag_line)?;
            if !seen.insert(an.clone()) {
                return self.err(format!("Attribute {an} redefined"));
            }
            raw.push((an, v));
        }

        let scope_mark = self.scope_log.len();
        let mut attrs = Vec::with_capacity(raw.len());
        for (an, v) in &raw {
            if an == "xmlns" {
                if v == XML_NS || v == XMLNS_NS {
                    return self.ns_binding_error(v);
                }
                self.bind("", v);
            } else if let Some(p) = an.strip_prefix("xmlns:") {
                if p.is_empty() || p.contains(':') {
                    return self.ns_err(format!("Failed to parse QName '{an}'"));
                }
                if v.is_empty() {
                    return self.ns_err(format!("xmlns:{p}: Empty XML namespace is not allowed"));
                }
                if p == "xmlns" {
                    return self.ns_err("redefinition of the xmlns prefix is forbidden");
                }
                if p == "xml" {
                    if v != XML_NS {
                        return self.ns_err("xml namespace prefix mapped to wrong URI");
                    }
                    continue;
                }
                if v == XML_NS || v == XMLNS_NS {
                    return self.ns_binding_error(v);
                }
                self.bind(p, v);
            }
        }
        let (prefix, local) = self.split_qname(&qname)?;
        let mut expanded: HashSet<(String, String)> = HashSet::new();
        for (an, v) in raw {
            if an == "xmlns" || an.starts_with("xmlns:") {
                let (p, l) = match an.split_once(':') {
                    Some((p, l)) => (p.to_string(), l.to_string()),
                    None => (String::new(), an.clone()),
                };
                attrs.push(XmlAttr {
                    qname: an,
                    prefix: p,
                    local: l,
                    ns: XMLNS_NS.to_string(),
                    value: v,
                });
                continue;
            }
            let (p, l) = self.split_qname(&an)?;
            let ans = if p.is_empty() {
                String::new()
            } else {
                match self.lookup_namespace(&p) {
                    Some(u) => u.to_string(),
                    None => {
                        return self.ns_err(format!(
                            "Namespace prefix {p} for {l} on {local} is not defined"
                        ));
                    }
                }
            };
            if !ans.is_empty() && !expanded.insert((ans.clone(), l.clone())) {
                return self.ns_err(format!("Namespaced Attribute {l} in '{ans}' redefined"));
            }
            attrs.push(XmlAttr {
                qname: an,
                prefix: p,
                local: l,
                ns: ans,
                value: v,
            });
        }
        let ns = self.resolve_elem(&prefix, &local)?;
        self.bump();
        if empty {
            self.bump();
        }
        self.seen_root = true;
        self.seen_content = true;
        self.stack.push(Open {
            qname: qname.clone(),
            prefix: prefix.clone(),
            local: local.clone(),
            ns: ns.clone(),
            line: tag_line,
            scope_mark,
        });
        self.pending_end = empty;
        Ok(XmlEvent::Start {
            qname,
            prefix,
            local,
            ns,
            attrs,
            empty,
        })
    }

    fn ns_binding_error<T>(&self, uri: &str) -> R<T> {
        if uri == XML_NS {
            self.ns_err("xml namespace URI mapped to wrong prefix")
        } else {
            self.ns_err("reuse of the xmlns namespace name is forbidden")
        }
    }

    fn resolve_elem(&self, prefix: &str, local: &str) -> R<String> {
        match self.lookup_namespace(prefix) {
            Some(u) => Ok(u.to_string()),
            None if prefix.is_empty() => Ok(String::new()),
            None => self.ns_err(format!(
                "Namespace prefix {prefix} on {local} is not defined"
            )),
        }
    }

    fn end_tag(&mut self) -> R<XmlEvent> {
        if self.stack.is_empty() {
            return self.outside_root();
        }
        self.eat("</");
        let Some(name) = self.name() else {
            return self.err("EndTag: '</' not found");
        };
        let (open_name, open_line) = match self.stack.last() {
            Some(o) => (o.qname.clone(), o.line),
            None => return self.outside_root(),
        };
        if name != open_name {
            self.skip_ws();
            if self.peek() == Some('>') {
                self.bump();
            }
            return self.err(format!(
                "Opening and ending tag mismatch: {open_name} line {open_line} and {name}"
            ));
        }
        self.skip_ws();
        if self.peek().is_none() {
            return Err(self.premature(&name, open_line));
        }
        if self.peek() != Some('>') {
            return self.err("expected '>'");
        }
        self.bump();
        self.pop_end()
    }
}
