use super::xml_invalid_pos;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct XmlWriteOptions {
    pub indent: bool,
    pub indent_chars: String,
    pub newline: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XmlWriteError {
    InvalidChar { pos: usize },
    Order,
    UnboundPrefix,
}

const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

struct Elem {
    qname: String,
    scope_mark: usize,
    has_children: bool,
    has_text: bool,
}

struct Pending {
    local: String,
    ns: Option<String>,
    decls: Vec<(String, String)>,
    attrs: Vec<(String, Option<String>, String)>,
}

pub struct XmlWriter {
    o: XmlWriteOptions,
    out: String,
    stack: Vec<Elem>,
    pending: Option<Pending>,
    scope_log: Vec<(String, String)>,
    by_prefix: HashMap<String, Vec<String>>,
    by_uri: HashMap<String, Vec<String>>,
    root_closed: bool,
}

fn check(s: &str) -> Result<(), XmlWriteError> {
    let u: Vec<u16> = s.encode_utf16().collect();
    match xml_invalid_pos(&u, false) {
        0 => Ok(()),
        p => Err(XmlWriteError::InvalidChar { pos: p as usize }),
    }
}

fn esc(out: &mut String, s: &str, attr: bool) {
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' if attr => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

impl XmlWriter {
    pub fn new(o: XmlWriteOptions) -> XmlWriter {
        XmlWriter {
            o,
            out: String::new(),
            stack: Vec::new(),
            pending: None,
            scope_log: Vec::new(),
            by_prefix: HashMap::new(),
            by_uri: HashMap::new(),
            root_closed: false,
        }
    }

    pub fn set_indent(&mut self, on: bool) {
        self.o.indent = on;
    }

    fn resolve(&self, prefix: &str) -> Option<&str> {
        if prefix == "xml" {
            return Some(XML_NS);
        }
        self.by_prefix
            .get(prefix)
            .and_then(|v| v.last())
            .map(String::as_str)
    }

    fn bind(&mut self, prefix: &str, uri: &str) {
        self.scope_log.push((prefix.to_string(), uri.to_string()));
        self.by_prefix
            .entry(prefix.to_string())
            .or_default()
            .push(uri.to_string());
        self.by_uri
            .entry(uri.to_string())
            .or_default()
            .push(prefix.to_string());
    }

    fn unwind(&mut self, mark: usize) {
        while self.scope_log.len() > mark {
            let Some((p, u)) = self.scope_log.pop() else {
                break;
            };
            if let Some(v) = self.by_prefix.get_mut(&p) {
                v.pop();
                if v.is_empty() {
                    self.by_prefix.remove(&p);
                }
            }
            if let Some(v) = self.by_uri.get_mut(&u) {
                v.pop();
                if v.is_empty() {
                    self.by_uri.remove(&u);
                }
            }
        }
    }

    pub fn prefix_of(&self, uri: &str) -> Option<&str> {
        self.find_prefix(uri, true)
    }

    fn find_prefix(&self, uri: &str, allow_default: bool) -> Option<&str> {
        if uri == XML_NS {
            return Some("xml");
        }
        self.by_uri
            .get(uri)?
            .iter()
            .rev()
            .filter(|p| allow_default || !p.is_empty())
            .map(String::as_str)
            .find(|p| self.resolve(p) == Some(uri))
    }

    fn break_line(&mut self, depth: usize) {
        if self.o.indent && !self.out.is_empty() {
            self.out.push_str(&self.o.newline);
            for _ in 0..depth {
                self.out.push_str(&self.o.indent_chars);
            }
        }
    }

    fn flush(&mut self, close: bool) -> Result<(), XmlWriteError> {
        let Some(p) = self.pending.as_ref() else {
            return Ok(());
        };
        let qname = match p.ns.as_deref() {
            None => p.local.clone(),
            Some(uri) => match self.find_prefix(uri, true) {
                Some("") => p.local.clone(),
                Some(pre) => format!("{pre}:{}", p.local),
                None => return Err(XmlWriteError::UnboundPrefix),
            },
        };
        let mut tag = format!("<{qname}");
        for (pre, uri) in &p.decls {
            if pre.is_empty() {
                tag.push_str(" xmlns=\"");
            } else {
                tag.push_str(&format!(" xmlns:{pre}=\""));
            }
            esc(&mut tag, uri, true);
            tag.push('"');
        }
        for (local, ns, value) in &p.attrs {
            let name = match ns.as_deref() {
                None => local.clone(),
                Some(uri) => match self.find_prefix(uri, false) {
                    Some(pre) => format!("{pre}:{local}"),
                    None => return Err(XmlWriteError::UnboundPrefix),
                },
            };
            tag.push_str(&format!(" {name}=\""));
            esc(&mut tag, value, true);
            tag.push('"');
        }
        tag.push_str(if close { "/>" } else { ">" });
        self.pending = None;
        self.out.push_str(&tag);
        if let Some(e) = self.stack.last_mut() {
            e.qname = qname;
        }
        Ok(())
    }

    fn before_child(&mut self) -> Result<(), XmlWriteError> {
        self.flush(false)?;
        let depth = self.stack.len();
        let inline = match self.stack.last_mut() {
            Some(e) => {
                e.has_children = true;
                e.has_text
            }
            None => false,
        };
        if !inline {
            self.break_line(depth);
        }
        Ok(())
    }

    pub fn declaration(&mut self, encoding: Option<&str>) -> Result<(), XmlWriteError> {
        self.declaration_version("1.0", encoding)
    }

    pub fn declaration_version(
        &mut self,
        version: &str,
        encoding: Option<&str>,
    ) -> Result<(), XmlWriteError> {
        if !self.out.is_empty() || !self.stack.is_empty() {
            return Err(XmlWriteError::Order);
        }
        check(version)?;
        if let Some(e) = encoding {
            check(e)?;
            self.out
                .push_str(&format!("<?xml version=\"{version}\" encoding=\"{e}\"?>"));
        } else {
            self.out.push_str(&format!("<?xml version=\"{version}\"?>"));
        }
        Ok(())
    }

    pub fn start_element(&mut self, local: &str, ns: Option<&str>) -> Result<(), XmlWriteError> {
        check(local)?;
        self.before_child()?;
        self.stack.push(Elem {
            qname: String::new(),
            scope_mark: self.scope_log.len(),
            has_children: false,
            has_text: false,
        });
        self.pending = Some(Pending {
            local: local.to_string(),
            ns: ns.filter(|u| !u.is_empty()).map(str::to_string),
            decls: Vec::new(),
            attrs: Vec::new(),
        });
        Ok(())
    }

    pub fn namespace(&mut self, prefix: &str, uri: &str) -> Result<(), XmlWriteError> {
        check(prefix)?;
        check(uri)?;
        let p = self.pending.as_mut().ok_or(XmlWriteError::Order)?;
        if let Some(d) = p.decls.iter_mut().find(|d| d.0 == prefix) {
            d.1 = uri.to_string();
        } else {
            p.decls.push((prefix.to_string(), uri.to_string()));
        }
        self.bind(prefix, uri);
        Ok(())
    }

    pub fn attribute(
        &mut self,
        local: &str,
        ns: Option<&str>,
        value: &str,
    ) -> Result<(), XmlWriteError> {
        check(local)?;
        check(value)?;
        let p = self.pending.as_mut().ok_or(XmlWriteError::Order)?;
        p.attrs.push((
            local.to_string(),
            ns.filter(|u| !u.is_empty()).map(str::to_string),
            value.to_string(),
        ));
        Ok(())
    }

    fn content(&mut self, check_s: &str) -> Result<(), XmlWriteError> {
        check(check_s)?;
        if self.stack.is_empty() {
            return if self.root_closed {
                Ok(())
            } else {
                Err(XmlWriteError::Order)
            };
        }
        self.flush(false)?;
        if let Some(e) = self.stack.last_mut() {
            e.has_text = true;
        }
        Ok(())
    }

    pub fn text(&mut self, s: &str) -> Result<(), XmlWriteError> {
        check(s)?;
        if self.stack.is_empty() && !self.root_closed {
            return Err(XmlWriteError::Order);
        }
        if s.is_empty() {
            return Ok(());
        }
        self.content(s)?;
        esc(&mut self.out, s, false);
        Ok(())
    }

    pub fn raw(&mut self, s: &str) -> Result<(), XmlWriteError> {
        if s.is_empty() {
            check(s)?;
            return Ok(());
        }
        self.content(s)?;
        self.out.push_str(s);
        Ok(())
    }

    pub fn comment(&mut self, s: &str) -> Result<(), XmlWriteError> {
        check(s)?;
        self.before_child()?;
        self.out.push_str("<!--");
        self.out.push_str(s);
        self.out.push_str("-->");
        Ok(())
    }

    pub fn instruction(&mut self, target: &str, data: &str) -> Result<(), XmlWriteError> {
        check(target)?;
        check(data)?;
        self.before_child()?;
        self.out.push_str("<?");
        self.out.push_str(target);
        if !data.is_empty() {
            self.out.push(' ');
            self.out.push_str(data);
        }
        self.out.push_str("?>");
        Ok(())
    }

    pub fn cdata(&mut self, s: &str) -> Result<(), XmlWriteError> {
        self.content(s)?;
        self.out.push_str("<![CDATA[");
        self.out.push_str(&s.replace("]]>", "]]]]><![CDATA[>"));
        self.out.push_str("]]>");
        Ok(())
    }

    pub fn end_element(&mut self) -> Result<(), XmlWriteError> {
        let Some(e) = self.stack.last() else {
            return Err(XmlWriteError::Order);
        };
        let mark = e.scope_mark;
        if self.pending.is_some() {
            self.flush(true)?;
        } else {
            let inline = e.has_text || !e.has_children;
            let depth = self.stack.len() - 1;
            if !inline {
                self.break_line(depth);
            }
            let q = self
                .stack
                .last()
                .map(|e| e.qname.clone())
                .unwrap_or_default();
            self.out.push_str(&format!("</{q}>"));
        }
        self.stack.pop();
        self.unwind(mark);
        self.root_closed = self.stack.is_empty();
        Ok(())
    }

    pub fn close_start_tag(&mut self) -> Result<(), XmlWriteError> {
        self.flush(false)
    }

    pub fn finish(mut self) -> Result<String, XmlWriteError> {
        self.flush(false)?;
        Ok(self.out)
    }
}
