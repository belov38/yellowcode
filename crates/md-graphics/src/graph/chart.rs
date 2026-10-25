use super::chartfmt::{self, E_START, Fields, Pos};
use super::chartspec::{Codec, SPEC, Src};
use super::read::{R as F, ReadError};
use super::values::{R, is, slice};
use super::{
    color::Color,
    font::Font,
    scalar::{self, Text},
    tvalue::TypedValue,
};
use crate::graph::{
    lstr::{Grammar, LocalStrings},
    read::Read,
};
use crate::templates::allocation::Allocation;
use crate::templates::allocation::Owned;
use crate::templates::context::Lookup;
use crate::xml_out::semantic::SemanticEmit;
use brace::Node;
use std::fmt::Write;

pub(crate) const TEMPLATE_DECLARED: &[&str] = &["style", "pal"];
pub(crate) const FORM_DECLARED: &[&str] = &["style", "pal", "web", "win", "sys"];

const CHART_LINE: &[&str] = &[
    "None",
    "Solid",
    "Dotted",
    "Dashed",
    "DashDotted",
    "DashDottedDotted",
];
const BORDER: &[(i64, &str)] = &[
    (0, "WithoutBorder"),
    (1, "Single"),
    (200, "Double"),
    (2, "Embossed"),
    (3, "Indented"),
    (4, "Underline"),
    (5, "DoubleUnderline"),
    (6, "Rounded"),
    (7, "Overline"),
];
const MARKER: &[&str] = &["None", "Rect", "Circle", "Rhomb", "Auto", "Alternation"];
const LABEL_LOCATION: &[(i64, &str)] = &[
    (0, "Edge"),
    (1, "Center"),
    (2, "EdgeInside"),
    (3, "EdgeAuto"),
    (4, "Auto"),
    (5, "LeftTop"),
    (6, "LeftBottom"),
    (7, "RightTop"),
    (8, "RightBottom"),
    (9, "LeftAndTopSpecified"),
    (10, "EmptySpace"),
];
const ORIENTATION: &[&str] = &["Horizontal", "Vertical", "Auto", "CustomAngle"];
const SHOW_TITLE: &[&str] = &["Auto", "DontShow", "Show"];
const TITLE_SOURCE: &[&str] = &["Auto", "UseText", "AutoText"];
const TITLE_PLACEMENT: &[&str] = &["WithAxis", "PlotArea", "SpecialArea"];
const GRID_SHOW: &[(i64, &str)] = &[(0, "Show"), (1, "DontShow"), (2, "Auto")];
const SCALE_LABEL_LOC: &[&str] = &["Auto", "None", "Inside", "Outside"];
const SCALE_LOC: &[&str] = &["Auto", "BaseValue", "Edge"];
const SCALE_MARK: &[&str] = &["Auto", "Outside", "Inside", "Center", "None"];
const SHOW_IN: &[&str] = &["Auto", "Show", "DontShow"];
const DETECTION: &[&str] = &["AutoDetect", "UseValue", "UseValueWithLimitations"];
const SPLINE: &[&str] = &["None", "SmoothCurve"];
const SELECTION: &[&str] = &["Auto", "ValuesSelection", "PointsSelection", "None"];
const TOOLTIP_SHOW: &[&str] = &["Auto", "ShowForNearestValue", "ShowOnHover", "DontShow"];
const TOOLTIP_FILL: &[&str] = &["Auto", "SingleValue", "AllPointValues"];
const DROP_LINES: &[&str] = &["Auto", "Show", "DontShow"];
const VISUAL_TYPE: &[&str] = &["Auto", "Line", "Step", "Area", "Column", "Column3D"];
const TREND_TYPE: &[&str] = &[
    "Linear",
    "Exponential",
    "Logarithmic",
    "Power",
    "Polynomial",
];
const TREND_FACTOR: &[&str] = &["PointNumber", "PointValue", "Auto"];
const STOCK_VALUE: &[&str] = &["Open", "High", "Low", "Close", "OpenCloseAverage"];
const AUTO_COLOR: &str = "{4,4,{0},4}";
const AUTO_FONT: &str = "{8,3,0,1,100}";
const TREND_LINE: &str = "{4,0,{0},1,1,0,e5cabe59-d992-4d31-8086-3116931aff81,0}";
const EMPTY_LSTR: &str = "{1,0}";
const DEFAULT_AREA: &str = concat!(
    "{1,4,0.5,0.5,{8,3,0,1,100},{4,4,{0},4},{4,4,{0},4},1,",
    "{3,0,{0},0,1,0,48312c09-257f-4b29-b280-284dd89efc1e},{4,4,{0},4},4,2,0}"
);
const ITEM_EXT: &str = "{{1,{1,0},0},0}";
const SERIES_EXT_TAIL: [&str; 6] = ["0", "\"\"", "{1,0}", "{1,0}", "{1,0}", "0"];

