use super::{
    BuildError, Encoder, HEADERS, R, ZERO,
    nodes::{N, Nodes, copy},
    tables, values,
    xml::*,
};
use memory_budget::{BudgetedVec, Category};
impl Encoder<'_, '_, '_> {
    pub(super) fn run(mut self) -> R<N> {
        let cx = self.cx;
        let default = self.prepare()?;
        let mut out = Nodes::new(cx)?;
        for v in [8, 1, 12] {
            out.push(cx.int(v)?)?;
        }
        out.push(self.language()?)?;
        out.push(default)?;
        let mut lines = Nodes::new(cx)?;
        lines.push(cx.int(self.line_order.len())?)?;
        for i in &*self.line_order {
            lines.push(cx.int(1)?)?;
            lines.push(values::line(
                *self
                    .lines
                    .get(*i)
                    .ok_or(BuildError::Invalid("line index"))?,
                cx,
            )?)?;
            lines.push(cx.int(0)?)?;
        }
        out.push(lines.finish(cx)?)?;
        out.push(cx.int(0)?)?;
        for tag in HEADERS {
            out.push(self.cell(child(self.root, tag))?)?;
        }
        out.push(cx.int(i64::from(flag(self.root, "templateMode", false)?))?)?;
        let step = match text(self.root, "stepDirection", "ByColumns")? {
            "ByColumns" => 2,
            "ByRows" => 1,
            "WithoutMove" => 3,
            _ => return Err(BuildError::Invalid("step direction")),
        };
        out.push(cx.int(step)?)?;
        let mut rows = BudgetedVec::new(cx.budget, Category::Metadata)?;
        for ri in self.root.children().filter(|n| n.local() == "rowsItem") {
            cx.check()?;
            shape(ri, &["index", "indexTo", "row"], &[], &[])?;
            let begin = number(ri, "index", -1)?;
            let end = number(ri, "indexTo", begin)?;
            if begin < 0 || end < begin || end - begin >= 1_000_000 {
                return Err(BuildError::Invalid("row range"));
            }
            let row = child(ri, "row").ok_or(BuildError::Invalid("missing row"))?;
            shape(
                row,
                &["formatIndex", "columnsID", "c", "empty"],
                &["c"],
                &[],
            )?;
            if child(row, "empty").is_some()
                && (flag(row, "empty", false)? == row.children().any(|c| c.local() == "c"))
            {
                return Err(BuildError::Invalid("empty row"));
            }
            if begin != end && row.children().any(|c| c.local() == "c") {
                return Err(BuildError::Unsupported("range of nonempty rows"));
            }
            for index in begin..=end {
                cx.check()?;
                if rows.len() >= 1_000_000 {
                    return Err(BuildError::Invalid("row count limit"));
                }
                rows.try_push((index, row, index == begin))?;
            }
        }
        rows.as_mut_slice().sort_unstable_by_key(|(i, _, _)| *i);
        if rows
            .windows(2)
            .any(|p| p.first().map(|x| x.0) == p.get(1).map(|x| x.0))
        {
            return Err(BuildError::Invalid("overlapping rows"));
        }
        let mut row_columns = BudgetedVec::new(cx.budget, Category::Metadata)?;
        out.push(cx.int(rows.len())?)?;
        for (idx, row, first) in &*rows {
            cx.check()?;
            out.push(cx.int(idx)?)?;
            out.push(cx.int(self.index(number(*row, "formatIndex", 0)?)?)?)?;
            let cells = if *first {
                row.children().filter(|c| c.local() == "c").count()
            } else {
                0
            };
            out.push(cx.int(cells)?)?;
            let mut column = -1i64;
            if *first {
                for c in row.children().filter(|c| c.local() == "c") {
                    shape(c, &["i", "c"], &[], &[])?;
                    let index = number(
                        c,
                        "i",
                        column
                            .checked_add(1)
                            .ok_or(BuildError::Invalid("cell column"))?,
                    )?;
                    if index < 0 || index <= column {
                        return Err(BuildError::Invalid("cell column order"));
                    }
                    column = index;
                    out.push(cx.int(column)?)?;
                    out.push(self.cell(Some(
                        child(c, "c").ok_or(BuildError::Invalid("missing cell"))?,
                    ))?)?;
                }
            }
            if let Some(id) = child(*row, "columnsID") {
                row_columns.try_push((*idx, self.colset_index(leaf(id)?)?))?;
            }
        }
        let mut colsets = self.root.children().filter(|c| c.local() == "columns");
        out.push(if let Some(c) = colsets.next() {
            self.colset(c)?
        } else {
            cx.list([cx.int(0)?, cx.int(0)?, cx.atom(ZERO)?, cx.int(0)?])?
        })?;
        let height = number(self.root, "height", 0)?;
        if height < 0 {
            return Err(BuildError::Invalid("height"));
        }
        if number(self.root, "vgRows", height)? != height {
            return Err(BuildError::Invalid("vgRows differs from height"));
        }
        out.push(cx.int(height)?)?;
        out.push(
            cx.int(
                self.root
                    .children()
                    .filter(|c| c.local() == "columns")
                    .skip(1)
                    .count(),
            )?,
        )?;
        for c in colsets {
            out.push(self.colset(c)?)?;
        }
        out.push(cx.int(row_columns.len())?)?;
        for (idx, cs) in &*row_columns {
            out.push(cx.int(idx)?)?;
            out.push(cx.int(cs)?)?;
        }
        let drawings = self.root.children().filter(|c| c.local() == "drawing");
        let mut ids = BudgetedVec::new(cx.budget, Category::Metadata)?;
        for d in self.root.children().filter(|c| c.local() == "drawing") {
            let id = number(d, "id", 0)?;
            if id < 0 || ids.contains(&id) {
                return Err(BuildError::Invalid("duplicate or negative drawing id"));
            }
            ids.try_push(id)?;
        }
        out.push(cx.int(ids.iter().copied().max().unwrap_or(0))?)?;
        out.push(
            cx.int(
                self.root
                    .children()
                    .filter(|c| c.local() == "drawing")
                    .count(),
            )?,
        )?;
        for (z, d) in drawings.enumerate() {
            out.push(self.drawing(d, Some(z + 1))?)?;
        }
        for tag in ["vg", "hg"] {
            self.groups(tag, &mut out)?;
        }
        for tag in ["verticalPageBreak", "horizontalPageBreak"] {
            let breaks = self.root.children().filter(|c| c.local() == tag);
            out.push(cx.int(self.root.children().filter(|c| c.local() == tag).count())?)?;
            for b in breaks {
                shape(b, &["position", "columnsID"], &[], &[])?;
                out.push(cx.int(number(b, "position", 0)?)?)?;
                if tag == "horizontalPageBreak" {
                    out.push(cx.int(self.colset_index(text(b, "columnsID", "")?)?)?)?;
                } else if child(b, "columnsID").is_some() {
                    return Err(BuildError::Unsupported("vertical break column set"));
                }
            }
        }
        for section in 0..3 {
            out.push(self.merges(section)?)?;
        }
        out.push(self.named(&ids)?)?;
        out.push(cx.string(text(self.root, "printSettingsName", "")?)?)?;
        out.push(cx.list([self.print_settings()?])?)?;
        out.push(self.area(child(self.root, "printArea"))?)?;
        for (tag, begin, end) in [
            ("repeatRows", "beginRow", "endRow"),
            ("repeatColumns", "beginColumn", "endColumn"),
        ] {
            if let Some(e) = child(self.root, tag) {
                shape(
                    e,
                    &["type", "beginRow", "endRow", "beginColumn", "endColumn"],
                    &[],
                    &[],
                )?;
                let expected = if tag == "repeatRows" {
                    "Rows"
                } else {
                    "Columns"
                };
                if text(e, "type", expected)? != expected {
                    return Err(BuildError::Invalid("repeat area type"));
                }
                let other = if tag == "repeatRows" {
                    ["beginColumn", "endColumn"]
                } else {
                    ["beginRow", "endRow"]
                };
                for name in other {
                    if number(e, name, 0)? != 0 {
                        return Err(BuildError::Unsupported("repeat cross coordinates"));
                    }
                }
                out.push(cx.int(number(e, begin, 0)?)?)?;
                out.push(cx.int(number(e, end, 0)?)?)?;
            } else {
                out.push(cx.int(0)?)?;
                out.push(cx.int(0)?)?;
            }
        }
        for v in [0, 0] {
            out.push(cx.int(v)?)?;
        }
        for v in &*self.group_colors {
            out.push(cx.int(v)?)?;
        }
        out.push(cx.int(self.records.len())?)?;
        self.records.as_mut_slice().reverse();
        while let Some(v) = self.records.pop() {
            out.push(v)?;
        }
        out.push(cx.int(self.font_order.len())?)?;
        for i in &*self.font_order {
            out.push(values::font(
                *self
                    .fonts
                    .get(*i)
                    .ok_or(BuildError::Invalid("font index"))?,
                cx,
                self.lookup,
            )?)?;
        }
        self.strings.write(&mut out, cx)?;
        self.types.write(&mut out, cx)?;
        self.uuids.write(&mut out, cx)?;
        out.push(cx.int(self.colors.values.len())?)?;
        let mut style_map = Nodes::new(cx)?;
        style_map.push(
            cx.int(
                self.colors
                    .values
                    .iter()
                    .filter(|n| tables::resolved_color(n).is_some())
                    .count(),
            )?,
        )?;
        for (i, color) in self.colors.values.iter().enumerate() {
            if let Some(absolute) = tables::resolved_color(color) {
                out.push(cx.list([cx.int(4)?, cx.int(0)?, cx.ints([absolute])?, cx.int(0)?])?)?;
                style_map.push(cx.int(i)?)?;
                style_map.push(copy(color, cx)?)?;
            } else {
                out.push(copy(color, cx)?)?;
            }
        }
        let pictures = self.root.children().filter(|c| c.local() == "picture");
        out.push(
            cx.int(
                self.root
                    .children()
                    .filter(|c| c.local() == "picture")
                    .count(),
            )?,
        )?;
        for (index, p) in pictures.enumerate() {
            shape(p, &["index", "picture"], &[], &[])?;
            if number(p, "index", index as i64)? != index as i64 {
                return Err(BuildError::Unsupported("picture table index order"));
            }
            out.push(
                self.picture(child(p, "picture").ok_or(BuildError::Invalid("missing picture"))?)?,
            )?;
        }
        let save = flag(self.root, "saveViewSettings", false)?;
        out.push(cx.int(i64::from(save))?)?;
        if save {
            out.push(cx.int(i64::from(flag(self.root, "readOnly", false)?))?)?;
            out.push(self.view()?)?;
        }
        out.push(cx.int(0)?)?;
        out.push(cx.string(text(self.root, "usedFileName", "")?)?)?;
        if !save {
            out.push(cx.int(i64::from(flag(self.root, "readOnly", false)?))?)?;
            out.push(self.view()?)?;
        }
        out.push(style_map.finish(cx)?)?;
        self.masks.write(&mut out, cx)?;
        for value in [0, 0, 1, 0, 0, 0] {
            out.push(cx.int(value)?)?;
        }
        cx.check()?;
        out.finish(cx)
    }
    fn language(&self) -> R<N> {
        let cx = self.cx;
        let mut out = Nodes::new(cx)?;
        let Some(e) = child(self.root, "languageSettings") else {
            return cx.list([
                cx.string(if self.frame == super::Frame::FormSettings {
                    "#"
                } else {
                    ""
                })?,
                cx.string("")?,
                cx.int(0)?,
                cx.int(0)?,
                cx.int(0)?,
            ]);
        };
        shape(
            e,
            &["currentLanguage", "defaultLanguage", "languageInfo"],
            &["languageInfo"],
            &[],
        )?;
        for tag in ["currentLanguage", "defaultLanguage"] {
            out.push(cx.string(text(e, tag, "")?)?)?;
        }
        out.push(cx.int(0)?)?;
        out.push(cx.int(e.children().filter(|c| c.local() == "languageInfo").count())?)?;
        for it in e.children().filter(|c| c.local() == "languageInfo") {
            shape(it, &["id", "code", "description"], &[], &[])?;
            for tag in ["id", "code", "description"] {
                out.push(cx.string(text(it, tag, "")?)?)?;
            }
        }
        out.push(cx.int(0)?)?;
        out.finish(cx)
    }
    pub(super) fn colset_index(&self, id: &str) -> R<i64> {
        if id.is_empty() || id == ZERO {
            return Ok(-1);
        }
        let id = brace::Guid::parse(id).ok_or(BuildError::Invalid("column set GUID"))?;
        for (index, c) in self
            .root
            .children()
            .filter(|c| c.local() == "columns")
            .skip(1)
            .enumerate()
        {
            if brace::Guid::parse(text(c, "id", "")?) == Some(id) {
                return i64::try_from(index).map_err(|_| BuildError::Invalid("column set index"));
            }
        }
        Err(BuildError::Invalid("unresolved column set"))
    }
    fn colset(&self, e: X<'_, '_>) -> R<N> {
        shape(
            e,
            &["size", "formatIndex", "id", "columnsItem"],
            &["columnsItem"],
            &[],
        )?;
        let cx = self.cx;
        let mut out = Nodes::new(cx)?;
        let size = number(e, "size", 0)?;
        if size < 0 {
            return Err(BuildError::Invalid("column set size"));
        }
        out.push(cx.int(size)?)?;
        out.push(cx.int(self.index(number(e, "formatIndex", 0)?)?)?)?;
        let id = brace::Guid::parse(text(e, "id", ZERO)?)
            .ok_or(BuildError::Invalid("column set GUID"))?;
        out.push(cx.int(id)?)?;
        let cols = e.children().filter(|c| c.local() == "columnsItem");
        out.push(cx.int(e.children().filter(|c| c.local() == "columnsItem").count())?)?;
        let mut previous = -1;
        for c in cols {
            shape(c, &["index", "column"], &[], &[])?;
            let index = number(c, "index", -1)?;
            if index <= previous || index < 0 || index >= size {
                return Err(BuildError::Invalid("column index"));
            }
            previous = index;
            let col = child(c, "column").ok_or(BuildError::Invalid("missing column"))?;
            shape(col, &["formatIndex"], &[], &[])?;
            out.push(cx.int(index)?)?;
            out.push(cx.int(self.index(number(col, "formatIndex", 0)?)?)?)?;
        }
        out.finish(cx)
    }
    pub(super) fn area(&self, e: Option<X<'_, '_>>) -> R<N> {
        let cx = self.cx;
        let Some(e) = e else {
            return cx.list([
                cx.int(0)?,
                cx.int(-1)?,
                cx.int(-1)?,
                cx.int(-1)?,
                cx.int(-1)?,
                cx.atom(ZERO)?,
            ]);
        };
        shape(
            e,
            &[
                "type",
                "beginColumn",
                "beginRow",
                "endColumn",
                "endRow",
                "columnsID",
            ],
            &[],
            &[],
        )?;
        let kind = super::super::mxltables::AREA_TYPES
            .iter()
            .find(|(_, n)| Some(*n) == text(e, "type", "").ok())
            .map(|(k, _)| *k)
            .ok_or(BuildError::Invalid("area type"))?;
        let mut out = Nodes::new(cx)?;
        out.push(cx.int(kind)?)?;
        for tag in ["beginColumn", "beginRow", "endColumn", "endRow"] {
            out.push(cx.int(number(e, tag, -1)?)?)?;
        }
        let id = text(e, "columnsID", ZERO)?;
        self.colset_index(id)?;
        out.push(
            cx.int(brace::Guid::parse(id).ok_or(BuildError::Invalid("area column set GUID"))?)?,
        )?;
        out.finish(cx)
    }
    fn named(&self, ids: &[i64]) -> R<N> {
        let cx = self.cx;
        let mut out = Nodes::new(cx)?;
        let names = self.root.children().filter(|c| c.local() == "namedItem");
        out.push(
            cx.int(
                self.root
                    .children()
                    .filter(|c| c.local() == "namedItem")
                    .count(),
            )?,
        )?;
        let mut seen = BudgetedVec::new(cx.budget, Category::Metadata)?;
        for n in names {
            shape(
                n,
                &["name", "area", "drawingID"],
                &[],
                &[(Some(XSI), "type")],
            )?;
            let name = text(n, "name", "")?;
            if name.is_empty() || seen.contains(&name) {
                return Err(BuildError::Invalid("duplicate or empty named item"));
            }
            seen.try_push(name)?;
            out.push(cx.string(name)?)?;
            let (ns, ty) = qname(
                n,
                attr(n, Some(XSI), "type").ok_or(BuildError::Invalid("named item type"))?,
            )?;
            if ns != NS {
                return Err(BuildError::Invalid("named item type namespace"));
            }
            out.push(match ty {
                "NamedItemCells" => {
                    if child(n, "drawingID").is_some() {
                        return Err(BuildError::Invalid("named area drawing"));
                    }
                    cx.list([
                        cx.int(1)?,
                        self.area(Some(
                            child(n, "area").ok_or(BuildError::Invalid("named area"))?,
                        ))?,
                        cx.int(0)?,
                    ])?
                }
                "NamedItemDrawing" => {
                    let id = number(n, "drawingID", -1)?;
                    if child(n, "area").is_some() || !ids.contains(&id) {
                        return Err(BuildError::Invalid("named drawing"));
                    }
                    cx.ints([2, id, 0])?
                }
                _ => return Err(BuildError::Unsupported("named item type")),
            })?;
        }
        out.finish(cx)
    }
}
