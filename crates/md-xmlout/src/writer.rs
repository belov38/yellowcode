use md_model::XmlElem;

pub const NS_DECL: &str = concat!(
    "xmlns=\"http://v8.1c.ru/8.3/MDClasses\" xmlns:app=\"http://v8.1c.ru/8.2/managed-application/core\" ",
    "xmlns:cfg=\"http://v8.1c.ru/8.1/data/enterprise/current-config\" ",
    "xmlns:cmi=\"http://v8.1c.ru/8.2/managed-application/cmi\" xmlns:ent=\"http://v8.1c.ru/8.1/data/enterprise\" ",
    "xmlns:lf=\"http://v8.1c.ru/8.2/managed-application/logform\" ",
    "xmlns:pal=\"http://v8.1c.ru/8.1/data/ui/colors/palette\" xmlns:style=\"http://v8.1c.ru/8.1/data/ui/style\" ",
    "xmlns:sys=\"http://v8.1c.ru/8.1/data/ui/fonts/system\" xmlns:v8=\"http://v8.1c.ru/8.1/data/core\" ",
    "xmlns:v8ui=\"http://v8.1c.ru/8.1/data/ui\" xmlns:web=\"http://v8.1c.ru/8.1/data/ui/colors/web\" ",
    "xmlns:win=\"http://v8.1c.ru/8.1/data/ui/colors/windows\" xmlns:xen=\"http://v8.1c.ru/8.3/xcf/enums\" ",
    "xmlns:xpr=\"http://v8.1c.ru/8.3/xcf/predef\" xmlns:xr=\"http://v8.1c.ru/8.3/xcf/readable\" ",
    "xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\""
);
pub const DUMP_VERSION: &str = "2.21";

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn esc_attr(s: &str) -> String {
    esc(s).replace('"', "&quot;")
}

#[derive(Debug, Clone, Default)]
pub struct XmlWriter {
    lines: Vec<String>,
    depth: usize,
}

fn attrs_str(attrs: &[(&str, &str)]) -> String {
    attrs
        .iter()
        .map(|(k, v)| format!(" {k}=\"{}\"", esc_attr(v)))
        .collect()
}

impl XmlWriter {
    pub fn new(depth: usize) -> Self {
        XmlWriter {
            lines: Vec::new(),
            depth,
        }
    }

    fn push(&mut self, line: &str) {
        self.lines
            .push(format!("{}{line}", "\t".repeat(self.depth)));
    }

    pub fn open(&mut self, tag: &str, attrs: &[(&str, &str)]) {
        self.push(&format!("<{tag}{}>", attrs_str(attrs)));
        self.depth += 1;
    }

    pub fn close(&mut self, tag: &str) {
        self.depth = self.depth.saturating_sub(1);
        self.push(&format!("</{tag}>"));
    }

    pub fn leaf(&mut self, tag: &str, text: Option<&str>, attrs: &[(&str, &str)]) {
        let a = attrs_str(attrs);
        match text {
            None | Some("") => self.push(&format!("<{tag}{a}/>")),
            Some(t) => self.push(&format!("<{tag}{a}>{}</{tag}>", esc(t))),
        }
    }

    pub fn raw(&mut self, line: &str) {
        self.push(line);
    }

    pub fn elem(&mut self, e: &XmlElem) {
        self.elem_as(&e.name, e);
    }

    pub(crate) fn elem_as(&mut self, name: &str, e: &XmlElem) {
        let attrs: Vec<(&str, &str)> = e
            .attrs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        if e.children.is_empty() && !e.expanded {
            self.leaf(name, e.text.as_deref(), &attrs);
        } else {
            self.open(name, &attrs);
            for c in &e.children {
                self.elem(c);
            }
            self.close(name);
        }
    }

    pub fn mark(&self) -> usize {
        self.lines.len()
    }

    pub fn truncate(&mut self, mark: usize) {
        self.lines.truncate(mark);
    }

    pub fn depth(&self) -> usize {
        self.depth
    }

    pub fn text(&self) -> String {
        self.lines.join("\r\n")
    }
}

pub fn document(body: &XmlWriter) -> String {
    let mut lines = vec![
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_string(),
        format!("<MetaDataObject {NS_DECL} version=\"{DUMP_VERSION}\">"),
    ];
    lines.extend(body.lines.iter().cloned());
    lines.push("</MetaDataObject>".into());
    lines.join("\r\n")
}