fn dc(n: &Node) -> String {
    brace::to_compact(n)
}

fn compact_eq(node: &Node, expected: &str) -> bool {
    fn take(expected: &mut &[u8], part: &[u8]) -> bool {
        if let Some(rest) = expected.strip_prefix(part) {
            *expected = rest;
            true
        } else {
            false
        }
    }
    fn matches(node: &Node, expected: &mut &[u8]) -> bool {
        match node {
            Node::List(items) => {
                if !take(expected, b"{") {
                    return false;
                }
                for (at, item) in items.iter().enumerate() {
                    if at != 0 && !take(expected, b",") {
                        return false;
                    }
                    if !matches(item, expected) {
                        return false;
                    }
                }
                take(expected, b"}")
            }
            Node::Atom(text) => take(expected, text.as_bytes()),
            Node::Str(text) => {
                if !take(expected, b"\"") {
                    return false;
                }
                for (at, part) in text.split('"').enumerate() {
                    if at != 0 && !take(expected, b"\"\"") {
                        return false;
                    }
                    if !take(expected, part.as_bytes()) {
                        return false;
                    }
                }
                take(expected, b"\"")
            }
        }
    }
    let mut expected = expected.as_bytes();
    matches(node, &mut expected) && expected.is_empty()
}

fn b(v: &Node) -> &'static str {
    if is(v, "1") { "true" } else { "false" }
}

fn pick(read: Read<'_>, table: &[&'static str], x: &Node) -> F<&'static str> {
    Ok(*read.at(table, read.int(x)?)?)
}

fn pick_dict(read: Read<'_>, table: &[(i64, &'static str)], x: &Node) -> F<&'static str> {
    let k = read.int(x)?;
    table
        .iter()
        .find(|(c, _)| *c == k)
        .map(|(_, n)| *n)
        .ok_or_else(|| read.semantic("chart data", || format!("chart code {k}")))
}

fn table(name: &str, key: &str) -> Option<&'static str> {
    mdschema::static_template_tables()
        .chart()
        .get(name)?
        .get(key)?
        .as_str()
}

#[expect(
    clippy::large_enum_variant,
    reason = "Bounded stack decimal scratch avoids an uncharged heap allocation"
)]
enum Number<'a> {
    Borrowed(&'a str),
    Formatted(Text<384>),
}
impl std::ops::Deref for Number<'_> {
    type Target = str;
    fn deref(&self) -> &str {
        match self {
            Self::Borrowed(text) => text,
            Self::Formatted(text) => text.as_str(),
        }
    }
}
fn scalar_error(error: scalar::ScalarError, text: &str, read: Read<'_>) -> ReadError {
    match error {
        scalar::ScalarError::Memory(error) => ReadError::Memory(error),
        scalar::ScalarError::Invalid => {
            read.semantic("not a number", || format!("not a number: {text}"))
        }
    }
}
fn double(value: f64, read: Read<'_>) -> F<Text<384>> {
    scalar::xml_double(value).map_err(|error| match error {
        scalar::ScalarError::Memory(error) => ReadError::Memory(error),
        scalar::ScalarError::Invalid => read.semantic("non-finite XML double", || {
            format!("not a finite number: {value}")
        }),
    })
}
fn num_x<'n>(read: Read<'_>, n: &'n Node) -> F<Number<'n>> {
    let text = read.scalar(n)?;
    if text.contains(['e', 'E']) {
        let value = scalar::number(text, read.allocation)
            .map_err(|error| scalar_error(error, text, read))?;
        Ok(Number::Formatted(double(value, read)?))
    } else {
        Ok(Number::Borrowed(text))
    }
}
fn alpha_with(read: Read<'_>, x: &Node) -> F<Text<64>> {
    let n = 255i128 - i128::from(read.int(x)?);
    let p = n * 100;
    if p.unsigned_abs() > 1u128 << 52 {
        return Err(read.semantic("translucence", || format!("translucence {n}")));
    }
    let mut text = Text::new();
    write!(&mut text, "{}", (2 * p + 255).div_euclid(510))
        .map_err(|_| read.invalid("translucence overflow"))?;
    Ok(text)
}

struct O<'w, 'a> {
    w: &'w mut dyn SemanticEmit,
    p: &'a str,
    lookup: &'a dyn Lookup,
    read: Read<'a>,
    declared: &'a [&'a str],
}

