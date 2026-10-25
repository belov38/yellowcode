use super::XML_HEAD;
use crate::paths::{object_dir, sub_dir};
use crate::{DumpSink, Part, PartStatus, Result};
use brace::Guid;
use md_model::parts::*;
use md_model::*;
use regex::bytes::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

const GUID_RE: &str = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";

fn re(pattern: &str) -> Option<Regex> {
    Regex::new(pattern).ok()
}

static COMMENT: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"(?s-u)<!--.*?-->"));
static TAG: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"(?is-u)<(a|img)\b[^>]*>"));
static HREF: LazyLock<Option<Regex>> = LazyLock::new(|| re(r#"(?is-u)(\shref=")([^"]*)(")"#));
static SRC: LazyLock<Option<Regex>> = LazyLock::new(|| re(r#"(?is-u)(\ssrc=")([^"]*)(")"#));
static OBJECT_LINK: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(&format!(r"(?s-u)\A\.\./id({GUID_RE})/{GUID_RE}(.*)\z")));
static PICTURE_LINK: LazyLock<Option<Regex>> = LazyLock::new(|| {
    re(&format!(
        r"(?s-u)\A\.\./\.\./mdpicture/id({GUID_RE})/{GUID_RE}\z"
    ))
});
static STD_LINK: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"(?s-u)\A\.\./\.\./mdpicture/idn(-[0-9]+)\z"));
static FILE_LINK: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(&format!(r"(?s-u)\A{GUID_RE}_files/(.*)\z")));

pub struct Linker<'a> {
    pub cfg: &'a Configuration,
    pub schema: Option<&'a mdschema::Schema>,
    pub files: HashSet<String>,
}

impl Linker<'_> {
    fn name_of(&self, u: &[u8]) -> Option<String> {
        let g = Guid::parse(std::str::from_utf8(u).ok()?)?;
        match self.cfg.full_name(g) {
            Some(n) => Some(n.to_string()),
            None if g == self.cfg.id => Some(format!("Configuration.{}", self.cfg.settings.name)),
            None => None,
        }
    }

    fn std_picture(&self, key: &[u8]) -> Option<Vec<u8>> {
        let name = self.schema?.std_picture(std::str::from_utf8(key).ok()?)?;
        Some(format!("StdPicture.{name}").into_bytes())
    }

    fn fix_value(&self, v: &[u8]) -> Vec<u8> {
        if let Some(c) = OBJECT_LINK.as_ref().and_then(|r| r.captures(v)) {
            let (Some(u), Some(rest)) = (c.get(1), c.get(2)) else {
                return v.to_vec();
            };
            let Some(nm) = self.name_of(u.as_bytes()) else {
                return v.to_vec();
            };
            let parts: Vec<&str> = nm.split('.').collect();
            let template = nm.starts_with("CommonTemplate.")
                || parts.len() >= 2 && parts.get(parts.len() - 2) == Some(&"Template");
            let sub = if template { "Template" } else { "Help" };
            let mut out = format!("{nm}/{sub}").into_bytes();
            out.extend_from_slice(rest.as_bytes());
            return out;
        }
        if let Some(c) = PICTURE_LINK.as_ref().and_then(|r| r.captures(v)) {
            let Some(u) = c.get(1) else {
                return v.to_vec();
            };
            return self
                .name_of(u.as_bytes())
                .map(String::into_bytes)
                .or_else(|| self.std_picture(u.as_bytes()))
                .unwrap_or_else(|| v.to_vec());
        }
        if let Some(c) = STD_LINK.as_ref().and_then(|r| r.captures(v)) {
            let Some(k) = c.get(1) else {
                return v.to_vec();
            };
            return self.std_picture(k.as_bytes()).unwrap_or_else(|| v.to_vec());
        }
        if let Some(name) = FILE_LINK
            .as_ref()
            .and_then(|r| r.captures(v))
            .and_then(|c| c.get(1))
            .filter(|n| {
                self.files
                    .contains(String::from_utf8_lossy(n.as_bytes()).as_ref())
            })
        {
            let mut out = b"_files/".to_vec();
            out.extend_from_slice(name.as_bytes());
            return out;
        }
        v.to_vec()
    }

    fn fix_tag(&self, tag: &[u8], is_a: bool) -> Vec<u8> {
        let re = if is_a { &HREF } else { &SRC };
        let Some(c) = re.as_ref().and_then(|r| r.captures(tag)) else {
            return tag.to_vec();
        };
        let (Some(all), Some(pre), Some(val), Some(post)) =
            (c.get(0), c.get(1), c.get(2), c.get(3))
        else {
            return tag.to_vec();
        };
        let mut out = tag.get(..all.start()).unwrap_or_default().to_vec();
        out.extend_from_slice(pre.as_bytes());
        out.extend(self.fix_value(val.as_bytes()));
        out.extend_from_slice(post.as_bytes());
        out.extend_from_slice(tag.get(all.end()..).unwrap_or_default());
        out
    }

    fn fix_part(&self, part: &[u8]) -> Vec<u8> {
        match TAG.as_ref() {
            Some(tag) => tag
                .replace_all(part, |c: &regex::bytes::Captures| {
                    let whole = c.get(0).map_or(&[][..], |m| m.as_bytes());
                    let is_a = c
                        .get(1)
                        .is_some_and(|m| m.as_bytes().eq_ignore_ascii_case(b"a"));
                    self.fix_tag(whole, is_a)
                })
                .into_owned(),
            None => part.to_vec(),
        }
    }

    pub fn fix_links(&self, html: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(html.len());
        let mut pos = 0;
        if let Some(comment) = COMMENT.as_ref() {
            for m in comment.find_iter(html) {
                out.extend(self.fix_part(html.get(pos..m.start()).unwrap_or_default()));
                out.extend_from_slice(m.as_bytes());
                pos = m.end();
            }
        }
        out.extend(self.fix_part(html.get(pos..).unwrap_or_default()));
        out
    }
}

