use super::values::{self as v, GraphError, Names, R, count, is, slice};
use crate::Configuration;
use brace::Node;
use std::collections::HashMap;

const ROOT_NS: &str = concat!(
    "xmlns=\"http://v8.1c.ru/8.3/xcf/scheme\" xmlns:pal=\"http://v8.1c.ru/8.1/data/ui/colors/palette\" ",
    "xmlns:sch=\"http://v8.1c.ru/8.2/data/graphscheme\" xmlns:style=\"http://v8.1c.ru/8.1/data/ui/style\" ",
    "xmlns:v8=\"http://v8.1c.ru/8.1/data/core\" xmlns:v8ui=\"http://v8.1c.ru/8.1/data/ui\" ",
    "xmlns:web=\"http://v8.1c.ru/8.1/data/ui/colors/web\" xmlns:win=\"http://v8.1c.ru/8.1/data/ui/colors/windows\" ",
    "xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" ",
    "version=\"2.21\""
);
const ZERO: &str = "00000000-0000-0000-0000-000000000000";

fn kind_tag(kind: i64) -> Option<&'static str> {
    Some(match kind {
        0 => "Decoration",
        1 => "ConnectionLine",
        2 => "Start",
        3 => "Completion",
        4 => "Condition",
        5 => "Activity",
        6 => "Switch",
        7 => "Split",
        8 => "Join",
        9 => "Processing",
        10 => "SubBusinessProcess",
        _ => return None,
    })
}

fn events_of(kind: i64) -> Option<&'static [&'static str]> {
    Some(match kind {
        2 => &["BeforeStart"],
        3 => &["OnComplete"],
        4 => &["ConditionCheck"],
        6 => &["SwitchProcessing"],
        9 => &["Processing"],
        5 => &[
            "InteractiveActivationProcessing",
            "BeforeCreateTasks",
            "OnCreateTask",
            "OnExecute",
            "CheckExecutionProcessing",
            "BeforeExecute",
            "BeforeExecuteInteractively",
        ],
        10 => &[
            "BeforeCreateTasks",
            "OnCreateTask",
            "OnCreateSubBusinessProcesses",
            "OnExecute",
            "BeforeExecute",
            "BeforeCreateSubBusinessProcesses",
        ],
        _ => return None,
    })
}

const H_ALIGN: &[&str] = &["Left", "Center", "Right"];
const V_ALIGN: &[&str] = &["Top", "Center", "Bottom"];
const PIC_LOC: &[&str] = &["None", "Left", "Top", "Right", "Bottom", "Center"];
const PICTURE_SIZE: &[&str] = &[
    "RealSize",
    "Stretch",
    "Proportionally",
    "Tile",
    "AutoSize",
    "RealSizeIgnoreScale",
    "AutoSizeIgnoreScale",
];
const SHAPES: &[&str] = &[
    "None",
    "File",
    "Folder",
    "RightArrow",
    "LeftArrow",
    "UpArrow",
    "DownArrow",
    "LeftRightArrow",
    "UpDownArrow",
    "VerticalBrackets",
    "Document",
    "Block",
    "HorizontalBrackets",
    "Ellipse",
];
const GRID_MODES: &[&str] = &["None", "Dots", "Chess", "Lines"];
const TEXT_LOC: &[&str] = &["FirstSegment", "Middle"];
const ARROWS: &[&str] = &["None", "Filled", "Blank"];

#[derive(Debug, Clone, Copy)]
enum PCodec {
    Any,
    N,
    B,
    Map(&'static [&'static str]),
}

fn print_param(k: i64) -> Option<(&'static str, PCodec)> {
    use PCodec::*;
    Some(match k {
        0 => ("Paper", Any),
        1 => ("PageOrientation", Map(&["", "Portrait", "Landscape"])),
        2 => ("Scale", N),
        3 => ("Collate", B),
        4 => ("Copies", N),
        5 => ("PerPage", N),
        6 => ("TopMargin", N),
        7 => ("LeftMargin", N),
        8 => ("BottomMargin", N),
        9 => ("RightMargin", N),
        10 => ("Header", N),
        11 => ("Footer", N),
        12 => ("FitToPage", B),
        13 => ("BlackAndWhite", B),
        14 => ("Printer", Any),
        15 => ("PaperSource", Any),
        16 => ("FitPageMode", Map(&["Auto", "PageWidth", "Proportionally"])),
        17 => (
            "DuplexPrinting",
            Map(&[
                "",
                "None",
                "FlipPagesLeft",
                "FlipPagesUp",
                "UsePrinterSettings",
            ]),
        ),
        18 => (
            "PageMargin",
            Map(&["Auto", "DontUse", "MirrorOnLeft", "MirrorOnTop"]),
        ),
        _ => return None,
    })
}