impl O<'_, '_> {
    fn t(&self, tag: &str) -> F<Owned<String>> {
        Ok(self.read.allocation.text(&[self.p, ":", tag])?)
    }
    fn leaf(&mut self, tag: &str, text: &str) -> F<()> {
        self.w.leaf_parts(&[self.p, ":", tag], text, false)?;
        Ok(())
    }
    fn text(&mut self, tag: &str, text: &str) -> F<()> {
        self.w.leaf_parts(&[self.p, ":", tag], text, true)?;
        Ok(())
    }
    fn leaf_a(&mut self, tag: &str, text: Option<&str>, attrs: &[(&str, &str)]) -> F<()> {
        let tag = self.t(tag)?;
        self.w.value(&tag, text, false, attrs)?;
        Ok(())
    }
    fn empty(&mut self, tag: &str) -> F<()> {
        self.leaf_a(tag, None, &[])
    }
    fn open(&mut self, tag: &str, attrs: &[(&str, &str)]) -> F<()> {
        let tag = self.t(tag)?;
        self.w.open_attributes(&tag, attrs)?;
        Ok(())
    }
    fn close(&mut self, tag: &str) -> F<()> {
        self.w.close_parts(&[self.p, ":", tag])?;
        Ok(())
    }
    fn color(&mut self, tag: &str, c: &Node) -> F<()> {
        let read = self.read;
        let color = Color::graph(c, read, self.lookup, self.declared, None)?;
        let parts = color.pieces();
        let pre = parts[0];
        if (pre == "web" || pre == "win") && !self.declared.contains(&pre) {
            return Err(read.semantic("chart color local namespace", || {
                format!("chart color {} needs a local namespace", parts.concat())
            }));
        }
        let tag = self.t(tag)?;
        color.write(self.w, &tag, read.allocation)
    }
    fn font(&mut self, tag: &str, f: &Node) -> F<()> {
        let read = self.read;
        let font = Font::lookup(
            read.list(f)?,
            read,
            super::font::Grammar::Graph,
            self.lookup,
            self.declared.contains(&"sys"),
        )?;
        let tag = self.t(tag)?;
        font.write(self.w, &tag)
    }
    fn lstr(&mut self, tag: &str, x: &Node) -> F<()> {
        let tag = self.t(tag)?;
        LocalStrings::read(x, self.read, Grammar::Graph)?.write(self.w, &tag, "v8")
    }
    fn typed(&mut self, tag: &str, x: &Node) -> F<()> {
        let value = TypedValue::read(x, self.read, Grammar::Graph)?;
        let tag = self.t(tag)?;
        value.write(self.w, &tag)
    }

    fn line(&mut self, tag: &str, ln: &Node) -> F<()> {
        let read = self.read;
        let l = read.list(ln)?;
        let width = read.scalar(read.at(l, 4)?)?;
        let gap = b(read.at(l, 7)?);
        self.open(tag, &[("width", width), ("gap", gap)])?;
        let style = pick(read, CHART_LINE, read.at(l, 3)?)?;
        self.w
            .value(
                "v8ui:style",
                Some(style),
                false,
                &[("xsi:type", "v8ui:ChartLineType")],
            )
            .map_err(ReadError::from)?;
        self.close(tag)?;
        Ok(())
    }

    fn border(&mut self, tag: &str, r: &Node) -> F<()> {
        let read = self.read;
        let l = read.list(r)?;
        self.open(tag, &[("width", read.scalar(read.at(l, 4)?)?)])?;
        let style = pick_dict(read, BORDER, read.at(l, 3)?)?;
        self.w
            .value(
                "v8ui:style",
                Some(style),
                false,
                &[("xsi:type", "v8ui:ControlBorderType")],
            )
            .map_err(ReadError::from)?;
        self.close(tag)?;
        Ok(())
    }
}

fn write_trend(o: &mut O, t: &Node) -> F<()> {
    let read = o.read;
    let t = read.list(t)?;
    let f = |i: i64| read.at(t, i);
    o.open("trendline", &[])?;
    if !is(f(1)?, "0") {
        o.leaf("approximationType", pick(read, TREND_TYPE, f(1)?)?)?;
    }
    if !is(f(2)?, "3") {
        o.leaf("approximationDegree", &num_x(read, f(2)?)?)?;
    }
    if !compact_eq(f(3)?, TREND_LINE) {
        o.line("line", f(3)?)?;
    }
    if !is(f(4)?, "2") {
        o.leaf("factor", pick(read, TREND_FACTOR, f(4)?)?)?;
    }
    if !compact_eq(f(5)?, AUTO_COLOR) {
        o.color("color", f(5)?)?;
    }
    if !compact_eq(f(6)?, EMPTY_LSTR) {
        o.lstr("text", f(6)?)?;
    }
    for (i, tag) in [
        (7, "showInLegend"),
        (8, "showEquation"),
        (9, "showDeterminationFactor"),
    ] {
        if !is(f(i)?, "0") {
            o.leaf(tag, b(f(i)?))?;
        }
    }
    if !is(f(10)?, "4") {
        o.leaf("marker", pick(read, MARKER, f(10)?)?)?;
    }
    if !is(f(12)?, "4") {
        o.leaf("stockChartUsedPointValue", pick(read, STOCK_VALUE, f(12)?)?)?;
    }
    write_label_area(o, "equationArea", f(13)?)?;
    o.typed("details", f(11)?)?;
    o.close("trendline")?;
    Ok(())
}

