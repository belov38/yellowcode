use brace::Node;

use super::variant::{self, Layout, NoLayout, Section, Serialization};

pub(crate) const FORMID: &str = "02023637-7868-4a5f-8576-835a76e0c9ba";

const CLASSES: [(&str, &str); 6] = [
    ("cd5394d0-7dda-4b56-8927-93ccbe967a01", "Group"),
    ("77ffcc29-7f2d-4223-b22f-19666e7250ba", "Field"),
    ("3d3cb80c-508b-41fa-8a18-680cdf5f1712", "Decoration"),
    ("a9f3b1ac-f51b-431e-b102-55a69acdecad", "Button"),
    ("143c00f7-a42d-4cd7-9189-88e4467dc768", "Table"),
    ("c5259a1d-518a-4afd-b98d-0176027e4feb", "Addition"),
];

pub(crate) const NESTED: &str = "<ITEM>";

const MAX_DEPTH: usize = 256;

pub(crate) fn class_of(guid: &str) -> Option<&'static str> {
    CLASSES.iter().find(|(g, _)| *g == guid).map(|(_, c)| *c)
}

fn class_guid(class: &str) -> Option<&'static str> {
    CLASSES.iter().find(|(_, c)| *c == class).map(|(g, _)| *g)
}

pub(crate) fn kind(class: &str, code: &str) -> Option<&'static str> {
    Some(match (class, code) {
        ("Group", "5") => "UsualGroup",
        ("Group", "6") => "ButtonGroup",
        ("Group", "2") => "ColumnGroup",
        ("Group", "1") => "Popup",
        ("Group", "3") => "Pages",
        ("Group", "4") => "Page",
        ("Group", "0") => "CommandBar",
        ("Field", "1") => "LabelField",
        ("Field", "2") => "InputField",
        ("Field", "3") => "CheckBoxField",
        ("Field", "4") => "PictureField",
        ("Field", "6") => "SpreadSheetDocumentField",
        ("Field", "5") => "RadioButtonField",
        ("Field", "7") => "TextDocumentField",
        ("Field", "8") => "CalendarField",
        ("Field", "17") => "FormattedDocumentField",
        ("Field", "11") => "ChartField",
        ("Field", "19") => "PlannerField",
        ("Field", "9") => "ProgressBarField",
        ("Field", "15") => "HTMLDocumentField",
        ("Field", "10") => "TrackBarField",
        ("Field", "20") => "PDFDocumentField",
        ("Field", "14") => "GraphicalSchemaField",
        ("Field", "16") => "GeographicalSchemaField",
        ("Decoration", "1") => "PictureDecoration",
        ("Decoration", "0") => "LabelDecoration",
        ("Addition", "0") => "SearchStringAddition",
        ("Addition", "2") => "SearchControlAddition",
        ("Addition", "1") => "ViewStatusAddition",
        _ => return None,
    })
}

pub(crate) fn in_kind(class: &str) -> bool {
    matches!(class, "Group" | "Field" | "Decoration" | "Addition")
}

pub(crate) fn nested_at(class: &str, pos: usize) -> Option<&'static str> {
    Some(match (class, pos) {
        ("Field", 42) | ("Decoration", 20) | ("Addition", 15) | ("Table", 58) => "ContextMenu",
        ("Field", 52)
        | ("Decoration", 26)
        | ("Group", 25)
        | ("Button", 32)
        | ("Addition", 17)
        | ("Table", 73) => "ExtendedTooltip",
        ("Field", 58) | ("Table", 80) => "ViewStatusAddition",
        ("Table", 60) => "AutoCommandBar",
        ("Table", 78) => "SearchStringAddition",
        ("Table", 82) => "SearchControlAddition",
        _ => return None,
    })
}