pub(crate) fn b(v: &Node) -> &'static str {
    if is(v, "1") { "true" } else { "false" }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Task {
    pub name: String,
    pub attrs: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Route {
    pub task: R<Option<Task>>,
}

#[allow(dead_code)]
pub(crate) fn polygon(kind: i64, l: i64, t: i64, r: i64, bt: i64) -> Option<Vec<(i64, i64)>> {
    let k30 = (std::f64::consts::PI / 6.0).tan() / 2.0;
    let floor_mul = |n: i64| -> Option<i64> {
        let x = (n as f64 * k30).floor();
        (x >= i64::MIN as f64 && x < i64::MAX as f64).then_some(x as i64)
    };
    let w = r.checked_sub(l)?;
    let h = bt.checked_sub(t)?;
    let (ww, hh) = (w.checked_sub(1)?, h.checked_sub(1)?);
    let pts = match kind {
        2 | 3 => {
            let mut a = floor_mul(w)?;
            if a > h {
                a = h.div_euclid(2);
            }
            let cx = w.div_euclid(2);
            let ha = h.checked_sub(a)?;
            if kind == 2 {
                vec![(0, 0), (ww, 0), (ww, ha), (cx, hh), (0, ha)]
            } else {
                vec![(cx, 0), (ww, a), (ww, hh), (0, hh), (0, a)]
            }
        }
        4 => {
            let mut a = floor_mul(h)?;
            if a > w.div_euclid(2) {
                a = w.div_euclid(4);
            }
            let cy = h.div_euclid(2);
            let wa = ww.checked_sub(a)?;
            vec![(0, cy), (a, 0), (wa, 0), (ww, cy), (wa, hh), (a, hh)]
        }
        7 | 8 => {
            let ax = ww.div_euclid(2);
            let ax2 = ax.checked_mul(2)?;
            if kind == 7 {
                vec![(0, 0), (ax2, 0), (ax, hh)]
            } else {
                vec![(0, hh), (ax2, hh), (ax, 0)]
            }
        }
        _ => vec![(0, 0), (ww, 0), (ww, hh), (0, hh)],
    };
    pts.into_iter()
        .map(|(x, y)| Some((l.checked_add(x)?, t.checked_add(y)?)))
        .collect()
}

#[allow(dead_code)]
pub(crate) fn ns_map(data: &[u8]) -> R<HashMap<String, String>> {
    let root = crate::xml::parse_xml(data).map_err(|e| GraphError(e.to_string()))?;
    let mut m = HashMap::new();
    let mut stack = vec![&root];
    while let Some(n) = stack.pop() {
        for (p, u) in &n.ns_decls {
            m.entry(p.clone().unwrap_or_default())
                .or_insert_with(|| u.clone());
        }
        stack.extend(n.children.iter().rev());
    }
    Ok(m)
}

use super::read::{R as F, Read, ReadError};
use super::route::{RouteAccess, TaskAccess};
use super::{
    color::Color,
    font::Font,
    line::Line,
    lstr::{Grammar, LocalStrings},
    picture::GraphPicture,
    scalar::{self, Text},
    tvalue::TypedValue,
};
use crate::templates::{
    allocation::{Allocation, Owned},
    context::Lookup,
};
use crate::xml_out::{emit::XmlEmit, semantic::SemanticEmit};
use std::fmt::Write;
fn offset(read: Read<'_>, base: i64, step: i64, index: usize) -> F<i64> {
    i64::try_from(index)
        .ok()
        .and_then(|index| step.checked_mul(index))
        .and_then(|span| base.checked_add(span))
        .ok_or_else(|| read.invalid("index overflow"))
}
pub(crate) fn to_xml(cfg: &Configuration, entry: &Node, route: Option<&Route>) -> R<Vec<u8>> {
    to_xml_with(
        cfg,
        entry,
        route.map(|route| route as &dyn RouteAccess),
        Allocation::Native,
    )
    .map_err(ReadError::legacy)?
    .native()
    .map_err(|error| GraphError(error.to_string()))
}
pub(crate) fn to_xml_with<'a>(
    cfg: &'a dyn Lookup,
    entry: &Node,
    route: Option<&'a dyn RouteAccess>,
    allocation: Allocation<'a>,
) -> F<Owned<Vec<u8>>> {
    Dec {
        names: Names::from_lookup(cfg),
        lookup: cfg,
        route,
        read: Read::new(allocation),
        w: XmlEmit::new(allocation, 1)?,
    }
    .run(entry)
}

