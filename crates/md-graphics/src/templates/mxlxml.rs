use super::allocation::Owned;
use super::allocation::{Allocation, Vector};
use super::context::Lookup;
use super::moxel::MoxelError;
use super::moxel_view::MoxelView;
use super::mxltables::{self as t, Codec, FormatSlot, ViewKind};
use crate::Configuration;
use crate::graph::{
    lstr::{Grammar, LocalStrings},
    read::{R as F, Read, ReadError},
};
use crate::xml_out::emit::XmlEmit;
use brace::Node;
use memory_budget::BudgetError;

const NS: &str = concat!(
    "xmlns=\"http://v8.1c.ru/8.2/data/spreadsheet\" xmlns:pal=\"http://v8.1c.ru/8.1/data/ui/colors/palette\" ",
    "xmlns:style=\"http://v8.1c.ru/8.1/data/ui/style\" xmlns:v8=\"http://v8.1c.ru/8.1/data/core\" ",
    "xmlns:v8ui=\"http://v8.1c.ru/8.1/data/ui\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" ",
    "xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\""
);
const ZERO: &str = "00000000-0000-0000-0000-000000000000";
const T_DETAIL_ROW: &str = "3031edd8-c3df-47b2-98ca-47f628d4ec18";
const HEADER_SLOTS: [&str; 6] = [
    "leftHeader",
    "leftFooter",
    "centerHeader",
    "centerFooter",
    "rightHeader",
    "rightFooter",
];
const HEADER_ORDER: [usize; 6] = [0, 2, 4, 1, 3, 5];
const CELL_BITS: i64 = 1 | 2 | 4 | 8 | 16 | 32 | 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unsupported(pub String);

impl From<BudgetError> for Unsupported {
    fn from(error: BudgetError) -> Self {
        Self(error.to_string())
    }
}

impl From<MoxelError> for Unsupported {
    fn from(e: MoxelError) -> Self {
        Unsupported(e.0)
    }
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub(crate) type R<T> = Result<T, Unsupported>;

fn is(n: &Node, lit: &str) -> bool {
    n.text() == Some(lit)
}

fn count(n: i64) -> usize {
    usize::try_from(n).unwrap_or(0)
}

fn native_error(error: ReadError) -> Unsupported {
    Unsupported(error.legacy().0)
}
fn leaf(w: &mut XmlEmit<'_>, tag: &str, text: &str) -> F<()> {
    w.leaf(tag, Some(text), [])?;
    Ok(())
}
fn leaf_number(w: &mut XmlEmit<'_>, tag: &str, value: impl std::fmt::Display) -> F<()> {
    use std::fmt::Write;
    let mut text = crate::graph::scalar::Text::<64>::new();
    write!(&mut text, "{value}").map_err(|_| BudgetError::Overflow)?;
    leaf(w, tag, text.as_str())
}
fn write_local(writer: &mut XmlEmit<'_>, tag: &str, node: &Node, read: Read<'_>) -> F<()> {
    LocalStrings::read(node, read, Grammar::Spreadsheet)?.write(writer, tag, "v8")
}
fn write_typed(writer: &mut XmlEmit<'_>, tag: &str, node: &Node, read: Read<'_>) -> F<()> {
    crate::graph::tvalue::TypedValue::read(node, read, Grammar::Spreadsheet)?.write(writer, tag)
}
fn first_text_with<'n>(node: &'n Node, read: Read<'_>) -> F<Option<&'n str>> {
    match node {
        Node::List(values) => Ok(read.index(values, 0)?.text()),
        Node::Str(text) | Node::Atom(text) => {
            let first = text
                .chars()
                .next()
                .ok_or_else(|| read.invalid("string index out of range"))?;
            Ok(text.get(..first.len_utf8()))
        }
    }
}

pub(crate) struct Renum<K> {
    order: Vector<K>,
    lookup: Vector<(K, usize)>,
    base: usize,
}
impl<K: Copy + Ord> Renum<K> {
    pub(crate) fn new(base: usize, allocation: Allocation<'_>) -> Result<Self, BudgetError> {
        Ok(Self {
            order: allocation.vector(0)?,
            lookup: allocation.vector(0)?,
            base,
        })
    }
    pub(crate) fn refer(&mut self, key: K) -> Result<usize, BudgetError> {
        let at = match self.lookup.binary_search_by_key(&key, |(key, _)| *key) {
            Ok(at) => {
                return self
                    .lookup
                    .get(at)
                    .map(|(_, index)| *index)
                    .ok_or(BudgetError::Overflow);
            }
            Err(at) => at,
        };
        let index = self
            .order
            .len()
            .checked_add(self.base)
            .ok_or(BudgetError::Overflow)?;
        self.order.reserve(1)?;
        self.lookup.reserve(1)?;
        self.order.push(key)?;
        self.lookup.insert(at, (key, index))?;
        Ok(index)
    }
    pub(crate) fn get(&self, key: &K) -> Option<usize> {
        self.lookup
            .binary_search_by_key(key, |(key, _)| *key)
            .ok()
            .and_then(|at| self.lookup.get(at))
            .map(|(_, index)| *index)
    }
}
struct SourceMap<K, V> {
    entries: Vector<(K, V, usize)>,
}
impl<K: Ord, V> SourceMap<K, V> {
    fn new(allocation: Allocation<'_>) -> Result<Self, BudgetError> {
        Ok(Self {
            entries: allocation.vector(0)?,
        })
    }
    fn push(&mut self, key: K, value: V) -> Result<(), BudgetError> {
        let order = self.entries.len();
        self.entries.push((key, value, order))
    }
    fn sort(&mut self) {
        self.entries
            .sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)));
    }
    fn get(&self, key: &K) -> Option<&V> {
        self.entries
            .partition_point(|(entry, _, _)| entry <= key)
            .checked_sub(1)
            .and_then(|index| self.entries.get(index))
            .filter(|(entry, _, _)| entry == key)
            .map(|(_, value, _)| value)
    }
    fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.iter().map(|(key, _, _)| key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum FKey {
    B(i64),
    Default,
}

pub(crate) fn bool_s(v: &Node) -> &'static str {
    if is(v, "1") { "true" } else { "false" }
}