fn write_series(
    o: &mut O,
    tag: &str,
    rec: &[Node],
    ext: &Node,
    point: bool,
    sid: Option<&Node>,
) -> F<()> {
    let read = o.read;
    let [r0, r1, r2, r3, r4, r5, r6, r7, key, info, prio] = rec else {
        return Err(read.semantic("chart data", || format!("series record of {}", rec.len())));
    };
    let ext = read.list(ext)?;
    let (text, changed, iid, color, line, marker, expand, indicator) = if point {
        (r0, r1, r2, read.at(ext, 0)?, r4, r5, r6, r7)
    } else {
        let sx = slice(ext, 2, i64::MAX);
        if !compact_eq(sx.first().ok_or_else(|| read.invalid("series ext"))?, "0")
            || sx.len() != SERIES_EXT_TAIL.len() + 2
            || !sx
                .iter()
                .skip(2)
                .zip(SERIES_EXT_TAIL)
                .all(|(node, text)| compact_eq(node, text))
        {
            return Err(read.semantic("chart data", || {
                format!("series ext {}", dc(&Node::list(ext.to_vec())))
            }));
        }
        (r3, r4, r7, read.at(ext, 0)?, r1, read.at(ext, 1)?, r5, r6)
    };
    o.open(tag, &[])?;
    o.leaf("id", read.scalar(iid)?)?;
    o.color("color", color)?;
    o.line("line", line)?;
    o.leaf("marker", pick(read, MARKER, marker)?)?;
    o.lstr("text", text)?;
    o.leaf("strIsChanged", b(changed))?;
    o.leaf("isExpand", b(expand))?;
    o.leaf("isIndicator", b(indicator))?;
    o.leaf("colorPriority", b(prio))?;
    if !is(read.item(key, 0)?, "U") || !is(read.item(info, 0)?, "U") {
        return Err(read.invalid("series key/valInfo"));
    }
    if let Some(sid) = sid {
        let sl = read.list(sid)?;
        if read.scalar(read.at(sl, 1)?)? != read.scalar(iid)? {
            let id = read.scalar(iid)?;
            return Err(read.semantic("chart data", || format!("series id {id}")));
        }
        let n = read.int(read.at(sl, 2)?)?;
        for t in slice(sl, 3, n.saturating_add(3)) {
            write_trend(o, t)?;
        }
    }
    if !point && !is(read.at(ext, 3)?, "0") {
        o.leaf("visualType", pick(read, VISUAL_TYPE, read.at(ext, 3)?)?)?;
    }
    o.close(tag)?;
    Ok(())
}

fn write_label_area(o: &mut O, tag: &str, a: &Node) -> F<()> {
    let read = o.read;
    let a = read.list(a)?;
    let f = |i: i64| read.at(a, i);
    o.open(tag, &[])?;
    if !is(f(1)?, "4") {
        o.leaf("location", pick_dict(read, LABEL_LOCATION, f(1)?)?)?;
    }
    if !is(f(2)?, "0.5") {
        o.leaf("left", &num_x(read, f(2)?)?)?;
    }
    if !is(f(3)?, "0.5") {
        o.leaf("top", &num_x(read, f(3)?)?)?;
    }
    o.font("font", f(4)?)?;
    o.color("textColor", f(5)?)?;
    o.color("backColor", f(6)?)?;
    if !is(f(7)?, "1") {
        o.leaf("transparent", b(f(7)?))?;
    }
    o.border("border", f(8)?)?;
    o.color("borderColor", f(9)?)?;
    if !is(f(10)?, "4") {
        o.leaf("marker", pick(read, MARKER, f(10)?)?)?;
    }
    if !is(f(11)?, "2") {
        o.leaf("orientation", pick(read, ORIENTATION, f(11)?)?)?;
    }
    if !is(f(12)?, "0") {
        o.leaf("angle", &num_x(read, f(12)?)?)?;
    }
    o.close(tag)?;
    Ok(())
}

