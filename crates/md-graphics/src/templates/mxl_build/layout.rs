use super::{
    BuildError, Encoder, R, ZERO,
    nodes::{N, Nodes},
    values,
    xml::*,
};
use memory_budget::{BudgetedVec, Category};
impl Encoder<'_, '_, '_> {
    pub(super) fn groups(&self, tag: &str, out: &mut Nodes) -> R<()> {
        let cx = self.cx;
        let groups = self.root.children().filter(|c| c.local() == tag);
        out.push(cx.int(self.root.children().filter(|c| c.local() == tag).count())?)?;
        let mut spans = BudgetedVec::new(cx.budget, Category::Metadata)?;
        for g in self.root.children().filter(|c| c.local() == tag) {
            shape(g, &["b", "e", "t", "o", "g", "columnsID"], &[], &[])?;
            let begin = number(g, "b", -1)?;
            let end = number(g, "e", begin)?;
            let cs = self.colset_index(text(g, "columnsID", "")?)?;
            if begin < 0 || end < begin {
                return Err(BuildError::Invalid("group interval"));
            }
            spans.try_push((begin, end, cs))?;
        }
        let mut levels = 0;
        for (i, g) in groups.enumerate() {
            cx.check()?;
            let (begin, end, cs) = *spans.get(i).ok_or(BuildError::Invalid("group index"))?;
            let level = spans
                .get(..i)
                .ok_or(BuildError::Invalid("group index"))?
                .iter()
                .filter(|(b, e, c)| *b <= begin && end <= *e && *c == cs)
                .count();
            levels = levels.max(level + 1);
            let kind = match text(g, "g", "Auto")? {
                "Auto" => 0,
                "Begin" => 1,
                "End" => 2,
                _ => return Err(BuildError::Invalid("group kind")),
            };
            out.push(cx.list([
                cx.int(begin)?,
                cx.int(end)?,
                cx.int(level)?,
                values::lstr(child(g, "t"), cx)?,
                cx.int(i64::from(!flag(g, "o", true)?))?,
                cx.int(kind)?,
            ])?)?;
            out.push(cx.int(cs)?)?;
        }
        if tag == "vg" && number(self.root, "vgLevels", levels as i64)? != levels as i64 {
            return Err(BuildError::Invalid("group levels"));
        }
        Ok(())
    }
    pub(super) fn merges(&self, section: usize) -> R<N> {
        let cx = self.cx;
        let mut records = BudgetedVec::new(cx.budget, Category::Metadata)?;
        for (tag, kind) in [
            ("merge", 0),
            ("verticalUnmerge", 2),
            ("horizontalUnmerge", 1),
        ] {
            for m in self.root.children().filter(|c| c.local() == tag) {
                shape(m, &["r", "c", "h", "w", "columnsID"], &[], &[])?;
                let row = number(m, "r", 0)?;
                let col = number(m, "c", 0)?;
                let height = number(m, "h", 0)?;
                let width = number(m, "w", 0)?;
                if row < -1 || col < -1 || row == -1 && col == -1 || height < 0 || width < 0 {
                    return Err(BuildError::Invalid("merge bounds"));
                }
                let belongs = if row == -1 {
                    2
                } else if col == -1 {
                    1
                } else {
                    0
                };
                if belongs != section {
                    continue;
                }
                let right = col
                    .checked_add(width)
                    .ok_or(BuildError::Invalid("merge overflow"))?;
                let bottom = row
                    .checked_add(height)
                    .ok_or(BuildError::Invalid("merge overflow"))?;
                let mut rec = Nodes::new(cx)?;
                for v in [col, row, right, bottom, kind] {
                    rec.push(cx.int(v)?)?;
                }
                let cid = text(m, "columnsID", ZERO)?;
                if section == 2 {
                    self.colset_index(cid)?;
                    rec.push(
                        cx.int(
                            brace::Guid::parse(cid)
                                .ok_or(BuildError::Invalid("merge column set GUID"))?,
                        )?,
                    )?;
                } else if cid != ZERO {
                    return Err(BuildError::Unsupported("merge column set"));
                }
                records.try_push((
                    if section == 2 { col } else { row },
                    if section == 0 { col } else { 0 },
                    rec.finish(cx)?,
                ))?;
            }
        }
        records
            .as_mut_slice()
            .sort_unstable_by_key(|(r, c, _)| (*r, *c));
        if records
            .windows(2)
            .any(|p| p.first().map(|x| (x.0, x.1)) == p.get(1).map(|x| (x.0, x.1)))
        {
            return Err(BuildError::Invalid("duplicate merge"));
        }
        let mut out = Nodes::new(cx)?;
        out.push(cx.int(records.len())?)?;
        records.as_mut_slice().reverse();
        while let Some((_, _, n)) = records.pop() {
            out.push(n)?;
        }
        out.finish(cx)
    }
    pub(super) fn view(&self) -> R<N> {
        use super::super::mxltables::{VIEW_FIELDS, ViewKind, view_default_ref};
        let cx = self.cx;
        let e = child(self.root, "viewSettings");
        if let Some(e) = e
            && (e.attrs().next().is_some()
                || e.children().any(|c| {
                    c.ns() != Some(NS)
                        || !VIEW_FIELDS.iter().any(|(_, n, _)| *n == c.local())
                        || e.children().filter(|n| n.local() == c.local()).count() != 1
                }))
        {
            return Err(BuildError::Unsupported("view settings"));
        }
        let mut out = Nodes::new(cx)?;
        for (index, dflt) in view_default_ref().iter().enumerate() {
            let value = VIEW_FIELDS
                .iter()
                .find(|(i, _, _)| *i == index)
                .and_then(|(_, tag, kind)| e.and_then(|e| child(e, tag)).map(|e| (e, kind)));
            out.push(if let Some((e, kind)) = value {
                match kind {
                    ViewKind::S => cx.string(leaf(e)?)?,
                    ViewKind::B => cx.int(i64::from(boolean(leaf(e)?)?))?,
                    ViewKind::N => cx.int(integer(leaf(e)?)?)?,
                }
            } else if index == 21 {
                cx.string(dflt)?
            } else {
                cx.atom(dflt)?
            })?;
        }
        out.finish(cx)
    }
    pub(super) fn print_settings(&self) -> R<N> {
        use super::super::mxltables::{PRINT_KEYS, PrintCodec};
        let cx = self.cx;
        let e = child(self.root, "printSettings");
        let mut bag: [Option<N>; 22] = std::array::from_fn(|_| None);
        for slot in bag
            .get_mut(6..12)
            .ok_or(BuildError::Invalid("print margins"))?
        {
            *slot = Some(cx.list([cx.string("N")?, cx.int(1000)?])?);
        }
        if let Some(e) = e {
            if e.attrs().next().is_some() || e.text().is_some_and(|s| !s.trim().is_empty()) {
                return Err(BuildError::Invalid("print settings shape"));
            }
            let mut seen = 0u32;
            for c in e.children() {
                if c.ns() != Some(NS) {
                    return Err(BuildError::Invalid("print setting namespace"));
                }
                let value = leaf(c)?;
                let (key, rec) = match c.local() {
                    "paper" => (0, cx.list([cx.string("N")?, cx.int(integer(value)?)?])?),
                    "pageSize" => (0, cx.list([cx.string("S")?, cx.string(value)?])?),
                    "printerName" => (14, cx.list([cx.string("S")?, cx.string(value)?])?),
                    tag => {
                        let (key, _, codec) = PRINT_KEYS
                            .iter()
                            .find(|(_, name, _)| *name == tag)
                            .ok_or(BuildError::Unsupported("print setting"))?;
                        let v = match codec {
                            PrintCodec::N => integer(value)?,
                            PrintCodec::B => i64::from(boolean(value)?),
                            PrintCodec::Orientation => match value {
                                "Portrait" => 1,
                                "Landscape" => 2,
                                _ => return Err(BuildError::Invalid("page orientation")),
                            },
                            PrintCodec::Map(items) => integer(
                                items
                                    .iter()
                                    .find(|(_, v)| *v == value)
                                    .map(|(k, _)| *k)
                                    .ok_or(BuildError::Invalid("print setting enum"))?,
                            )?,
                            PrintCodec::Skip => {
                                return Err(BuildError::Unsupported("print setting"));
                            }
                        };
                        (*key, cx.list([cx.string("N")?, cx.int(v)?])?)
                    }
                };
                if seen & (1 << key) != 0 {
                    return Err(BuildError::Invalid("duplicate print setting"));
                }
                seen |= 1 << key;
                *bag.get_mut(key)
                    .ok_or(BuildError::Invalid("print setting key"))? = Some(rec);
            }
        }
        let mut out = Nodes::new(cx)?;
        out.push(cx.int(0)?)?;
        out.push(cx.int(bag.iter().flatten().count())?)?;
        for (i, n) in bag.into_iter().enumerate() {
            if let Some(n) = n {
                out.push(cx.int(i)?)?;
                out.push(n)?;
            }
        }
        out.finish(cx)
    }
}
