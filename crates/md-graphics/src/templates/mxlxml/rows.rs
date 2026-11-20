use super::{F, MoxelView, Read, RowCoverage, SourceMap, Vector};
use crate::templates::moxel_view::Cells;
#[derive(Clone, Copy)]
pub(super) struct RowSpan<'a> {
    pub begin: i64,
    pub end: i64,
    pub format: i64,
    pub cells: Cells<'a>,
}
fn gap<'a>(
    out: &mut Vector<RowSpan<'a>>,
    mut begin: i64,
    end: i64,
    columns: &SourceMap<i64, i64>,
) -> F<()> {
    while begin <= end {
        let last = if columns.get(&begin).is_some() {
            begin
        } else {
            let next = columns.entries.partition_point(|(row, _, _)| *row <= begin);
            columns
                .entries
                .get(next)
                .filter(|(row, _, _)| *row <= end)
                .and_then(|(row, _, _)| row.checked_sub(1))
                .unwrap_or(end)
        };
        out.push(RowSpan {
            begin,
            end: last,
            format: 0,
            cells: Cells::Flat(&[]),
        })?;
        let Some(next) = last.checked_add(1) else {
            break;
        };
        begin = next;
    }
    Ok(())
}
pub(super) fn spans<'a>(
    document: &MoxelView<'a>,
    columns: &SourceMap<i64, i64>,
    read: Read<'_>,
    coverage: RowCoverage,
) -> F<Vector<RowSpan<'a>>> {
    if document.height < 0 {
        return Err(read.invalid("height"));
    }
    let mut out = read.allocation.vector(0)?;
    let mut next = Some(0);
    for (index, format, cells) in document.rows.iter() {
        let first = next.ok_or_else(|| read.invalid("row index order"))?;
        if index < first {
            return Err(read.invalid("row index order"));
        }
        if coverage == RowCoverage::FullHeight && first < document.height {
            let last = index
                .min(document.height)
                .checked_sub(1)
                .ok_or_else(|| read.invalid("row index overflow"))?;
            gap(&mut out, first, last, columns)?;
        }
        out.push(RowSpan {
            begin: index,
            end: index,
            format,
            cells,
        })?;
        next = index.checked_add(1);
    }
    if let Some(first) =
        next.filter(|first| coverage == RowCoverage::FullHeight && *first < document.height)
    {
        let last = document
            .height
            .checked_sub(1)
            .ok_or_else(|| read.invalid("height"))?;
        gap(&mut out, first, last, columns)?;
    }
    Ok(out)
}