struct Scale<'a> {
    show_title: &'a Node,
    title_text_source: &'a Node,
    title_placement: &'a Node,
    title_text: &'a Node,
    title_area: &'a Node,
    grid_lines_show_mode: &'a Node,
    scale_label_location: &'a Node,
    grid_line: Option<&'a Node>,
    grid_line_color: &'a Node,
    label_font: &'a Node,
    label_color: &'a Node,
    label_orientation: &'a Node,
    label_format: &'a Node,
    scale_line: Option<&'a Node>,
    scale_line_color: &'a Node,
    tail: [&'a Node; 6],
}

fn scale_fields<'n>(read: Read<'_>, sc: &'n Node) -> F<Scale<'n>> {
    let sc = read.list(sc)?;
    let f = |i: i64| read.at(sc, i);
    f(0)?;
    let mut p: i64 = 8;
    let grid_line = if is(f(p)?, "1") {
        p += 1;
        Some(f(p)?)
    } else {
        None
    };
    p += 1;
    let [
        grid_line_color,
        label_font,
        label_color,
        label_orientation,
        label_format,
    ] = slice(sc, p, p + 5)
    else {
        return Err(read.invalid("scale record"));
    };
    p += 5;
    let scale_line = if is(f(p)?, "1") {
        p += 1;
        Some(f(p)?)
    } else {
        None
    };
    p += 1;
    let [scale_line_color, m0, m1, m2, m3, m4, m5] = slice(sc, p, p + 7) else {
        return Err(read.invalid("scale record"));
    };
    if usize::try_from(p + 7).ok() != Some(sc.len()) {
        return Err(read.semantic("chart data", || format!("scale length {}", sc.len())));
    }
    Ok(Scale {
        show_title: f(1)?,
        title_text_source: f(2)?,
        title_placement: f(3)?,
        title_text: f(4)?,
        title_area: f(5)?,
        grid_lines_show_mode: f(6)?,
        scale_label_location: f(7)?,
        grid_line,
        grid_line_color,
        label_font,
        label_color,
        label_orientation,
        label_format,
        scale_line,
        scale_line_color,
        tail: [m0, m1, m2, m3, m4, m5],
    })
}

const SCALE_TAIL: [(&str, Option<&[&str]>); 6] = [
    ("maxLabelRows", None),
    ("labelAngle", None),
    ("scaleLocation", Some(SCALE_LOC)),
    ("scaleStep", None),
    ("scaleMarkLocation", Some(SCALE_MARK)),
    ("showInChart", Some(SHOW_IN)),
];

fn scale_is_default(f: &Scale) -> bool {
    is(f.show_title, "0")
        && is(f.title_text_source, "0")
        && is(f.title_placement, "2")
        && compact_eq(f.title_text, EMPTY_LSTR)
        && is(f.grid_lines_show_mode, "2")
        && is(f.scale_label_location, "0")
        && f.grid_line.is_none()
        && compact_eq(f.grid_line_color, AUTO_COLOR)
        && compact_eq(f.label_font, AUTO_FONT)
        && compact_eq(f.label_color, AUTO_COLOR)
        && is(f.label_orientation, "2")
        && compact_eq(f.label_format, EMPTY_LSTR)
        && f.scale_line.is_none()
        && compact_eq(f.scale_line_color, AUTO_COLOR)
        && f.tail.iter().all(|x| is(x, "0"))
        && compact_eq(f.title_area, DEFAULT_AREA)
}