struct Dec<'a> {
    names: Names<'a>,
    route: Option<&'a dyn RouteAccess>,
    lookup: &'a dyn Lookup,
    read: Read<'a>,
    w: XmlEmit<'a>,
}

impl<'a> Dec<'a> {
    fn leaf(&mut self, tag: &str, text: &str) -> F<()> {
        self.w.value(tag, Some(text), false, &[])?;
        Ok(())
    }
    fn text(&mut self, tag: &str, text: &str) -> F<()> {
        self.w.value(tag, Some(text), true, &[])?;
        Ok(())
    }
    fn lstr(&mut self, tag: &str, value: &Node) -> F<()> {
        LocalStrings::read(value, self.read, Grammar::Graph)?.write(&mut self.w, tag, "v8")
    }
    fn typed(&mut self, tag: &str, value: &Node) -> F<()> {
        TypedValue::read(value, self.read, Grammar::Graph)?.write(&mut self.w, tag)
    }
    fn color(&mut self, tag: &str, value: &Node) -> F<()> {
        Color::graph(value, self.read, self.lookup, v::DECLARED, None)?.write(
            &mut self.w,
            tag,
            self.read.allocation,
        )
    }
    fn line(&mut self, tag: &str, value: &Node) -> F<()> {
        let line = Line::graph(value, self.read)?;
        let attrs = line.attrs();
        let (kind, name) = line
            .style
            .ok_or_else(|| self.read.invalid("connector line style"))?;
        let mut xsi = Text::<64>::new();
        write!(&mut xsi, "sch:{kind}").map_err(|_| self.read.invalid("line QName"))?;
        self.w.open_attributes(tag, &attrs)?;
        self.w.value(
            "v8ui:style",
            Some(name),
            false,
            &[("xsi:type", xsi.as_str())],
        )?;
        self.w.close_parts(&[tag])?;
        Ok(())
    }
    fn picture(&mut self, value: &Node) -> F<()> {
        let Some(picture) = GraphPicture::read(value, self.read, self.lookup)? else {
            self.w.value("Picture", None, false, &[])?;
            return Ok(());
        };
        let xr = "http://v8.1c.ru/8.3/xcf/readable";
        self.w.open_attributes("Picture", &[])?;
        self.w
            .value("Ref", Some(&picture.text), false, &[("xmlns", xr)])?;
        self.w.value(
            "LoadTransparent",
            Some(if picture.transparent { "true" } else { "false" }),
            false,
            &[("xmlns", xr)],
        )?;
        self.w.close_parts(&["Picture"])?;
        Ok(())
    }

