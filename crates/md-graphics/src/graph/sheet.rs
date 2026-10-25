use super::chart::{self, TEMPLATE_DECLARED};
use super::read::{R as F, Read, ReadError};
use super::scalar::Text;
use super::values::{GraphError, R, is};
use crate::Configuration;
use crate::templates::mxlxml::{self, DrawingHook, Unsupported, Writer, bool_s, split_cell_with};
use crate::templates::{allocation::Allocation, tree};
use crate::templates::{allocation::Owned, context::Lookup};
use brace::Node;
use std::fmt::Write;

pub(crate) const CHART_DRAWING: &str = "a8b97779-1a4b-4059-b09c-807f86d2a461";
const CHART_NS: &str = "http://v8.1c.ru/8.2/data/chart";

impl From<Unsupported> for GraphError {
    fn from(e: Unsupported) -> Self {
        GraphError(e.0)
    }
}

struct ChartHook;

impl DrawingHook for ChartHook {
    fn drawing(
        &self,
        w: &mut Writer<'_>,
        tag: &str,
        dr: &[Node],
        zorder: Option<usize>,
    ) -> Option<crate::graph::read::R<()>> {
        let object = dr.get(1).is_some_and(|k| is(k, "10"))
            && dr.len() == 14
            && dr.get(12).is_some_and(Node::is_list);
        if !object {
            return None;
        }
        Some(chart_drawing(w, tag, dr, zorder))
    }
}

fn number(value: usize, read: Read<'_>) -> F<Text<64>> {
    let mut text = Text::new();
    write!(&mut text, "{value}").map_err(|_| read.invalid("drawing number overflow"))?;
    Ok(text)
}

fn leaf(w: &mut Writer<'_>, tag: &str, text: &str) -> F<()> {
    w.w.leaf(tag, Some(text), []).map_err(ReadError::from)
}

fn chart_drawing(w: &mut Writer<'_>, tag: &str, dr: &[Node], zorder: Option<usize>) -> F<()> {
    let read = w.read;
    let kind = read.at(dr, 11)?;
    if !is(kind, CHART_DRAWING) {
        return Err(read.semantic("drawing object", || {
            format!(
                "drawing object {} (Gantt chart) is not converted",
                brace::to_compact(kind)
            )
        }));
    }
    let p = split_cell_with(read.at(dr, 0)?, read)?;
    w.w.open(tag, []).map_err(ReadError::from)?;
    leaf(w, "drawingType", "Chart")?;
    leaf(w, "id", read.scalar(read.at(dr, 10)?)?)?;
    let x = w.xf_with(p.fmt)?;
    leaf(w, "formatIndex", number(x, read)?.as_str())?;
    w.w_text_with(&p, "text")?;
    if let Some(v) = p.v {
        crate::graph::tvalue::TypedValue::read(v, w.read, crate::graph::lstr::Grammar::Spreadsheet)
            .and_then(|value| value.write(&mut w.w, "value"))?;
    }
    if let Some(dp) = p.detail_parameter {
        leaf(w, "detailParameter", read.scalar(dp)?)?;
    }
    for (t, i) in [
        ("beginRow", 3),
        ("beginRowOffset", 5),
        ("endRow", 7),
        ("endRowOffset", 9),
        ("beginColumn", 2),
        ("beginColumnOffset", 4),
        ("endColumn", 6),
        ("endColumnOffset", 8),
    ] {
        leaf(w, t, read.scalar(read.at(dr, i)?)?)?;
    }
    leaf(w, "autoSize", bool_s(read.at(dr, 13)?))?;
    leaf(w, "pictureSize", "Stretch")?;
    if let Some(z) = zorder {
        leaf(w, "zOrder", number(z, read)?.as_str())?;
    }
    let depth =
        w.w.depth()
            .checked_add(1)
            .ok_or_else(|| read.invalid("chart namespace depth"))?;
    let mut prefix = Text::<64>::new();
    write!(&mut prefix, "d{depth}p1").map_err(|_| read.invalid("chart namespace prefix"))?;
    let prefix = prefix.as_str();
    let mut xmlns = Text::<96>::new();
    write!(&mut xmlns, "xmlns:{prefix}").map_err(|_| read.invalid("chart namespace attribute"))?;
    let mut xsi = Text::<96>::new();
    write!(&mut xsi, "{prefix}:Chart").map_err(|_| read.invalid("chart type name"))?;
    w.w.open(
        "object",
        [(xmlns.as_str(), CHART_NS), ("xsi:type", xsi.as_str())],
    )?;
    let value = read.list(read.at(dr, 12)?)?;
    chart::write_chart_with(
        &mut w.w,
        value,
        prefix,
        w.cfg,
        TEMPLATE_DECLARED,
        read.allocation,
    )?;
    w.w.close("object").map_err(ReadError::from)?;
    w.w.close(tag).map_err(ReadError::from)?;
    Ok(())
}

pub(crate) fn sheet_xml(cfg: &Configuration, data: &[u8]) -> R<Vec<u8>> {
    sheet_xml_with(cfg, data, Allocation::Native)
        .map_err(ReadError::legacy)?
        .native()
        .map_err(|error| GraphError(error.to_string()))
}
pub(crate) fn sheet_xml_with(
    cfg: &dyn Lookup,
    data: &[u8],
    allocation: Allocation<'_>,
) -> F<Owned<Vec<u8>>> {
    let read = Read::new(allocation);
    let source = tree::MoxelSource::read(data, allocation)
        .map_err(|error| mxlxml::source_error(error, read))?;
    let view = source
        .view()
        .map_err(|error| mxlxml::source_error(error, read))?;
    Writer::construct(cfg, view, read)?
        .with_hook(&ChartHook)
        .run_with()
}

pub(crate) fn template_xml_with(
    cfg: &dyn Lookup,
    data: &[u8],
    allocation: Allocation<'_>,
) -> F<Owned<Vec<u8>>> {
    let read = Read::new(allocation);
    let source = tree::MoxelSource::read(data, allocation)
        .map_err(|error| mxlxml::source_error(error, read))?;
    let view = source
        .view()
        .map_err(|error| mxlxml::source_error(error, read))?;
    match Writer::construct(cfg, view, read).and_then(Writer::run_with) {
        Err(ReadError::Semantic { .. }) => {
            let view = source
                .view()
                .map_err(|error| mxlxml::source_error(error, read))?;
            Writer::construct(cfg, view, read)?
                .with_hook(&ChartHook)
                .run_with()
        }
        result => result,
    }
}