pub(crate) fn item_tag(it: &Item, parent_class: Option<&str>, pos: Option<usize>) -> &'static str {
    if let Some(c) = it.class {
        if in_kind(c) {
            let code = it.body.get(5).and_then(Node::text).unwrap_or("");
            return kind(c, code).unwrap_or(c);
        }
        return c;
    }
    if let (Some(pc), Some(p)) = (parent_class, pos) {
        let key = if pc == "nested" { "Addition" } else { pc };
        if let Some(n) = nested_at(key, p) {
            return n;
        }
    }
    "AutoCommandBar"
}

pub(crate) fn is_item(n: &Node) -> bool {
    match n.as_list() {
        Some(l) => {
            l.len() > 6
                && l.get(1)
                    .and_then(Node::as_list)
                    .is_some_and(|p| p.len() == 2 && p.get(1).and_then(Node::text) == Some(FORMID))
        }
        None => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Item {
    pub id_text: String,
    pub class: Option<&'static str>,
    pub version: String,
    pub body: Vec<Node>,
    pub children: Vec<Item>,
    pub nested: Vec<(usize, Item)>,
    pub children_at: Option<usize>,
    pub opt: Vec<(&'static str, Node)>,
    pub bag: Vec<(String, Node)>,
    pub bag_at: Option<usize>,
}

impl Item {
    pub(crate) fn parse_in(
        raw: &Node,
        class: Option<&'static str>,
        layout: &'static Layout,
    ) -> Option<Item> {
        Self::parse_at(raw, class, layout, 0)
    }

    fn parse_at(
        raw: &Node,
        class: Option<&'static str>,
        layout: &'static Layout,
        depth: usize,
    ) -> Option<Item> {
        if depth > MAX_DEPTH {
            return None;
        }
        let list = raw.as_list()?;
        let version = list.first()?.text()?.to_string();
        let id_text = list.get(1)?.at(0)?.text()?.to_string();
        let mut it = Item {
            id_text,
            class,
            version,
            body: Vec::new(),
            children: Vec::new(),
            nested: Vec::new(),
            children_at: None,
            opt: Vec::new(),
            bag: Vec::new(),
            bag_at: None,
        };
        let mut i = 0;
        while let Some(e) = list.get(i) {
            if let Some(cls) = e.text().and_then(class_of)
                && let Some(next) = list.get(i + 1)
                && is_item(next)
            {
                if it.children_at.is_none() {
                    it.children_at = Some(it.body.len().checked_sub(1)?);
                }
                it.children
                    .push(Item::parse_at(next, Some(cls), layout, depth + 1)?);
                i += 2;
                continue;
            }
            if is_item(e) {
                it.nested
                    .push((it.body.len(), Item::parse_at(e, None, layout, depth + 1)?));
                it.body.push(Node::atom(NESTED));
                i += 1;
                continue;
            }
            it.body.push(e.clone());
            i += 1;
        }
        let uv = if class == Some("Button") { 3 } else { 4 };
        if it.body.get(uv).and_then(Node::text) == Some("1")
            && it.body.get(uv + 1).is_some_and(Node::is_list)
        {
            let v = it.body.remove(uv + 1);
            it.opt.push(("UserVisible", v));
            it.shift(uv + 1, -1);
        }
        it.bag_at = layout.bag_at(class, &it.version);
        if let Some(at) = it.bag_at
            && let Some(cnt) = it.body.get(at)
        {
            let n = usize::try_from(cnt.as_i64()?).ok()?;
            if n > it.body.len() {
                return None;
            }
            for j in 0..n {
                let k = it.body.get(at + 1 + 2 * j)?.text()?.to_string();
                let v = it.body.get(at + 2 + 2 * j)?.clone();
                it.bag.push((k, v));
            }
            it.body.drain(at + 1..at + 1 + 2 * n);
            it.shift(at + 1, -2 * isize::try_from(n).ok()?);
        }
        if !layout.accepts_item(class, &it.version, it.body.len()) {
            return None;
        }
        if layout.strict
            && let Some(c) = class
            && in_kind(c)
            && kind(c, it.body.get(5).and_then(Node::text).unwrap_or("")).is_none()
        {
            return None;
        }
        Some(it)
    }

    fn shift(&mut self, start: usize, delta: isize) {
        let mv = |k: usize| k.checked_add_signed(delta).unwrap_or(k);
        for (k, _) in &mut self.nested {
            if *k >= start {
                *k = mv(*k);
            }
        }
        if let Some(c) = self.children_at.as_mut()
            && *c >= start
        {
            *c = mv(*c);
        }
    }

    pub(crate) fn name(&self) -> Option<&str> {
        self.body
            .get(2..12.min(self.body.len()))?
            .iter()
            .find_map(Node::as_str)
    }

    pub(crate) fn walk<'a>(&'a self, out: &mut Vec<&'a Item>) {
        out.push(self);
        for (_, n) in &self.nested {
            n.walk(out);
        }
        for c in &self.children {
            c.walk(out);
        }
    }

    pub(crate) fn to_raw(&self) -> Node {
        let mut toks: Vec<(Node, bool, Option<usize>)> = self
            .body
            .iter()
            .enumerate()
            .map(|(k, v)| (v.clone(), self.children_at == Some(k), Some(k)))
            .collect();
        if let Some(at) = self.bag_at
            && !self.bag.is_empty()
        {
            let ins: Vec<_> = self
                .bag
                .iter()
                .flat_map(|(k, v)| {
                    [
                        (Node::atom(k.clone()), false, None),
                        (v.clone(), false, None),
                    ]
                })
                .collect();
            let p = (at + 1).min(toks.len());
            toks.splice(p..p, ins);
        }
        if let Some((_, v)) = self.opt.iter().find(|(k, _)| *k == "UserVisible") {
            let uv = if self.class == Some("Button") { 3 } else { 4 };
            let p = (uv + 1).min(toks.len());
            toks.insert(p, (v.clone(), false, None));
        }
        let mut raw = Vec::with_capacity(toks.len());
        for (v, kids, bi) in toks {
            let nested = bi.and_then(|b| self.nested.iter().find(|(p, _)| *p == b));
            match nested {
                Some((_, n)) => raw.push(n.to_raw()),
                None => raw.push(v),
            }
            if kids {
                for c in &self.children {
                    if let Some(g) = c.class.and_then(class_guid) {
                        raw.push(Node::atom(g));
                    }
                    raw.push(c.to_raw());
                }
            }
        }
        Node::List(raw)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rec {
    pub class: &'static str,
    pub raw: Node,
    pub body: Vec<Node>,
    pub id: Option<i64>,
    pub name: String,
    pub bag: Vec<(String, Node)>,
    pub opt: Vec<(&'static str, Node)>,
    pub nested: Vec<(usize, Item)>,
    pub children: Vec<Rec>,
}

impl Rec {
    pub(crate) fn new(
        class: &'static str,
        raw: &Node,
        body: Vec<Node>,
        id: Option<i64>,
        name: String,
    ) -> Rec {
        Rec {
            class,
            raw: raw.clone(),
            body,
            id,
            name,
            bag: Vec::new(),
            opt: Vec::new(),
            nested: Vec::new(),
            children: Vec::new(),
        }
    }
}

fn section(n: &Node) -> Option<&[Node]> {
    let l = n.as_list()?;
    let cnt = usize::try_from(l.get(1)?.as_i64()?).ok()?;
    let rows = l.get(2..)?;
    Some(rows.get(..cnt.min(rows.len())).unwrap_or(rows))
}

fn rec_id(r: &Node) -> Option<i64> {
    r.at(1)?.at(0)?.text()?.parse().ok()
}

pub(crate) fn attr_records(attributes: &Node) -> Option<Vec<Rec>> {
    let mut out = Vec::new();
    for r in section(attributes)? {
        let l = r.as_list()?;
        let n = usize::try_from(l.get(13)?.as_i64()?).ok()?;
        let cols_src = l.get(14..)?;
        let cols_src = cols_src.get(..n.min(cols_src.len())).unwrap_or(cols_src);
        let mut cols = Vec::new();
        for c in cols_src {
            let cl = c.as_list()?;
            cols.push(Rec::new(
                "Column",
                c,
                cl.to_vec(),
                Some(cl.get(1)?.text()?.parse().ok()?),
                cl.get(3)?.text()?.to_string(),
            ));
        }
        let mut body: Vec<Node> = l.get(..14)?.to_vec();
        body.extend(l.get(14 + n..).unwrap_or(&[]).iter().cloned());
        let mut rec = Rec::new(
            "Attribute",
            r,
            body,
            Some(rec_id(r)?),
            l.get(3)?.text()?.to_string(),
        );
        rec.children = cols;
        out.push(rec);
    }
    Some(out)
}

pub(crate) fn command_records(commands: &Node) -> Option<Vec<Rec>> {
    section(commands)?
        .iter()
        .map(|r| {
            let l = r.as_list()?;
            Some(Rec::new(
                "Command",
                r,
                l.to_vec(),
                Some(rec_id(r)?),
                l.get(2)?.text()?.to_string(),
            ))
        })
        .collect()
}

pub(crate) fn param_records(parameters: &Node) -> Option<Vec<Rec>> {
    section(parameters)?
        .iter()
        .map(|r| {
            let l = r.as_list()?;
            Some(Rec::new(
                "Parameter",
                r,
                l.to_vec(),
                None,
                l.get(1)?.text()?.to_string(),
            ))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DecodedForm {
    pub root: Node,
    pub form: Item,
    pub module_text: String,
    pub attrs: Vec<Rec>,
    pub params: Vec<Rec>,
    pub commands: Vec<Rec>,
    pub layout: &'static Layout,
}

impl DecodedForm {
    pub(crate) fn acb(&self) -> Option<&Item> {
        self.form.nested.first().map(|(_, i)| i)
    }

    pub(crate) fn ext(&self) -> Vec<(&str, &Node)> {
        let k = self.form.body.get(18).and_then(Node::as_i64).unwrap_or(0);
        let k = usize::try_from(k).unwrap_or(0).min(self.form.body.len());
        (0..k)
            .filter_map(|i| {
                Some((
                    self.form.body.get(19 + 2 * i)?.text()?,
                    self.form.body.get(20 + 2 * i)?,
                ))
            })
            .collect()
    }

    pub(crate) fn head(&self) -> &[Node] {
        self.form.body.get(1..18).unwrap_or_default()
    }

    pub(crate) fn tail(&self) -> &[Node] {
        self.form
            .children_at
            .and_then(|c| self.form.body.get(c.checked_add(1)?..))
            .unwrap_or_default()
    }

    pub(crate) fn ext_get(&self, key: &str) -> Option<&Node> {
        self.ext()
            .into_iter()
            .rev()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    pub(crate) fn events(&self) -> Option<&Node> {
        let k = usize::try_from(self.form.body.get(18)?.as_i64()?).ok()?;
        self.form.body.get(k.checked_mul(2)?.checked_add(19)?)
    }

    pub(crate) fn command_set(&self) -> Option<&Node> {
        let k = usize::try_from(self.form.body.get(18)?.as_i64()?).ok()?;
        self.form.body.get(k.checked_mul(2)?.checked_add(20)?)
    }

    pub(crate) fn items(&self) -> Vec<&Item> {
        let mut out = Vec::new();
        if let Some(a) = self.acb() {
            a.walk(&mut out);
        }
        for c in &self.form.children {
            c.walk(&mut out);
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    Shape,
    Unsupported(Serialization),
    Rejected(&'static Layout),
}

pub(crate) fn decode_form(entry: &Node) -> Option<DecodedForm> {
    decode_form_checked(entry).ok()
}

pub(crate) fn decode_form_checked(entry: &Node) -> Result<DecodedForm, Failure> {
    decode_form_with(entry, super::tables::TableView::embedded(false))
}

pub(crate) fn decode_form_with(
    entry: &Node,
    events: super::tables::TableView<'_>,
) -> Result<DecodedForm, Failure> {
    let layout = match variant::detect(entry) {
        Ok(l) => l,
        Err(NoLayout::Shape) => return Err(Failure::Shape),
        Err(NoLayout::Unsupported(s)) => return Err(Failure::Unsupported(s)),
    };
    decode_in(entry, layout, events).ok_or(Failure::Rejected(layout))
}

pub(crate) fn strict_sections(layout: &Layout, t: &[Node]) -> Option<()> {
    if !layout.strict {
        return Some(());
    }
    type Check = (usize, Section, usize, fn(&Node) -> bool);
    let checks: [Check; 3] = [
        (3, Section::Attribute, 10, |n| n.as_bool().is_some()),
        (4, Section::Parameter, 3, |n| n.as_bool().is_some()),
        (5, Section::Command, 8, |n| n.as_str().is_some()),
    ];
    for (at, section_kind, flag, ok) in checks {
        for r in section(t.get(at)?)? {
            let l = r.as_list()?;
            if !layout.accepts_record(section_kind, l.first().and_then(Node::text))
                || !ok(l.get(flag)?)
            {
                return None;
            }
        }
    }
    Some(())
}

fn strict_events(
    layout: &Layout,
    tables: super::tables::TableView<'_>,
    events: Option<&Node>,
) -> Option<()> {
    if !layout.strict {
        return Some(());
    }
    match super::codecs_a::event_list(tables, events) {
        Ok(Some(_)) => Some(()),
        _ => None,
    }
}

fn decode_in(
    entry: &Node,
    layout: &'static Layout,
    tables: super::tables::TableView<'_>,
) -> Option<DecodedForm> {
    let t = entry.as_list()?;
    let f = t.get(1)?.as_list()?;
    let k = usize::try_from(f.get(18)?.as_i64()?).ok()?;
    let i = 19usize.checked_add(k.checked_mul(2)?)?;
    if k > f.len() {
        return None;
    }
    let at = |d: usize| i.checked_add(d);
    let acb_raw = f.get(at(3)?)?;
    let cnt = usize::try_from(f.get(at(4)?)?.as_i64()?).ok()?;
    if cnt > f.len() {
        return None;
    }
    let mut form = Item {
        id_text: t
            .get(1)
            .and_then(|r| r.get(&[1, 0]))
            .and_then(Node::text)
            .map_or_else(|| "0".to_string(), String::from),
        class: None,
        version: f.first()?.text()?.to_string(),
        body: f.get(..at(3)?)?.to_vec(),
        children: Vec::new(),
        nested: Vec::new(),
        children_at: None,
        opt: Vec::new(),
        bag: Vec::new(),
        bag_at: None,
    };
    form.nested
        .push((at(3)?, Item::parse_in(acb_raw, None, layout)?));
    form.body.push(Node::atom(NESTED));
    form.body.push(f.get(at(4)?)?.clone());
    form.children_at = Some(at(4)?);
    for j in 0..cnt {
        let base = at(5)?.checked_add(j.checked_mul(2)?)?;
        let g = f.get(base)?.text()?;
        let it = f.get(base.checked_add(1)?)?;
        form.children
            .push(Item::parse_in(it, Some(class_of(g)?), layout)?);
    }
    let tail = at(5)?.checked_add(cnt.checked_mul(2)?)?;
    let tail_nodes = f.get(tail..)?;
    if !layout.accepts_tail(tail_nodes.len()) {
        return None;
    }
    form.body.extend(tail_nodes.iter().cloned());
    let module_text = t.get(2)?.text()?.to_string();
    strict_sections(layout, t)?;
    strict_events(layout, tables, f.get(i))?;
    Some(DecodedForm {
        layout,
        attrs: attr_records(t.get(3)?)?,
        params: param_records(t.get(4)?)?,
        commands: command_records(t.get(5)?)?,
        module_text,
        form,
        root: entry.clone(),
    })
}