pub fn help_files(cfg: &Configuration, h: &Help) -> Vec<(String, Vec<u8>)> {
    let linker = Linker {
        cfg,
        schema: mdschema::schema().ok(),
        files: h.files.iter().map(|(n, _)| n.clone()).collect(),
    };
    let mut out = Vec::new();
    for (lang, html) in &h.pages {
        let fixed = linker.fix_links(html);
        out.push((format!("Help/{lang}.html"), crlf_to_lf(&fixed)));
    }
    for (name, data) in &h.files {
        out.push((format!("Help/_files/{name}"), data.clone()));
    }
    let mut xml = format!(
        "{XML_HEAD}\r\n<Help xmlns=\"http://v8.1c.ru/8.3/xcf/extrnprops\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" version=\"2.21\">\r\n"
    );
    for (lang, _) in &h.pages {
        xml.push_str(&format!("\t<Page>{lang}</Page>\r\n"));
    }
    xml.push_str("</Help>");
    out.push(("Help.xml".to_string(), xml.into_bytes()));
    out
}

fn crlf_to_lf(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'\r' && b.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn put_help(cfg: &Configuration, sink: &mut DumpSink, dir: &str, h: &Help) -> Result<()> {
    for (rel, data) in help_files(cfg, h) {
        let path = if dir.is_empty() {
            format!("Ext/{rel}")
        } else {
            format!("{dir}/Ext/{rel}")
        };
        sink.put(Part::Help, &path, data.as_slice())?;
    }
    Ok(())
}

pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    let mut stack: Vec<&MdObject> = cfg.objects.iter().rev().collect();
    while let Some(o) = stack.pop() {
        if let Some(h) = &o.parts.help {
            let dir = object_dir(cfg, o);
            if !dir.is_empty() {
                put_help(cfg, sink, &dir, h)?;
            }
        }
        for (fid, h) in &o.parts.form_help {
            if let Some(f) = o.forms.iter().find(|f| f.id == *fid) {
                put_help(cfg, sink, &sub_dir(cfg, o, f), h)?;
            }
        }
        stack.extend(o.subsystems.iter().rev());
    }
    if let Some(h) = &cfg.parts.help {
        put_help(cfg, sink, "", h)?;
    }
    Ok(PartStatus::Written)
}