fn write_scale(o: &mut O, tag: &str, sc: &Node) -> F<()> {
    let read = o.read;
    let f = scale_fields(read, sc)?;
    if scale_is_default(&f) {
        return Ok(());
    }
    o.open(tag, &[])?;
    if !is(f.show_title, "0") {
        o.leaf("showTitle", pick(read, SHOW_TITLE, f.show_title)?)?;
    }
    if !is(f.title_text_source, "0") {
        o.leaf(
            "titleTextSource",
            pick(read, TITLE_SOURCE, f.title_text_source)?,
        )?;
    }
    if !compact_eq(f.title_text, EMPTY_LSTR) {
        o.lstr("titleText", f.title_text)?;
    }
    write_label_area(o, "titleArea", f.title_area)?;
    if !is(f.title_placement, "2") {
        o.leaf(
            "titlePlacement",
            pick(read, TITLE_PLACEMENT, f.title_placement)?,
        )?;
    }
    if !is(f.grid_lines_show_mode, "2") {
        o.leaf(
            "gridLinesShowMode",
            pick_dict(read, GRID_SHOW, f.grid_lines_show_mode)?,
        )?;
    }
    if !is(f.scale_label_location, "0") {
        o.leaf(
            "scaleLabelLocation",
            pick(read, SCALE_LABEL_LOC, f.scale_label_location)?,
        )?;
    }
    if let Some(l) = f.grid_line {
        o.line("gridLine", l)?;
    }
    if !compact_eq(f.grid_line_color, AUTO_COLOR) {
        o.color("gridLineColor", f.grid_line_color)?;
    }
    if !compact_eq(f.label_font, AUTO_FONT) {
        o.font("labelFont", f.label_font)?;
    }
    if !compact_eq(f.label_color, AUTO_COLOR) {
        o.color("labelColor", f.label_color)?;
    }
    if !is(f.label_orientation, "2") {
        o.leaf(
            "labelOrientation",
            pick(read, ORIENTATION, f.label_orientation)?,
        )?;
    }
    if !compact_eq(f.label_format, EMPTY_LSTR) {
        o.lstr("labelFormat", f.label_format)?;
    }
    if let Some(l) = f.scale_line {
        o.line("scaleLine", l)?;
    }
    if !compact_eq(f.scale_line_color, AUTO_COLOR) {
        o.color("scaleLineColor", f.scale_line_color)?;
    }
    for ((k, t), x) in SCALE_TAIL.iter().zip(f.tail) {
        if !is(x, "0") {
            let text = match t {
                Some(t) => Number::Borrowed(pick(read, t, x)?),
                None => num_x(read, x)?,
            };
            o.leaf(k, &text)?;
        }
    }
    o.close(tag)?;
    Ok(())
}