#[derive(Debug, Default)]
pub(crate) struct Cell<'a> {
    pub fmt: i64,
    pub control: Option<&'a Node>,
    pub v: Option<&'a Node>,
    pub d: Option<&'a Node>,
    pub detail_parameter: Option<&'a Node>,
    pub picture_parameter: Option<&'a Node>,
    pub text: Option<&'a Node>,
    pub note: Option<(&'a Node, &'a Node)>,
    pub ftext: Option<&'a Node>,
}

fn take_with<'n>(
    values: &'n [Node],
    at: &mut usize,
    read: crate::graph::read::Read<'_>,
) -> crate::graph::read::R<&'n Node> {
    let node = read.index(values, *at)?;
    *at = at
        .checked_add(1)
        .ok_or_else(|| read.invalid("offset overflow"))?;
    Ok(node)
}
pub(crate) fn split_cell_with<'n>(
    c: &'n Node,
    read: crate::graph::read::Read<'_>,
) -> crate::graph::read::R<Cell<'n>> {
    let c = read.list(c)?;
    let m = read.plain_int(read.index(c, 0)?)?;
    if m & !CELL_BITS != 0 {
        return Err(read.semantic("cell mask", || format!("cell mask {m}")));
    }
    let mut out = Cell {
        fmt: read.plain_int(read.index(c, 1)?)?,
        ..Cell::default()
    };
    let mut p = 2;
    if m & 1 != 0 {
        out.control = Some(read.index(c, p + 1)?);
        p += 2;
    }
    if m & 2 != 0 {
        out.v = Some(take_with(c, &mut p, read)?);
    }
    if m & 4 != 0 {
        out.d = Some(take_with(c, &mut p, read)?);
    }
    if m & 8 != 0 {
        out.detail_parameter = Some(take_with(c, &mut p, read)?);
    }
    if m & 64 != 0 {
        out.picture_parameter = Some(take_with(c, &mut p, read)?);
    }
    if m & 16 != 0 {
        out.text = Some(take_with(c, &mut p, read)?);
    }
    if m & 32 != 0 {
        out.note = Some((read.index(c, p)?, read.index(c, p + 2)?));
        p += 3;
    }
    if m & 16 != 0 {
        if is(read.index(c, p)?, "1") {
            out.ftext = Some(read.index(c, p + 1)?);
            p += 1;
        }
        p += 1;
    }
    if p != c.len() {
        return Err(read.semantic("cell length", || {
            format!("cell length {} != {p} mask {m}", c.len())
        }));
    }
    Ok(out)
}

pub(crate) trait DrawingHook {
    fn drawing(
        &self,
        w: &mut Writer<'_>,
        tag: &str,
        dr: &[Node],
        zorder: Option<usize>,
    ) -> Option<F<()>>;
}

pub(crate) struct Writer<'a> {
    pub(crate) cfg: &'a dyn Lookup,
    pub(crate) read: Read<'a>,
    pub(crate) d: MoxelView<'a>,
    pub(crate) w: XmlEmit<'a>,
    fmt: Renum<FKey>,
    fonts: Renum<i64>,
    lines: Renum<i64>,
    colors: Renum<i64>,
    fdraw: Vector<i64>,
    style_colors: SourceMap<i64, &'a Node>,
    default_index: Option<usize>,
    hook: Option<&'a dyn DrawingHook>,
}

impl<'a> Writer<'a> {
    pub(crate) fn from_view(cfg: &'a Configuration, d: MoxelView<'a>) -> R<Self> {
        Self::construct(cfg, d, Read::new(Allocation::Native)).map_err(native_error)
    }
    pub(crate) fn construct(cfg: &'a dyn Lookup, d: MoxelView<'a>, read: Read<'a>) -> F<Self> {
        let mut style_colors = SourceMap::new(read.allocation)?;
        let sc = read.list(d.style_colors)?;
        for i in 0..count(read.plain_int(read.index(sc, 0)?)?) {
            let k = read.plain_int(read.index(sc, read.offset(1, 2, i)?)?)?;
            style_colors.push(k, read.index(sc, read.offset(2, 2, i)?)?)?;
        }
        style_colors.sort();
        if !d.embedded.is_empty() {
            return Err(read.invalid("embedded table"));
        }
        Ok(Writer {
            cfg,
            read,
            d,
            w: XmlEmit::new(read.allocation, 1)?,
            fmt: Renum::new(1, read.allocation)?,
            fonts: Renum::new(0, read.allocation)?,
            lines: Renum::new(0, read.allocation)?,
            colors: Renum::new(0, read.allocation)?,
            fdraw: read.allocation.vector(0)?,
            style_colors,
            default_index: None,
            hook: None,
        })
    }

    pub(crate) fn with_hook(mut self, hook: &'a dyn DrawingHook) -> Self {
        self.hook = Some(hook);
        self
    }

    fn ref_fmt(&mut self, b: i64, drawing: bool) -> F<()> {
        if b != 0 {
            if drawing && self.fmt.get(&FKey::B(b)).is_none() {
                self.fdraw.push(b)?;
            }
            self.fmt.refer(FKey::B(b))?;
        }
        Ok(())
    }