    fn common(&mut self, hdr: &Node, cb: &Node) -> F<()> {
        let read = self.read;
        let h = read.list(hdr)?;
        let c = read.list(cb)?;
        self.leaf("Name", read.scalar(read.at(h, 3)?)?)?;
        self.lstr("Title", read.at(h, 2)?)?;
        self.lstr("ToolTip", read.at(c, 5)?)?;
        self.leaf("TabOrder", read.scalar(read.at(h, 4)?)?)?;
        self.color("BackColor", read.at(c, 1)?)?;
        self.color("TextColor", read.at(c, 3)?)?;
        self.color("LineColor", read.at(c, 2)?)?;
        self.leaf("GroupNumber", read.scalar(read.at(c, 12)?)?)?;
        self.leaf("ZOrder", read.scalar(read.at(c, 9)?)?)?;
        self.leaf("Hyperlink", b(read.at(c, 10)?))?;
        self.leaf("Transparent", b(read.at(c, 11)?))?;
        Font::lookup(
            read.list(read.at(c, 4)?)?,
            read,
            super::font::Grammar::Graph,
            self.lookup,
            false,
        )?
        .write(&mut self.w, "Font")?;
        self.leaf(
            "HorizontalAlign",
            read.at(H_ALIGN, read.int(read.at(c, 6)?)?)?,
        )?;
        self.leaf(
            "VerticalAlign",
            read.at(V_ALIGN, read.int(read.at(c, 7)?)?)?,
        )?;
        self.leaf(
            "PictureLocation",
            read.at(PIC_LOC, read.int(read.at(c, 8)?)?)?,
        )?;
        if !is(read.at(c, 13)?, "0") || !is(read.at(c, 0)?, "7") {
            return Err(read.invalid("common block"));
        }
        Ok(())
    }

    fn events(&mut self, kind: i64, ev: &Node) -> F<()> {
        let read = self.read;
        let names = events_of(kind)
            .ok_or_else(|| read.semantic("graphical schema", || format!("events of {kind}")))?;
        let e = read.list(ev)?;
        let n = read.int(read.at(e, 0)?)?;
        let mut h: [Option<&str>; 7] = [None; 7];
        let mut unknown = false;
        for x in slice(e, 1, n.saturating_add(1)) {
            let k = read.int(read.item(x, 0)?)?;
            let t = read.scalar(read.item(x, 1)?)?;
            match usize::try_from(k)
                .ok()
                .and_then(|k| h.get_mut(..names.len()).and_then(|h| h.get_mut(k)))
            {
                Some(slot) => *slot = Some(t),
                None => unknown = true,
            }
        }
        if unknown {
            return Err(read.invalid("event index"));
        }
        self.w.open_attributes("Events", &[])?;
        for (nm, t) in names.iter().zip(h) {
            self.w.value("Event", t, false, &[("name", nm)])?;
        }
        self.w.close_parts(&["Events"])?;
        Ok(())
    }

    fn location(&mut self, coordinates: [i64; 4]) -> F<()> {
        let mut text: [Text<64>; 4] = std::array::from_fn(|_| Text::new());
        for (text, value) in text.iter_mut().zip(coordinates) {
            write!(text, "{value}").map_err(|_| self.read.invalid("location coordinate"))?;
        }
        let [l, t, r, bt] = &text;
        self.w.value(
            "Location",
            None,
            false,
            &[
                ("top", t.as_str()),
                ("left", l.as_str()),
                ("bottom", bt.as_str()),
                ("right", r.as_str()),
            ],
        )?;
        Ok(())
    }