fn write_axis(o: &mut O, tag: &str, ax: &Node) -> F<()> {
    let read = o.read;
    let ax = read.list(ax)?;
    let mm = read.at(ax, 2)?;
    let mut kids: [Option<(&str, Number<'_>, bool)>; 5] = std::array::from_fn(|_| None);
    let mut count = 0;
    for (key, index, decimal) in [
        ("baseValue", 1, false),
        ("minValue", 2, true),
        ("maxValue", 4, true),
    ] {
        let node = if decimal {
            read.item(mm, index)?
        } else {
            read.at(ax, index)?
        };
        if !is(node, "0") {
            *kids
                .get_mut(count)
                .ok_or_else(|| read.invalid("axis children"))? =
                Some((key, num_x(read, node)?, decimal));
            count += 1;
        }
    }
    for (key, index) in [
        ("minValueDetectionMethod", 4),
        ("maxValueDetectionMethod", 3),
    ] {
        let node = read.at(ax, index)?;
        if !is(node, "0") {
            *kids
                .get_mut(count)
                .ok_or_else(|| read.invalid("axis children"))? =
                Some((key, Number::Borrowed(pick(read, DETECTION, node)?), false));
            count += 1;
        }
    }
    if !is(read.at(ax, 0)?, "0")
        || !is(read.item(mm, 0)?, "0")
        || !is(read.item(mm, 1)?, "1")
        || !is(read.item(mm, 3)?, "1")
    {
        return Err(read.semantic("chart data", || {
            format!("axis {}", dc(&Node::list(ax.to_vec())))
        }));
    }
    if count == 0 {
        o.empty(tag)?;
        return Ok(());
    }
    o.open(tag, &[])?;
    for (key, text, decimal) in kids.into_iter().flatten() {
        let attrs = if decimal {
            &[("xsi:type", "xs:decimal")][..]
        } else {
            &[][..]
        };
        o.leaf_a(key, Some(&text), attrs)?;
    }
    o.close(tag)?;
    Ok(())
}

fn write_items(o: &mut O, f: &Fields<'_>) -> F<()> {
    let read = o.read;
    if f.items.is_empty() {
        return Ok(());
    }
    o.open("realDataItems", &[])?;
    for (it, ext) in f.items.iter().zip(f.iext) {
        o.open("item", &[])?;
        o.typed("valData", read.at(it, 0)?)?;
        o.typed("valInfo", read.at(it, 1)?)?;
        o.text("toolTip", read.scalar(read.at(it, 2)?)?)?;
        if !compact_eq(ext, ITEM_EXT) {
            return Err(read.semantic("chart data", || format!("data item ext {}", dc(ext))));
        }
        o.close("item")?;
    }
    o.close("realDataItems")?;
    Ok(())
}

fn write_bands(o: &mut O, tag: &str, q: &Node) -> F<()> {
    let read = o.read;
    let q = read.list(q)?;
    let n = read.int(read.at(q, 1)?)?;
    let use_text = b(read.at(q, n.saturating_add(2))?);
    let use_tooltip = b(read.at(q, n.saturating_add(3))?);
    let attrs = [("useTextStr", use_text), ("useTooltipStr", use_tooltip)];
    if n == 0 {
        let t = o.t(tag)?;
        o.w.value(&t, None, false, &attrs)
            .map_err(ReadError::from)?;
        return Ok(());
    }
    o.open(tag, &attrs)?;
    for it in slice(q, 2, n.saturating_add(2)) {
        let it = read.list(it)?;
        o.w.open_attributes("v8ui:item", &[])
            .map_err(ReadError::from)?;
        o.w.value(
            "v8ui:begin",
            Some(&num_x(read, read.at(it, 1)?)?),
            false,
            &[],
        )
        .map_err(ReadError::from)?;
        o.w.value("v8ui:end", Some(&num_x(read, read.at(it, 2)?)?), false, &[])
            .map_err(ReadError::from)?;
        Color::graph(read.at(it, 3)?, read, o.lookup, o.declared, None)?.write(
            o.w,
            "v8ui:backColor",
            read.allocation,
        )?;
        LocalStrings::read(read.at(it, 4)?, read, Grammar::Graph)?.write(o.w, "v8ui:text", "v8")?;
        LocalStrings::read(read.at(it, 5)?, read, Grammar::Graph)?.write(
            o.w,
            "v8ui:tooltip",
            "v8",
        )?;
        o.w.close_parts(&["v8ui:item"]).map_err(ReadError::from)?;
    }
    o.close(tag)?;
    Ok(())
}

fn write_palette(o: &mut O, tag: &str, cp: &Node) -> F<()> {
    let read = o.read;
    let k = read.item(cp, 1)?;
    if is(k, "14") {
        return Ok(());
    }
    let key = read.scalar(k)?;
    let name = table("paletteKind", key)
        .ok_or_else(|| read.semantic("chart data", || format!("palette {key}")))?;
    o.open(tag, &[])?;
    o.leaf("colorPalette", name)?;
    o.close(tag)?;
    Ok(())
}

fn get<'a>(read: Read<'_>, f: &Fields<'a>, src: Src) -> F<&'a Node> {
    match src {
        Src::Head(i) => read.at(f.head, i),
        Src::Ph(i) => read.at(f.points_head, i),
        Src::Scales(i) => read.at(f.scales, i),
        Src::At(part, i) => {
            let (p, base) = f.part(part);
            read.at(p, idx(read, Pos::from(i) - base)?)
        }
        Src::Special => Err(read.invalid("chart source")),
    }
}

fn idx(read: Read<'_>, x: Pos) -> F<i64> {
    i64::try_from(x).map_err(|_| read.invalid("chart index"))
}

pub(crate) fn write_chart(
    w: &mut dyn SemanticEmit,
    value: &[Node],
    prefix: &str,
    cfg: &dyn Lookup,
    declared: &[&str],
) -> R<()> {
    write_chart_with(w, value, prefix, cfg, declared, Allocation::Native).map_err(ReadError::legacy)
}
pub(crate) fn write_chart_with(
    w: &mut dyn SemanticEmit,
    value: &[Node],
    prefix: &str,
    cfg: &dyn Lookup,
    declared: &[&str],
    allocation: Allocation<'_>,
) -> F<()> {
    let read = Read::new(allocation);
    let h = read.at(value, 0)?;
    if !compact_eq(h, "{11}") {
        return Err(read.semantic("chart data", || format!("chart header {}", dc(h))));
    }
    let f = chartfmt::split_with(read.list(read.at(value, 1)?)?, allocation)?;
    let mut o = O {
        w,
        p: prefix,
        lookup: cfg,
        read,
        declared,
    };
    let mut sids = allocation.vector(f.sids.len())?;
    for (ordinal, x) in f.sids.iter().enumerate() {
        if let Some(key) = read.item(x, 1)?.text() {
            sids.push((key, ordinal, x))?;
        }
    }
    sids.sort_unstable_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(&b.1)));
    let sid_of = |rec: &[Node]| -> F<Option<&Node>> {
        let Some(key) = read.at(rec, 7)?.text() else {
            return Ok(None);
        };
        let end = sids.partition_point(|(candidate, _, _)| *candidate <= key);
        Ok(end
            .checked_sub(1)
            .and_then(|at| sids.get(at))
            .filter(|(candidate, _, _)| *candidate == key)
            .map(|(_, _, value)| *value))
    };
    for &(name, src, codec) in SPEC {
        match name {
            "realSeriesData" => {
                for (i, rec) in f.series.iter().enumerate() {
                    let ext = f.sext.get(i).ok_or_else(|| read.invalid("series ext"))?;
                    let sid = sid_of(rec)?;
                    write_series(&mut o, name, rec, ext, false, sid)?;
                }
                continue;
            }
            "realExSeriesData" => {
                if let Some(ex) = f.exseries {
                    let ext = f
                        .sext
                        .get(f.series.len())
                        .ok_or_else(|| read.invalid("series ext"))?;
                    let sid = sid_of(ex)?;
                    write_series(&mut o, name, ex, ext, false, sid)?;
                }
                continue;
            }
            "realPointData" => {
                for (rec, ext) in f.points.iter().zip(f.pext) {
                    write_series(&mut o, name, rec, ext, true, None)?;
                }
                continue;
            }
            "realDataItems" => {
                write_items(&mut o, &f)?;
                continue;
            }
            "splineMode" => {
                let x = read.at(f.b, idx(read, 128 - chartfmt::B_START)?)?;
                if read.int(x)? != 0 {
                    o.leaf(name, pick(read, SPLINE, x)?)?;
                }
                continue;
            }
            "gaugeQualityBands" => {
                write_bands(&mut o, name, get(read, &f, src)?)?;
                continue;
            }
            "valuesAxis" | "pointsAxis" => {
                write_axis(&mut o, name, get(read, &f, src)?)?;
                continue;
            }
            "pointsScale" | "valuesScale" | "seriesScale" => {
                write_scale(&mut o, name, get(read, &f, src)?)?;
                continue;
            }
            "colorPaletteDescription" => {
                write_palette(&mut o, name, get(read, &f, src)?)?;
                continue;
            }
            _ => {}
        }
        let x = get(read, &f, src)?;
        if matches!(
            name,
            "legendPlacement" | "plotAreaPlacement" | "titleAreaPlacement"
        ) && is(x, "0")
        {
            continue;
        }
        match codec {
            Codec::Num => o.leaf(name, &num_x(read, x)?)?,
            Codec::Bool => o.leaf(name, b(x))?,
            Codec::Enum => {
                let key = read.scalar(x)?;
                let t = table(name, key)
                    .ok_or_else(|| read.semantic("chart data", || format!("{name} code {key}")))?;
                o.leaf(name, t)?;
            }
            Codec::Text => o.text(name, read.scalar(x)?)?,
            Codec::LStr => o.lstr(name, x)?,
            Codec::Color => o.color(name, x)?,
            Codec::Font => o.font(name, x)?,
            Codec::Line => o.line(name, x)?,
            Codec::Border => o.border(name, x)?,
            Codec::Pct => {
                let text = read.scalar(x)?;
                let p = scalar::number(text, allocation)
                    .map_err(|error| scalar_error(error, text, read))?
                    * 100.0;
                o.leaf(name, double(p, read)?.as_str())?;
            }
            Codec::Alpha => o.leaf(name, alpha_with(read, x)?.as_str())?,
            Codec::Rect => {
                let Src::At(_, base) = src else {
                    return Err(read.invalid("rect source"));
                };
                let i = idx(read, Pos::from(base) - E_START)?;
                let r = slice(f.e, i, i.saturating_add(4));
                o.open(name, &[])?;
                o.leaf("left", &num_x(read, read.at(r, 0)?)?)?;
                o.leaf("right", &num_x(read, read.at(r, 2)?)?)?;
                o.leaf("top", &num_x(read, read.at(r, 1)?)?)?;
                o.leaf("bottom", &num_x(read, read.at(r, 3)?)?)?;
                o.close(name)?;
            }
            Codec::Raw => return Err(read.semantic("codec", || format!("codec of {name}"))),
        }
    }
    let tt = read.at(f.f, idx(read, 201 - chartfmt::F_START)?)?;
    for (i, tag, t) in [
        (1, "valuesToolTipShowMode", TOOLTIP_SHOW),
        (2, "valuesToolTipFillType", TOOLTIP_FILL),
        (3, "pointsDropLinesShowMode", DROP_LINES),
        (4, "valuesDropLinesShowMode", DROP_LINES),
    ] {
        let x = read.item(tt, i)?;
        if !is(x, "0") {
            o.leaf(tag, pick(read, t, x)?)?;
        }
    }
    let sel = read.at(f.f, idx(read, 203 - chartfmt::F_START)?)?;
    if !is(sel, "0") {
        o.leaf("selectionMode", pick(read, SELECTION, sel)?)?;
    }
    let rad = read.at(f.g, idx(read, 205 - chartfmt::G_START)?)?;
    if !is(rad, "60") {
        o.leaf("innerRadiusDonutChart", &num_x(read, rad)?)?;
    }
    Ok(())
}