    fn format_rec(&self, b: i64) -> F<&'a Node> {
        let read = self.read;
        let d = self.d;
        b.checked_sub(1)
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| d.formats.get(i))
            .ok_or_else(|| read.semantic("spreadsheet", || format!("format {b}")))
    }

    fn same_as_default(&self) -> F<Option<i64>> {
        let mut best = None;
        for k in self.fmt.order.iter() {
            if let FKey::B(b) = *k
                && self.format_rec(b)? == self.d.default_format
            {
                best = best.max(Some(b));
            }
        }
        Ok(best)
    }

    fn default_mask(&self) -> F<i64> {
        let read = self.read;
        read.plain_int(read.index(read.list(self.d.default_format)?, 0)?)
    }

    fn header(&self, slot: usize) -> F<&'a Node> {
        let read = self.read;
        let d = self.d;
        d.headers.get(slot).ok_or_else(|| read.invalid("header"))
    }

    fn number_formats(&mut self) -> F<()> {
        let read = self.read;
        let d = self.d;
        for cs in d.columns.iter() {
            let cs = read.list(cs)?;
            self.ref_fmt(read.plain_int(read.index(cs, 1)?)?, false)?;
            for i in 0..count(read.plain_int(read.index(cs, 3)?)?) {
                self.ref_fmt(
                    read.plain_int(read.index(cs, read.offset(5, 2, i)?)?)?,
                    false,
                )?;
            }
        }
        for (_, fmt, cells) in d.rows.iter() {
            self.ref_fmt(fmt, false)?;
            for (_, c) in cells.iter() {
                let p = split_cell_with(c, read)?;
                self.ref_fmt(p.fmt, false)?;
                if let Some((_, nd)) = p.note {
                    let f =
                        read.plain_int(read.index(read.list(read.index(read.list(nd)?, 0)?)?, 1)?)?;
                    self.ref_fmt(f, true)?;
                }
            }
        }
        for dr in d.drawings {
            let f = read.plain_int(read.index(read.list(read.index(read.list(dr)?, 0)?)?, 1)?)?;
            self.ref_fmt(f, true)?;
        }
        if let Some(b) = self.same_as_default()? {
            self.default_index = self.fmt.get(&FKey::B(b));
        } else if self.default_mask()? != 0 {
            self.default_index = Some(self.fmt.refer(FKey::Default)?);
        }
        for slot in HEADER_ORDER {
            let h = read.list(self.header(slot)?)?;
            self.ref_fmt(read.plain_int(read.index(h, 1)?)?, false)?;
        }
        if self.default_index.is_none()
            && self.default_mask()? == 0
            && let Some(b) = self.same_as_default()?
        {
            self.default_index = self.fmt.get(&FKey::B(b));
        }
        Ok(())
    }

    pub(crate) fn xf_with(&self, b: i64) -> F<usize> {
        let read = self.read;
        if b == 0 {
            return Ok(0);
        }
        self.fmt
            .get(&FKey::B(b))
            .ok_or_else(|| read.semantic("spreadsheet", || format!("format {b} not numbered")))
    }

    pub(crate) fn run_with(mut self) -> F<Owned<Vec<u8>>> {
        let read = self.read;
        let d = self.d;
        self.number_formats()?;
        let mut fw = XmlEmit::new(read.allocation, 1)?;
        for index in 0..self.fmt.order.len() {
            let k = *self
                .fmt
                .order
                .get(index)
                .ok_or_else(|| read.invalid("format order"))?;
            let (rec, drawing) = match k {
                FKey::B(b) => (self.format_rec(b)?, self.fdraw.contains(&b)),
                FKey::Default => (d.default_format, false),
            };
            self.w_format(&mut fw, rec, drawing)?;
        }
        self.w_lang()?;
        for cs in d.columns.iter() {
            self.w_columns(cs)?;
        }
        self.w_rows()?;
        for (i, dr) in d.drawings.iter().enumerate() {
            self.w_drawing("drawing", dr, Some(i + 1))?;
        }
        for slot in HEADER_ORDER {
            let h = self.header(slot)?;
            let hl = read.list(h)?;
            if read.plain_int(read.index(hl, 0)?)? != 0 || read.plain_int(read.index(hl, 1)?)? != 0
            {
                let tag = HEADER_SLOTS.get(slot).copied().unwrap_or_default();
                self.w_cell(tag, h)?;
            }
        }
        leaf(&mut self.w, "templateMode", "true")?;
        let step = read.scalar(read.index(d.u13, 1)?)?;
        if step != "2" {
            let v = match step {
                "1" => "ByRows",
                "3" => "WithoutMove",
                _ => return Err(read.semantic("spreadsheet", || format!("step {step}"))),
            };
            leaf(&mut self.w, "stepDirection", v)?;
        }
        if let Some(i) = self.default_index {
            leaf_number(&mut self.w, "defaultFormatIndex", i)?;
        }
        if d.height != 0 {
            leaf_number(&mut self.w, "height", d.height)?;
        }
        self.w_groups()?;
        self.w_breaks()?;
        self.w_merges()?;
        self.w_named()?;
        self.w_print()?;
        self.w_repeat()?;
        self.w_ui_colors()?;
        self.w_view()?;
        self.w_lines()?;
        self.w_fonts()?;
        let before = std::mem::replace(&mut self.w, XmlEmit::new(read.allocation, 1)?);
        self.w_pictures()?;
        if d.read_only != 0 {
            leaf(&mut self.w, "readOnly", "true")?;
        }
        let mut out = XmlEmit::new(read.allocation, 0)?;
        let header = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>";
        let segments = [before.len(), fw.len(), self.w.len()];
        let separators = segments
            .iter()
            .filter(|len| **len > 0)
            .count()
            .checked_add(2)
            .and_then(|count| count.checked_mul(2))
            .ok_or(BudgetError::Overflow)?;
        let size = [
            header.len(),
            "<document ".len(),
            NS.len(),
            1,
            "</document>".len(),
            before.len(),
            fw.len(),
            self.w.len(),
            separators,
        ]
        .into_iter()
        .try_fold(0usize, |total, len| {
            total.checked_add(len).ok_or(BudgetError::Overflow)
        })?;
        out.reserve(size)?;
        out.raw(header)?;
        out.raw_fragments(&["<document ", NS, ">"])?;
        out.append_segment(&before)?;
        out.append_segment(&fw)?;
        out.append_segment(&self.w)?;
        out.raw("</document>")?;
        Ok(out.finish())
    }

    fn w_lang(&mut self) -> F<()> {
        let read = self.read;
        let lg = read.list(self.d.lang)?;
        let w = &mut self.w;
        w.open("languageSettings", [])?;
        leaf(w, "currentLanguage", read.scalar(read.index(lg, 0)?)?)?;
        leaf(w, "defaultLanguage", read.scalar(read.index(lg, 1)?)?)?;
        for i in 0..count(read.plain_int(read.index(lg, 3)?)?) {
            w.open("languageInfo", [])?;
            leaf(
                w,
                "id",
                read.scalar(read.index(lg, read.offset(4, 3, i)?)?)?,
            )?;
            leaf(
                w,
                "code",
                read.scalar(read.index(lg, read.offset(5, 3, i)?)?)?,
            )?;
            leaf(
                w,
                "description",
                read.scalar(read.index(lg, read.offset(6, 3, i)?)?)?,
            )?;
            w.close("languageInfo")?;
        }
        w.close("languageSettings")?;
        Ok(())
    }

    fn w_columns(&mut self, cs: &Node) -> F<()> {
        let read = self.read;
        let cs = read.list(cs)?;
        self.w.open("columns", [])?;
        let id = read.index(cs, 2)?;
        if !is(id, ZERO) {
            leaf(&mut self.w, "id", read.scalar(id)?)?;
        }
        let f = read.plain_int(read.index(cs, 1)?)?;
        if f != 0 {
            let x = self.xf_with(f)?;
            leaf_number(&mut self.w, "formatIndex", x)?;
        }
        leaf(&mut self.w, "size", read.scalar(read.index(cs, 0)?)?)?;
        for i in 0..count(read.plain_int(read.index(cs, 3)?)?) {
            let index = read.scalar(read.index(cs, read.offset(4, 2, i)?)?)?;
            let x = self.xf_with(read.plain_int(read.index(cs, read.offset(5, 2, i)?)?)?)?;
            let w = &mut self.w;
            w.open("columnsItem", [])?;
            leaf(w, "index", index)?;
            w.open("column", [])?;
            leaf_number(w, "formatIndex", x)?;
            w.close("column")?;
            w.close("columnsItem")?;
        }
        self.w.close("columns")?;
        Ok(())
    }

    fn colset_id(&self, i: i64) -> F<&'a str> {
        let read = self.read;
        let d = self.d;
        let k = i.checked_add(1).ok_or_else(|| read.invalid("column set"))?;
        let k = if k < 0 {
            i64::try_from(d.columns.len())
                .ok()
                .and_then(|len| len.checked_add(k))
        } else {
            Some(k)
        };
        let column = k
            .and_then(|k| usize::try_from(k).ok())
            .and_then(|k| d.columns.get(k))
            .ok_or_else(|| read.invalid("column set"))?;
        read.scalar(read.index(read.list(column)?, 2)?)
    }

    fn w_rows(&mut self) -> F<()> {
        let read = self.read;
        let d = self.d;
        let mut rc = SourceMap::new(read.allocation)?;
        for (row, column) in d.row_columns.iter() {
            rc.push(row, column)?;
        }
        rc.sort();
        let mut rows = d.rows.iter().peekable();
        while let Some((idx, fmt, cells)) = rows.next() {
            let cid = match rc.get(&idx) {
                Some(&k) => Some(self.colset_id(k)?),
                None => None,
            };
            let head = |me: &mut Self, fmt: i64| -> F<()> {
                me.w.open("row", [])?;
                if let Some(c) = cid {
                    leaf(&mut me.w, "columnsID", c)?;
                }
                if fmt != 0 {
                    let x = me.xf_with(fmt)?;
                    leaf_number(&mut me.w, "formatIndex", x)?;
                }
                Ok(())
            };
            if cells.is_empty() {
                let mut last = idx;
                let mut combined = false;
                while let Some(next) = rows.peek() {
                    if next.2.is_empty()
                        && next.1 == fmt
                        && last.checked_add(1) == Some(next.0)
                        && rc.get(&next.0) == rc.get(&idx)
                    {
                        last = next.0;
                        combined = true;
                        rows.next();
                    } else {
                        break;
                    }
                }
                self.w.open("rowsItem", [])?;
                leaf_number(&mut self.w, "index", idx)?;
                if combined {
                    leaf_number(&mut self.w, "indexTo", last)?;
                }
                head(self, fmt)?;
                leaf(&mut self.w, "empty", "true")?;
                self.w.close("row")?;
                self.w.close("rowsItem")?;
                continue;
            }
            self.w.open("rowsItem", [])?;
            leaf_number(&mut self.w, "index", idx)?;
            head(self, fmt)?;
            let mut prev: i64 = -1;
            for (col, c) in cells.iter() {
                self.w.open("c", [])?;
                if prev.checked_add(1) != Some(col) {
                    leaf_number(&mut self.w, "i", col)?;
                }
                prev = col;
                self.w_cell("c", c)?;
                self.w.close("c")?;
            }
            self.w.close("row")?;
            self.w.close("rowsItem")?;
        }
        Ok(())
    }

    fn fill_type(&self, b: i64) -> F<Option<i64>> {
        let read = self.read;
        if b == 0 {
            return Ok(None);
        }
        let k = b.checked_sub(1).ok_or_else(|| read.invalid("format"))?;
        {
            let values = t::FormatValues::read(read.list(read.at(self.d.formats, k)?)?, read)?;
            values
                .get(15)
                .map(|node| {
                    node.text()
                        .ok_or_else(|| read.invalid("not a scalar"))
                        .and_then(|text| {
                            text.trim()
                                .parse()
                                .map_err(|_| read.invalid("not an integer"))
                        })
                })
                .transpose()
        }
    }

    pub(crate) fn w_text_with(&mut self, p: &Cell<'_>, tl_tag: &str) -> F<()> {
        let read = self.read;
        let local = |value| LocalStrings::read(value, read, Grammar::Spreadsheet);
        if let Some(ft) = p.ftext {
            return local(read.index(read.list(ft)?, 1)?)?.write(&mut self.w, "tfl", "v8");
        }
        let Some(text) = p.text else {
            return Ok(());
        };
        if self.fill_type(p.fmt)? == Some(1) {
            let items = local(text)?;
            leaf(
                &mut self.w,
                "parameter",
                items.iter().next().map_or("", |(_, text)| text),
            )?;
            Ok(())
        } else {
            local(text)?.write(&mut self.w, tl_tag, "v8")
        }
    }

    fn w_cell(&mut self, tag: &str, c: &Node) -> F<()> {
        let read = self.read;
        let p = split_cell_with(c, read)?;
        self.w.open(tag, [])?;
        let x = self.xf_with(p.fmt)?;
        leaf_number(&mut self.w, "f", x)?;
        self.w_text_with(&p, "tl")?;
        if let Some(v) = p.v {
            write_typed(&mut self.w, "v", v, read)?;
        }
        if let Some(dp) = p.detail_parameter {
            leaf(&mut self.w, "detailParameter", read.scalar(dp)?)?;
        }
        if let Some(dv) = p.d {
            let dl = read.list(dv)?;
            if is(read.index(dl, 0)?, "#") && is(read.index(dl, 1)?, T_DETAIL_ROW) {
                leaf(
                    &mut self.w,
                    "r",
                    read.scalar(read.index(read.list(read.index(dl, 2)?)?, 0)?)?,
                )?;
            } else {
                write_typed(&mut self.w, "d", dv, read)?;
            }
        }
        if let Some((_, nd)) = p.note {
            self.w_drawing("note", nd, None)?;
        }
        if let Some(ctl) = p.control {
            let text = read.scalar(read.index(read.list(ctl)?, 0)?)?;
            let text = read.allocation.replace_text(
                text.strip_prefix("#base64:").unwrap_or(text),
                "\r\r\n",
                "\r\n",
            )?;
            self.w
                .leaf("control", Some(&text), [("xsi:type", "xs:base64Binary")])?;
        }
        if let Some(pp) = p.picture_parameter {
            leaf(&mut self.w, "pictureParameter", read.scalar(pp)?)?;
        }
        self.w.close(tag)?;
        Ok(())
    }

    fn w_drawing(&mut self, tag: &str, dr: &Node, zorder: Option<usize>) -> F<()> {
        let read = self.read;
        let dr = read.list(dr)?;
        if let Some(h) = self.hook
            && let Some(r) = h.drawing(self, tag, dr, zorder)
        {
            return r;
        }
        let kind = read.plain_int(read.index(dr, 1)?)?;
        let name = t::DRAWING_TYPES
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, n)| *n);
        let long = dr.len() == 14;
        let Some(name) = name.filter(|_| {
            (dr.len() == 12 || long) && !(long && dr.get(12).is_some_and(Node::is_list))
        }) else {
            return Err(read.semantic("spreadsheet", || {
                format!("drawing type {kind} (chart objects are not decoded)")
            }));
        };
        let p = split_cell_with(read.index(dr, 0)?, read)?;
        self.w.open(tag, [])?;
        leaf(&mut self.w, "drawingType", name)?;
        leaf(&mut self.w, "id", read.scalar(read.index(dr, 10)?)?)?;
        let x = self.xf_with(p.fmt)?;
        leaf_number(&mut self.w, "formatIndex", x)?;
        self.w_text_with(&p, "text")?;
        if let Some(v) = p.v {
            write_typed(&mut self.w, "value", v, read)?;
        }
        if let Some(dp) = p.detail_parameter {
            leaf(&mut self.w, "detailParameter", read.scalar(dp)?)?;
        }
        for (tg, i) in [
            ("beginRow", 3),
            ("beginRowOffset", 5),
            ("endRow", 7),
            ("endRowOffset", 9),
            ("beginColumn", 2),
            ("beginColumnOffset", 4),
            ("endColumn", 6),
            ("endColumnOffset", 8),
        ] {
            leaf(&mut self.w, tg, read.scalar(read.index(dr, i)?)?)?;
        }
        let auto = bool_s(read.index(dr, if long { 13 } else { 11 })?);
        leaf(&mut self.w, "autoSize", auto)?;
        let size = if long {
            *read.at(&t::PICTURE_SIZE, read.plain_int(read.index(dr, 12)?)?)?
        } else {
            "Stretch"
        };
        leaf(&mut self.w, "pictureSize", size)?;
        if let Some(z) = zorder {
            leaf_number(&mut self.w, "zOrder", z)?;
        }
        if kind == 5 {
            leaf(
                &mut self.w,
                "pictureIndex",
                read.scalar(read.index(dr, 11)?)?,
            )?;
        }
        self.w.close(tag)?;
        Ok(())
    }

    fn w_groups(&mut self) -> F<()> {
        let read = self.read;
        let d = self.d;
        let mut levels: Option<i64> = None;
        for (g, _) in d.vg.iter() {
            levels = levels.max(Some(read.plain_int(read.index(read.list(g)?, 2)?)?));
        }
        if let Some(l) = levels {
            let n = l.checked_add(1).ok_or_else(|| read.invalid("level"))?;
            leaf_number(&mut self.w, "vgLevels", n)?;
        }
        leaf_number(&mut self.w, "vgRows", d.height)?;
        for (g, cs) in d.vg.iter() {
            self.w_group("vg", g, cs)?;
        }
        for (g, cs) in d.hg.iter() {
            self.w_group("hg", g, cs)?;
        }
        Ok(())
    }

    fn w_group(&mut self, tag: &str, g: &Node, cs: &Node) -> F<()> {
        let read = self.read;
        let g = read.list(g)?;
        self.w.open(tag, [])?;
        let cs = read.plain_int(cs)?;
        if cs >= 0 {
            let id = self.colset_id(cs)?;
            leaf(&mut self.w, "columnsID", id)?;
        }
        let w = &mut self.w;
        let b = read.index(g, 0)?;
        leaf(w, "b", read.scalar(b)?)?;
        let e = read.index(g, 1)?;
        if e.text() != b.text() || e.is_list() {
            leaf(w, "e", read.scalar(e)?)?;
        }
        let t3 = read.index(g, 3)?;
        if LocalStrings::read(t3, Read::new(read.allocation), Grammar::Spreadsheet)?
            .iter()
            .next()
            .is_some()
        {
            write_local(w, "t", t3, read)?;
        }
        if is(read.index(g, 4)?, "1") {
            leaf(w, "o", "false")?;
        }
        let g5 = read.index(g, 5)?;
        if !is(g5, "0") {
            let v = match g5.text() {
                Some("1") => "Begin",
                Some("2") => "End",
                _ => return Err(read.invalid("group place")),
            };
            leaf(w, "g", v)?;
        }
        w.close(tag)?;
        Ok(())
    }

    fn w_breaks(&mut self) -> F<()> {
        let read = self.read;
        let d = self.d;
        for b in d.vbreaks {
            self.w.open("verticalPageBreak", [])?;
            leaf(&mut self.w, "position", read.scalar(b)?)?;
            self.w.close("verticalPageBreak")?;
        }
        for (b, cs) in d.hbreaks.iter() {
            self.w.open("horizontalPageBreak", [])?;
            leaf(&mut self.w, "position", read.scalar(b)?)?;
            let cs = read.plain_int(cs)?;
            if cs >= 0 {
                let id = self.colset_id(cs)?;
                leaf(&mut self.w, "columnsID", id)?;
            }
            self.w.close("horizontalPageBreak")?;
        }
        Ok(())
    }

    fn w_merges(&mut self) -> F<()> {
        let read = self.read;
        let d = self.d;
        type M<'n> = (&'n Node, &'n Node, &'n Node, &'n Node, Option<&'n str>);
        let mut kinds: [Vector<M<'a>>; 3] = [
            read.allocation.vector(0)?,
            read.allocation.vector(0)?,
            read.allocation.vector(0)?,
        ];
        for (tab, with_id) in [
            (&d.merges, false),
            (&d.col_merges, true),
            (&d.row_merges, false),
        ] {
            for m in read.list(tab)?.iter().skip(1) {
                let m = read.list(m)?;
                let k = read.plain_int(read.index(m, 4)?)?;
                let slot = usize::try_from(k)
                    .ok()
                    .and_then(|k| kinds.get_mut(k))
                    .ok_or_else(|| read.semantic("spreadsheet", || format!("merge kind {k}")))?;
                let cid = if with_id {
                    Some(read.scalar(read.index(m, 5)?)?)
                } else {
                    None
                };
                slot.push((
                    read.index(m, 0)?,
                    read.index(m, 1)?,
                    read.index(m, 2)?,
                    read.index(m, 3)?,
                    cid,
                ))?;
            }
        }
        for (k, tag) in [
            (0, "merge"),
            (2, "verticalUnmerge"),
            (1, "horizontalUnmerge"),
        ] {
            let Some(items) = kinds.get(k) else { continue };
            for (c1, r1, c2, r2, cid) in items.iter() {
                let w = &mut self.w;
                w.open(tag, [])?;
                leaf(w, "r", read.scalar(r1)?)?;
                leaf(w, "c", read.scalar(c1)?)?;
                let h = read
                    .plain_int(r2)?
                    .checked_sub(read.plain_int(r1)?)
                    .ok_or_else(|| read.invalid("merge"))?;
                let wd = read
                    .plain_int(c2)?
                    .checked_sub(read.plain_int(c1)?)
                    .ok_or_else(|| read.invalid("merge"))?;
                if h != 0 {
                    leaf_number(w, "h", h)?;
                }
                if wd != 0 {
                    leaf_number(w, "w", wd)?;
                }
                if let Some(c) = cid
                    && !c.is_empty()
                    && *c != ZERO
                {
                    leaf(w, "columnsID", c)?;
                }
                w.close(tag)?;
            }
        }
        Ok(())
    }

    fn w_area(&mut self, tag: &str, a: &Node) -> F<()> {
        let read = self.read;
        let a = read.list(a)?;
        let k = read.plain_int(read.index(a, 0)?)?;
        let ty = t::AREA_TYPES
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, n)| *n)
            .ok_or_else(|| read.semantic("spreadsheet", || format!("area type {k}")))?;
        let w = &mut self.w;
        w.open(tag, [])?;
        leaf(w, "type", ty)?;
        leaf(w, "beginRow", read.scalar(read.index(a, 2)?)?)?;
        leaf(w, "endRow", read.scalar(read.index(a, 4)?)?)?;
        leaf(w, "beginColumn", read.scalar(read.index(a, 1)?)?)?;
        leaf(w, "endColumn", read.scalar(read.index(a, 3)?)?)?;
        let id = read.index(a, 5)?;
        if !is(id, ZERO) {
            leaf(w, "columnsID", read.scalar(id)?)?;
        }
        w.close(tag)?;
        Ok(())
    }

    fn w_named(&mut self) -> F<()> {
        let read = self.read;
        let d = self.d;
        let nm = read.list(d.named)?;
        for i in 0..count(read.plain_int(read.index(nm, 0)?)?) {
            let name = read.scalar(read.index(nm, read.offset(1, 2, i)?)?)?;
            let v = read.list(read.index(nm, read.offset(2, 2, i)?)?)?;
            match read.plain_int(read.index(v, 0)?)? {
                1 => {
                    self.w.open("namedItem", [("xsi:type", "NamedItemCells")])?;
                    leaf(&mut self.w, "name", name)?;
                    self.w_area("area", read.index(v, 1)?)?;
                    self.w.close("namedItem")?;
                }
                2 => {
                    self.w
                        .open("namedItem", [("xsi:type", "NamedItemDrawing")])?;
                    leaf(&mut self.w, "name", name)?;
                    leaf(&mut self.w, "drawingID", read.scalar(read.index(v, 1)?)?)?;
                    self.w.close("namedItem")?;
                }
                k => return Err(read.semantic("spreadsheet", || format!("named item kind {k}"))),
            }
        }
        Ok(())
    }

    fn w_print(&mut self) -> F<()> {
        let read = self.read;
        let d = self.d;
        let ps = read.list(read.index(read.list(d.print_settings)?, 0)?)?;
        let mut bag = SourceMap::new(read.allocation)?;
        for i in 0..count(read.plain_int(read.index(ps, 1)?)?) {
            let k = read.plain_int(read.index(ps, read.offset(2, 2, i)?)?)?;
            bag.push(k, read.index(ps, read.offset(3, 2, i)?)?)?;
        }
        bag.sort();
        if bag.keys().any(|k| !(6..=11).contains(k)) {
            let w = &mut self.w;
            w.open("printSettings", [])?;
            for (key, tag, conv) in t::PRINT_KEYS {
                let Some(v) = i64::try_from(*key).ok().and_then(|k| bag.get(&k)) else {
                    continue;
                };
                let v = read.list(v)?;
                if *key == 0 {
                    let tg = if is(read.index(v, 0)?, "S") {
                        "pageSize"
                    } else {
                        "paper"
                    };
                    leaf(w, tg, read.scalar(read.index(v, 1)?)?)?;
                    continue;
                }
                let x = t::print_value_ref(*conv, read.scalar(read.index(v, 1)?)?)
                    .ok_or_else(|| read.semantic("spreadsheet", || format!("print {tag}")))?;
                leaf(w, tag, x)?;
            }
            let known = |k: &i64| {
                t::PRINT_KEYS
                    .iter()
                    .any(|(n, _, _)| i64::try_from(*n).ok() == Some(*k))
            };
            if bag.keys().any(|k| !known(k)) {
                return Err(read.invalid("print settings keys"));
            }
            w.close("printSettings")?;
        }
        let pa = &d.print_area;
        if read.plain_int(read.index(read.list(pa)?, 0)?)? != 0 {
            self.w_area("printArea", pa)?;
        }
        Ok(())
    }

    fn w_repeat(&mut self) -> F<()> {
        let read = self.read;
        let u = &self.d.u_after_pa;
        let mut coordinates = [0i64; 4];
        for (index, value) in coordinates.iter_mut().enumerate() {
            *value = read.plain_int(read.index(u, index)?)?;
        }
        let [r0, r1, c0, c1] = coordinates;
        let w = &mut self.w;
        if r0 != 0 || r1 != 0 {
            w.open("repeatRows", [])?;
            leaf(w, "type", "Rows")?;
            leaf_number(w, "beginRow", r0)?;
            leaf_number(w, "endRow", r1)?;
            leaf(w, "beginColumn", "0")?;
            leaf(w, "endColumn", "0")?;
            w.close("repeatRows")?;
        }
        if c0 != 0 || c1 != 0 {
            w.open("repeatColumns", [])?;
            leaf(w, "type", "Columns")?;
            leaf(w, "beginRow", "0")?;
            leaf(w, "endRow", "0")?;
            leaf_number(w, "beginColumn", c0)?;
            leaf_number(w, "endColumn", c1)?;
            w.close("repeatColumns")?;
        }
        Ok(())
    }

    fn w_view(&mut self) -> F<()> {
        let read = self.read;
        let d = self.d;
        if d.save_view == 0 {
            return Ok(());
        }
        leaf(&mut self.w, "saveViewSettings", "true")?;
        let v = read.list(d.view_settings)?;
        let dflt = t::view_default_ref();
        if v.len() == dflt.len() && v.iter().zip(dflt).all(|(a, b)| a.text() == Some(*b)) {
            return Ok(());
        }
        let w = &mut self.w;
        w.open("viewSettings", [])?;
        for (i, tag, kind) in t::VIEW_FIELDS {
            let x = v
                .get(*i)
                .and_then(Node::text)
                .ok_or_else(|| read.invalid("view settings"))?;
            let text = match kind {
                ViewKind::B => {
                    if x == "1" {
                        "true"
                    } else {
                        "false"
                    }
                }
                ViewKind::N | ViewKind::S => x,
            };
            leaf(w, tag, text)?;
        }
        w.close("viewSettings")?;
        Ok(())
    }

    fn w_ui_colors(&mut self) -> F<()> {
        let read = self.read;
        let d = self.d;
        let tags = [
            ("groupsBackColor", "-1"),
            ("groupsColor", "-3"),
            ("headersBackColor", "-1"),
            ("headersColor", "-3"),
        ];
        for ((tag, dflt), v) in tags.into_iter().zip(d.u_after_pa.iter().skip(6)) {
            if d.colors.is_empty() {
                continue;
            }
            let c = read.list(read.at(d.colors, read.plain_int(v)?)?)?;
            if is(read.index(c, 1)?, "3") && first_text_with(read.index(c, 2)?, read)? == Some(dflt)
            {
                continue;
            }
            crate::graph::color::Color::spreadsheet_lookup(c, read, self.cfg)
                .and_then(|color| color.write(&mut self.w, tag, read.allocation))?;
        }
        Ok(())
    }

    fn w_format(&mut self, w: &mut XmlEmit<'_>, f: &Node, drawing: bool) -> F<()> {
        let read = self.read;
        let d = self.d;

        let vals = t::FormatValues::read(read.list(f)?, read)?;
        let get = |bit: u32| vals.get(bit);
        let mut unknown = [0u32; 64];
        let mut unknown_len = 0;
        for (bit, _) in vals.iter().filter(|(bit, _)| !t::is_known_bit(*bit)) {
            if let Some(slot) = unknown.get_mut(unknown_len) {
                *slot = bit;
                unknown_len += 1;
            }
        }
        if unknown_len > 0 {
            let unknown = unknown.get(..unknown_len).unwrap_or_default();
            return Err(read.semantic("spreadsheet", || format!("format bits {unknown:?}")));
        }
        let mark = w.mark();
        w.open("format", [])?;
        if drawing {
            if get(2).is_some() {
                return Err(read.invalid("drawing format bit 2"));
            }
            if let Some(v) = get(4) {
                leaf(w, "print", if is(v, "1") { "false" } else { "true" })?;
            }
            if let Some(v) = get(1) {
                let n = self.lines.refer(read.plain_int(v)?)?;
                leaf_number(w, "drawingBorder", n)?;
            }
            if let Some(v) = get(3) {
                let m = read.plain_int(v)?;
                for (i, tag) in [
                    "drawingHaveLeftBorder",
                    "drawingHaveTopBorder",
                    "drawingHaveRightBorder",
                    "drawingHaveBottomBorder",
                ]
                .iter()
                .enumerate()
                {
                    let on = (m >> i) & 1 == 1;
                    leaf(w, tag, if on { "true" } else { "false" })?;
                }
            }
        }
        for slot in t::FORMAT_ORDER {
            let bit = match slot {
                FormatSlot::Borders => {
                    if !drawing {
                        self.w_borders(w, vals)?;
                    }
                    continue;
                }
                FormatSlot::Bit(b) => *b,
            };
            let Some(v) = get(bit) else { continue };
            let Some((tag, codec)) = t::format_bit(bit) else {
                return Err(read.semantic("spreadsheet", || format!("format bit {bit}")));
            };
            match codec {
                Codec::Font => {
                    let n = self.fonts.refer(read.plain_int(v)?)?;
                    leaf_number(w, tag, n)?;
                }
                Codec::Color => self.w_color(w, tag, v)?,
                Codec::Int => leaf(w, tag, read.scalar(v)?)?,
                Codec::Bool => leaf(w, tag, bool_s(v))?,
                Codec::NBool => leaf(w, tag, if is(v, "1") { "false" } else { "true" })?,
                Codec::FStr => {
                    write_local(w, tag, read.at(d.format_strings, read.plain_int(v)?)?, read)?
                }
                Codec::Mask => write_local(w, tag, read.at(d.masks, read.plain_int(v)?)?, read)?,
                Codec::VType => self.w_type(w, tag, read.at(d.value_types, read.plain_int(v)?)?)?,
                Codec::Uuid => leaf(w, tag, read.scalar(read.at(d.uuids, read.plain_int(v)?)?)?)?,
                Codec::Map(_) | Codec::Pattern => {
                    let x = v
                        .text()
                        .and_then(|k| t::map_value_ref(codec, k))
                        .ok_or_else(|| read.semantic("spreadsheet", || format!("format {tag}")))?;
                    leaf(w, tag, x)?;
                }
            }
        }
        w.close("format")?;
        if w.lines_since(mark) == Some(2) {
            w.truncate(mark);
            w.leaf("format", None, [])?;
        }
        Ok(())
    }

    fn w_borders(&mut self, w: &mut XmlEmit<'_>, vals: t::FormatValues<'_>) -> F<()> {
        let read = self.read;
        let get = |bit| vals.get(bit);
        if let [Some(a), Some(b), Some(c), Some(d)] = [get(1), get(2), get(3), get(4)] {
            let texts = [
                read.scalar(a)?,
                read.scalar(b)?,
                read.scalar(c)?,
                read.scalar(d)?,
            ];
            if texts.iter().all(|value| Some(value) == texts.first()) {
                let n = self.lines.refer(read.plain_int(a)?)?;
                leaf_number(w, "border", n)?;
                return Ok(());
            }
        }
        for (i, tag) in [
            (1, "leftBorder"),
            (2, "topBorder"),
            (3, "rightBorder"),
            (4, "bottomBorder"),
        ] {
            if let Some(v) = get(i) {
                let n = self.lines.refer(read.plain_int(v)?)?;
                leaf_number(w, tag, n)?;
            }
        }
        Ok(())
    }

    fn w_color(&mut self, w: &mut XmlEmit<'_>, tag: &str, v: &Node) -> F<()> {
        let read = self.read;
        let d = self.d;
        let n = read.plain_int(v)?;
        self.colors.refer(n)?;
        let item = read.at(d.colors, n)?;
        let c = self.style_colors.get(&n).copied().unwrap_or(item);
        crate::graph::color::Color::spreadsheet_lookup(read.list(c)?, read, self.cfg)
            .and_then(|color| color.write(w, tag, read.allocation))?;
        Ok(())
    }

    fn w_type(&self, w: &mut XmlEmit<'_>, tag: &str, pat: &Node) -> F<()> {
        let read = self.read;

        super::type_view::TypePattern::read(pat, read, self.cfg)
            .and_then(|pattern| pattern.write(w, tag, read))
    }

    fn w_lines(&mut self) -> F<()> {
        let read = self.read;
        let d = self.d;
        let items = read.list(d.lines)?.get(1..).unwrap_or_default();
        let len = i64::try_from(items.len()).unwrap_or(i64::MAX);
        for &k in self.lines.order.iter() {
            let i = k
                .checked_mul(3)
                .and_then(|x| x.checked_add(1))
                .ok_or_else(|| read.invalid("line index"))?;
            if i < len {
                crate::graph::line::Line::spreadsheet(read.list(read.at(items, i)?)?, read)
                    .and_then(|line| line.write(&mut self.w, "line"))?;
            } else {
                self.w
                    .leaf("line", None, [("width", "0"), ("gap", "false")])?;
            }
        }
        Ok(())
    }

    fn w_fonts(&mut self) -> F<()> {
        let read = self.read;
        let d = self.d;
        for &k in self.fonts.order.iter() {
            crate::graph::font::Font::lookup(
                read.list(read.at(d.fonts, k)?)?,
                read,
                crate::graph::font::Grammar::Spreadsheet,
                self.cfg,
                false,
            )
            .and_then(|font| font.write(&mut self.w, "font"))?;
        }
        Ok(())
    }

    fn w_pictures(&mut self) -> F<()> {
        let read = self.read;
        let d = self.d;
        for (i, p) in d.pictures.iter().enumerate() {
            let w = &mut self.w;
            w.open("picture", [])?;
            leaf_number(w, "index", i)?;
            crate::graph::picture::Picture::lookup(read.list(p)?, read, self.cfg)
                .and_then(|picture| picture.write(w, "picture"))?;
            w.close("picture")?;
        }
        Ok(())
    }

    pub(crate) fn run(self) -> R<String> {
        self.run_with()
            .map_err(native_error)?
            .native()
            .map_err(Unsupported::from)
            .and_then(|bytes| {
                String::from_utf8(bytes).map_err(|_| Unsupported("invalid emitted UTF-8".into()))
            })
    }
}

pub(crate) fn entry_xml_with(
    cfg: &dyn Lookup,
    bytes: &[u8],
    allocation: Allocation<'_>,
) -> F<Owned<Vec<u8>>> {
    let read = Read::new(allocation);
    let source = super::tree::MoxelSource::read(bytes, allocation)
        .map_err(|error| source_error(error, read))?;
    let view = source.view().map_err(|error| source_error(error, read))?;
    Writer::construct(cfg, view, read)?.run_with()
}
pub(crate) fn source_error(error: super::tree::TreeError, read: Read<'_>) -> ReadError {
    match error {
        super::tree::TreeError::Memory(error) => ReadError::Memory(error),
        super::tree::TreeError::Brace(error) => {
            read.semantic("brace", || format!("bad brace: {error}"))
        }
        super::tree::TreeError::Semantic(code) => read.invalid(code),
    }
}
pub(crate) fn entry_xml(cfg: &Configuration, bytes: &[u8]) -> R<Vec<u8>> {
    entry_xml_with(cfg, bytes, Allocation::Native)
        .map_err(native_error)?
        .native()
        .map_err(Into::into)
}