    fn task(&self) -> F<Option<&'a dyn TaskAccess>> {
        match self.route {
            None => Ok(None),
            Some(route) => route.task(self.read),
        }
    }

    fn addressing(&mut self, ad: &Node) -> F<()> {
        let read = self.read;
        let a = read.list(ad)?;
        let n = read.int(read.at(a, 0)?)?;
        let mut vals = read.allocation.vector::<(&str, &Node)>(0)?;
        for x in slice(a, 1, n.saturating_add(1)) {
            let (k, val) = (read.scalar(read.item(x, 0)?)?, read.item(x, 1)?);
            match vals.iter_mut().find(|(u, _)| *u == k) {
                Some(slot) => slot.1 = val,
                None => vals.push((k, val))?,
            }
        }
        let task = self.task()?;
        if let Some(task) = task
            && let Some((uuid, _)) = vals.iter().find(|(uuid, _)| {
                !(0..task.len()).any(|at| task.attr(at).is_some_and(|(id, _)| id == *uuid))
            })
        {
            return Err(read.semantic("addressing attribute", || {
                format!("addressing attribute {uuid} not in {}", task.name())
            }));
        }
        let count = task.map_or(vals.len(), TaskAccess::len);
        if count == 0 {
            self.w.value("AddressingAttributes", None, false, &[])?;
            return Ok(());
        }
        self.w.open_attributes("AddressingAttributes", &[])?;
        for at in 0..count {
            let (uuid, known) = match task {
                Some(task) => {
                    let (uuid, name) = task
                        .attr(at)
                        .ok_or_else(|| read.invalid("task attribute"))?;
                    (uuid, Some(name))
                }
                None => (read.index(&vals, at)?.0, None),
            };
            let name = match known {
                Some(name) => name,
                None => self.names.name_of(uuid).ok_or_else(|| {
                    read.semantic("addressing attribute", || {
                        format!("addressing attribute {uuid}")
                    })
                })?,
            };
            self.w
                .open_attributes("AddressingAttribute", &[("ref", name)])?;
            match vals.iter().find(|(id, _)| *id == uuid) {
                Some((_, value)) => self.typed("Value", value)?,
                None => self.w.value("Value", None, false, &[("xsi:nil", "true")])?,
            }
            self.w.close_parts(&["AddressingAttribute"])?;
        }
        self.w.close_parts(&["AddressingAttributes"])?;
        Ok(())
    }

    fn rect(read: Read<'_>, y: &[Node]) -> F<[i64; 4]> {
        Ok([
            read.int(read.at(y, 2)?)?,
            read.int(read.at(y, 3)?)?,
            read.int(read.at(y, 4)?)?,
            read.int(read.at(y, 5)?)?,
        ])
    }

    fn point(&mut self, kind: i64, it: &Node) -> F<()> {
        let read = self.read;
        let itl = read.list(it)?;
        let head = read.at(itl, 0)?;
        let hdr = read.item(head, 0)?;
        let uuid = read.scalar(read.item(head, 2)?)?;
        if !is(read.item(head, 1)?, "4") || !is(read.item(head, 3)?, "0") {
            return Err(read.invalid("point header"));
        }
        let inner = read.list(read.item(read.at(itl, 2)?, 0)?)?;
        let y = read.list(read.at(inner, 0)?)?;
        let cb = read.at(y, 0)?;
        if !is(read.at(y, 1)?, "5") {
            return Err(read.invalid("point shape tag"));
        }
        let rect = Self::rect(read, y)?;
        let npts = read.int(read.at(y, 6)?)?;
        let p = npts
            .checked_mul(2)
            .and_then(|x| x.checked_add(7))
            .ok_or_else(|| read.invalid("point count"))?;
        let psize = read.at(y, p)?;
        let pict = read.at(y, p.saturating_add(1))?;
        let border = read.at(y, p.saturating_add(2))?;
        let tag = kind_tag(kind)
            .ok_or_else(|| read.semantic("graphical schema", || format!("item kind {kind}")))?;
        let id = read.scalar(read.item(hdr, 1)?)?;
        self.w.open_attributes(tag, &[("id", id), ("uuid", uuid)])?;
        self.w.open_attributes("Properties", &[])?;
        self.common(hdr, cb)?;
        self.location(rect)?;
        self.line("Border", border)?;
        self.picture(pict)?;
        self.leaf("PictureSize", read.at(PICTURE_SIZE, read.int(psize)?)?)?;
        let outer = slice(itl, 3, i64::MAX);
        let ev = match kind {
            5 => {
                let [expl, group, ev, ad, task] = outer else {
                    return Err(read.invalid("activity fields"));
                };
                self.text("TaskDescription", read.scalar(task)?)?;
                self.text("Explanation", read.scalar(expl)?)?;
                self.leaf("Group", b(group))?;
                self.addressing(ad)?;
                Some(ev)
            }
            4 => {
                self.leaf("TruePortIndex", read.scalar(read.at(inner, 2)?)?)?;
                self.leaf("FalsePortIndex", read.scalar(read.at(inner, 3)?)?)?;
                Some(read.at(outer, 0)?)
            }
            6 => {
                let n = read.int(read.at(inner, 2)?)?;
                let names = slice(outer, 1, n.saturating_add(1));
                for i in 0..count(n) {
                    let name = read.scalar(read.at(names, offset(read, 0, 1, i)?)?)?;
                    self.w.open_attributes("Case", &[])?;
                    self.leaf("sch:name", name)?;
                    self.lstr("sch:description", read.at(inner, offset(read, 3, 2, i)?)?)?;
                    self.color("sch:backColor", read.at(inner, offset(read, 4, 2, i)?)?)?;
                    self.w.close_parts(&["Case"])?;
                }
                Some(read.at(outer, n.saturating_add(1))?)
            }
            10 => {
                let [ev, sub, task] = outer else {
                    return Err(read.invalid("sub-process fields"));
                };
                let sn = if is(sub, ZERO) {
                    ""
                } else {
                    self.names
                        .name(sub)
                        .ok_or_else(|| read.invalid("sub-process"))?
                };
                self.leaf("Subprocess", sn)?;
                self.text("TaskDescription", read.scalar(task)?)?;
                Some(ev)
            }
            7 | 8 => None,
            _ => Some(read.at(outer, 0)?),
        };
        self.w.close_parts(&["Properties"])?;
        if let Some(ev) = ev {
            self.events(kind, ev)?;
        }
        self.w.close_parts(&[tag])?;
        Ok(())
    }

    fn decoration(&mut self, it: &Node) -> F<()> {
        let read = self.read;
        let itl = read.list(it)?;
        let hdr = read.at(itl, 0)?;
        let y = read.list(read.item(read.at(itl, 2)?, 0)?)?;
        let cb = read.at(y, 0)?;
        if !is(read.at(y, 1)?, "6") || !is(read.at(itl, 1)?, "2") {
            return Err(read.invalid("decoration layout"));
        }
        let rect = Self::rect(read, y)?;
        let [psize, pict, _transparent, shape, flip, angle] = slice(y, 6, 12) else {
            return Err(read.invalid("decoration fields"));
        };
        let id = read.scalar(read.item(hdr, 1)?)?;
        self.w.open_attributes("Decoration", &[("id", id)])?;
        self.w.open_attributes("Properties", &[])?;
        self.common(hdr, cb)?;
        self.location(rect)?;
        self.picture(pict)?;
        self.leaf("PictureSize", read.at(PICTURE_SIZE, read.int(psize)?)?)?;
        let sh = read.int(shape)?;
        let sh = usize::try_from(sh)
            .ok()
            .and_then(|i| SHAPES.get(i))
            .ok_or_else(|| read.semantic("graphical schema", || format!("shape {sh}")))?;
        self.leaf("Shape", sh)?;
        self.leaf("FlipMode", read.scalar(flip)?)?;
        let text = read.scalar(angle)?;
        let number = scalar::number(text, read.allocation).map_err(|error| match error {
            scalar::ScalarError::Memory(error) => ReadError::Memory(error),
            scalar::ScalarError::Invalid => {
                read.semantic("not a number", || format!("not a number: {text}"))
            }
        })?;
        let angle = scalar::xml_double(number).map_err(|error| match error {
            scalar::ScalarError::Memory(error) => ReadError::Memory(error),
            scalar::ScalarError::Invalid => read.semantic("not a finite number", || {
                format!("not a finite number: {number}")
            }),
        })?;
        self.w.value(
            "Angle",
            Some(angle.as_str()),
            false,
            &[("xsi:type", "xs:decimal")],
        )?;
        self.w.close_parts(&["Properties"])?;
        self.w.close_parts(&["Decoration"])?;
        Ok(())
    }

    fn connection(&mut self, it: &Node, by_id: &[(i64, usize, &str)]) -> F<()> {
        let read = self.read;
        let itl = read.list(it)?;
        let hdr = read.at(itl, 0)?;
        if !is(read.at(itl, 1)?, "3") {
            return Err(read.invalid("connection layout"));
        }
        let (frm, to, deco) = (
            read.int(read.at(itl, 2)?)?,
            read.int(read.at(itl, 4)?)?,
            read.at(itl, 5)?,
        );
        let y = read.list(read.item(read.at(itl, 6)?, 0)?)?;
        let cb = read.at(y, 0)?;
        if !is(read.at(y, 1)?, "6") {
            return Err(read.invalid("connection shape tag"));
        }
        let n = read.int(read.at(y, 2)?)?;
        let mut pts = read.allocation.vector(0)?;
        for i in 0..count(n) {
            pts.push((
                read.scalar(read.at(y, offset(read, 3, 2, i)?)?)?,
                read.scalar(read.at(y, offset(read, 4, 2, i)?)?)?,
            ))?;
        }
        let mut p = n
            .checked_mul(2)
            .and_then(|x| x.checked_add(3))
            .ok_or_else(|| read.invalid("pivot count"))?;
        let [ln, textloc, pfrom, pto, nseg] = slice(y, p, p.saturating_add(5)) else {
            return Err(read.invalid("connection fields"));
        };
        p = p.saturating_add(5);
        let mut segs = read.allocation.vector(0)?;
        for _ in 0..count(read.int(nseg)?) {
            let [x1, y1, x2, y2, idx] = slice(y, p, p.saturating_add(5)) else {
                return Err(read.invalid("segment"));
            };
            segs.push([
                read.scalar(x1)?,
                read.scalar(y1)?,
                read.scalar(x2)?,
                read.scalar(y2)?,
                read.scalar(idx)?,
            ])?;
            p = p.saturating_add(5);
        }
        let (begin, end) = (read.at(y, p)?, read.at(y, p.saturating_add(1))?);
        if i64::try_from(y.len()).ok() != p.checked_add(2) {
            return Err(read.invalid("connection length"));
        }
        let id = read.scalar(read.item(hdr, 1)?)?;
        self.w.open_attributes("ConnectionLine", &[("id", id)])?;
        self.w.open_attributes("Properties", &[])?;
        self.common(hdr, cb)?;
        if pts.is_empty() {
            self.w.value("PivotPoints", None, false, &[])?;
        } else {
            self.w.open_attributes("PivotPoints", &[])?;
            for &(x, yy) in pts.iter() {
                self.w.value("Point", None, false, &[("x", x), ("y", yy)])?;
            }
            self.w.close_parts(&["PivotPoints"])?;
        }
        if !segs.is_empty() {
            self.w.open_attributes("ManualyMovedSegments", &[])?;
            for &[x1, y1, x2, y2, idx] in segs.iter() {
                self.w.open_attributes("Segment", &[("index", idx)])?;
                self.w
                    .value("Start", None, false, &[("x", x1), ("y", y1)])?;
                self.w.value("End", None, false, &[("x", x2), ("y", y2)])?;
                self.w.close_parts(&["Segment"])?;
            }
            self.w.close_parts(&["ManualyMovedSegments"])?;
        }
        self.w.open_attributes("Connect", &[])?;
        for (tag, iid, port) in [("From", frm, pfrom), ("To", to, pto)] {
            self.w.open_attributes(tag, &[])?;
            let item_name = (iid >= 0).then(|| {
                by_id
                    .partition_point(|(id, _, _)| *id <= iid)
                    .checked_sub(1)
                    .and_then(|at| by_id.get(at))
                    .filter(|(id, _, _)| *id == iid)
                    .map(|(_, _, name)| *name)
                    .unwrap_or_default()
            });
            self.w.value("Item", item_name, false, &[])?;
            self.leaf("PortIndex", read.scalar(port)?)?;
            self.w.close_parts(&[tag])?;
        }
        self.w.close_parts(&["Connect"])?;
        self.line("Line", ln)?;
        self.leaf("DecorativeLine", b(deco))?;
        self.leaf("TextLocation", read.at(TEXT_LOC, read.int(textloc)?)?)?;
        self.leaf("BeginArrow", read.at(ARROWS, read.int(begin)?)?)?;
        self.leaf("EndArrow", read.at(ARROWS, read.int(end)?)?)?;
        self.w.close_parts(&["Properties"])?;
        self.w.close_parts(&["ConnectionLine"])?;
        Ok(())
    }

    fn print_params(&mut self, keys: &[(i64, &Node)]) -> F<()> {
        let read = self.read;
        self.w.open_attributes("PrintParameters", &[])?;
        for (k, val) in keys {
            let (tag, codec) = print_param(*k)
                .ok_or_else(|| read.semantic("graphical schema", || format!("print key {k}")))?;
            match codec {
                PCodec::Any => self.typed(tag, val)?,
                PCodec::N => self.leaf(tag, read.scalar(read.item(val, 1)?)?)?,
                PCodec::B => self.leaf(tag, b(read.item(val, 1)?))?,
                PCodec::Map(names) => {
                    self.leaf(tag, read.at(names, read.int(read.item(val, 1)?)?)?)?
                }
            }
        }
        self.w.close_parts(&["PrintParameters"])?;
        Ok(())
    }

    fn run(mut self, t: &Node) -> F<Owned<Vec<u8>>> {
        let read = self.read;
        let tl = read.list(t)?;
        if !is(read.at(tl, 0)?, "5") {
            return Err(read.invalid("schema version"));
        }
        let t1 = read.list(read.at(tl, 1)?)?;
        let c = read.list(read.at(t1, 0)?)?;
        if !is(read.at(c, 0)?, "1") || t1.len() != 1 {
            return Err(read.invalid("schema context"));
        }
        self.color("BackColor", read.at(c, 1)?)?;
        self.leaf("GridEnabled", b(read.at(c, 2)?))?;
        self.leaf(
            "DrawGridMode",
            read.at(GRID_MODES, read.int(read.at(c, 5)?)?)?,
        )?;
        self.leaf("GridHorizontalStep", read.scalar(read.at(c, 3)?)?)?;
        self.leaf("GridVerticalStep", read.scalar(read.at(c, 4)?)?)?;
        let mut keys = read.allocation.vector::<(i64, &Node)>(0)?;
        for i in 0..count(read.int(read.at(c, 6)?)?) {
            let k = read.int(read.at(c, offset(read, 7, 2, i)?)?)?;
            let val = read.at(c, offset(read, 8, 2, i)?)?;
            match keys.iter_mut().find(|(x, _)| *x == k) {
                Some(slot) => slot.1 = val,
                None => keys.push((k, val))?,
            }
        }
        if let Some((k, _)) = keys.iter().find(|(k, _)| print_param(*k).is_none()) {
            return Err(read.semantic("graphical schema", || format!("print key {k}")));
        }
        keys.sort_unstable_by_key(|(k, _)| *k);
        self.print_params(&keys)?;
        let n = read.int(read.at(tl, 2)?)?;
        let mut items = read.allocation.vector(0)?;
        for i in 0..count(n) {
            items.push((
                read.int(read.at(tl, offset(read, 3, 2, i)?)?)?,
                read.at(tl, offset(read, 4, 2, i)?)?,
            ))?;
        }
        if n.checked_mul(2).and_then(|x| x.checked_add(4)) != i64::try_from(tl.len()).ok() {
            return Err(read.invalid("schema length"));
        }
        let mut by_id = read.allocation.vector::<(i64, usize, &str)>(0)?;
        for (ordinal, (kind, it)) in items.iter().enumerate() {
            let hdr = if matches!(kind, 0 | 1) {
                read.item(it, 0)?
            } else {
                read.item(read.item(it, 0)?, 0)?
            };
            let key = read.int(read.item(hdr, 1)?)?;
            let name = read.scalar(read.item(hdr, 3)?)?;
            by_id.push((key, ordinal, name))?;
        }
        by_id.sort_unstable_by_key(|(id, ordinal, _)| (*id, *ordinal));
        if items.is_empty() {
            self.w.value("Items", None, false, &[])?;
        } else {
            self.w.open_attributes("Items", &[])?;
            for (kind, it) in items.iter() {
                match kind {
                    0 => self.decoration(it)?,
                    1 => self.connection(it, &by_id)?,
                    2..=10 => self.point(*kind, it)?,
                    _ => return Err(read.semantic("item kind", || format!("item kind {kind}"))),
                }
            }
            self.w.close_parts(&["Items"])?;
        }
        let body = self.w.finish();
        let header = [
            "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<GraphicalSchema ",
            ROOT_NS,
            ">\r\n",
        ];
        let tail = "</GraphicalSchema>";
        let newline = if body.is_empty() { "" } else { "\r\n" };
        let size = header
            .iter()
            .map(|text| text.len())
            .chain([body.len(), newline.len(), tail.len()])
            .try_fold(0usize, |sum, size| {
                sum.checked_add(size)
                    .ok_or(memory_budget::BudgetError::Overflow)
            })?;
        let mut out = read.allocation.vector(size)?;
        for part in header {
            out.extend(part.as_bytes())?;
        }
        out.extend(&body)?;
        out.extend(newline.as_bytes())?;
        out.extend(tail.as_bytes())?;
        Ok(out.finish())
    }
}
