use super::super::mxltables::{self as t, Codec};
use super::{
    BuildError, Encoder, R,
    nodes::{N, Nodes},
    tables, values,
    xml::*,
};
impl Encoder<'_, '_, '_> {
    pub(super) fn refer(&mut self, value: usize) -> R<()> {
        self.format_context(value, false)?;
        self.refer_key((value, false))
    }
    fn format_context(&self, index: usize, drawing: bool) -> R<()> {
        if index == 0 {
            return Ok(());
        }
        let format = self
            .formats
            .get(index - 1)
            .ok_or(BuildError::Invalid("format index"))?;
        for property in format.children() {
            let name = property.local();
            if drawing
                && [
                    "border",
                    "leftBorder",
                    "topBorder",
                    "rightBorder",
                    "bottomBorder",
                ]
                .contains(&name)
            {
                return Err(BuildError::Unsupported(
                    "cell format property used by drawing",
                ));
            }
            if !drawing
                && (name == "print" || name == "drawingBorder" || name.starts_with("drawingHave"))
            {
                return Err(BuildError::Unsupported(
                    "drawing format property used by cell",
                ));
            }
        }
        Ok(())
    }
    fn refer_key(&mut self, key: (usize, bool)) -> R<()> {
        if key == (0, false) {
            return Ok(());
        }
        if key.0 > self.formats.len() {
            return Err(BuildError::Invalid("format index"));
        }
        if !self.order.contains(&key) {
            self.order.try_push(key)?;
        }
        Ok(())
    }
    pub(super) fn index(&self, value: i64) -> R<usize> {
        self.key_index((
            usize::try_from(value).map_err(|_| BuildError::Invalid("format index"))?,
            false,
        ))
    }
    fn key_index(&self, key: (usize, bool)) -> R<usize> {
        if key == (0, false) {
            return Ok(0);
        }
        self.order
            .iter()
            .position(|v| *v == key)
            .map(|v| v + 1)
            .ok_or(BuildError::Invalid("unregistered format index"))
    }
    fn cell_key(&self, e: X<'_, '_>) -> R<(usize, bool)> {
        let idx = usize::try_from(number(e, "f", number(e, "formatIndex", 0)?)?)
            .map_err(|_| BuildError::Invalid("format index"))?;
        if idx > self.formats.len() {
            return Err(BuildError::Invalid("format index"));
        }
        let parameter = child(e, "parameter").is_some();
        let fill = if idx == 0 {
            None
        } else {
            child(
                *self
                    .formats
                    .get(idx - 1)
                    .ok_or(BuildError::Invalid("format index"))?,
                "fillType",
            )
            .map(leaf)
            .transpose()?
        };
        if parameter && fill.is_some_and(|s| s != "Parameter") {
            return Err(BuildError::Invalid("parameter fill type"));
        }
        if !parameter && fill == Some("Parameter") {
            for tag in ["tl", "text", "tfl"] {
                if child(e, tag).is_some() {
                    return Err(BuildError::Invalid(
                        "parameter format requires parameter payload",
                    ));
                }
            }
        }
        Ok((idx, parameter && fill.is_none()))
    }
    pub(super) fn cell_index(&self, e: X<'_, '_>) -> R<usize> {
        self.key_index(self.cell_key(e)?)
    }
    fn cell_ref(&mut self, e: X<'_, '_>, drawing: bool) -> R<()> {
        let key = self.cell_key(e)?;
        self.format_context(key.0, drawing)?;
        self.refer_key(key)?;
        if let Some(note) = child(e, "note") {
            let key = self.cell_key(note)?;
            self.format_context(key.0, true)?;
            self.refer_key(key)?;
        }
        Ok(())
    }
    pub(super) fn prepare(&mut self) -> R<N> {
        for slot in super::HEADERS {
            if let Some(e) = child(self.root, slot) {
                self.cell_ref(e, false)?;
            }
        }
        for ri in self.root.children().filter(|n| n.local() == "rowsItem") {
            let row = child(ri, "row").ok_or(BuildError::Invalid("missing row"))?;
            self.refer(
                usize::try_from(number(row, "formatIndex", 0)?)
                    .map_err(|_| BuildError::Invalid("row format index"))?,
            )?;
            for c in row.children().filter(|n| n.local() == "c") {
                self.cell_ref(
                    child(c, "c").ok_or(BuildError::Invalid("missing cell"))?,
                    false,
                )?;
            }
        }
        for cs in self.root.children().filter(|n| n.local() == "columns") {
            self.refer(
                usize::try_from(number(cs, "formatIndex", 0)?)
                    .map_err(|_| BuildError::Invalid("column format index"))?,
            )?;
            for ci in cs.children().filter(|n| n.local() == "columnsItem") {
                self.refer(
                    usize::try_from(number(
                        child(ci, "column").ok_or(BuildError::Invalid("missing column"))?,
                        "formatIndex",
                        0,
                    )?)
                    .map_err(|_| BuildError::Invalid("column format index"))?,
                )?;
            }
        }
        for e in self.root.children().filter(|n| n.local() == "drawing") {
            self.cell_ref(e, true)?;
        }
        let default = usize::try_from(number(self.root, "defaultFormatIndex", 0)?)
            .map_err(|_| BuildError::Invalid("default format index"))?;
        if default > self.formats.len() {
            return Err(BuildError::Invalid("default format index"));
        }
        self.format_context(default, false)?;
        for (idx, _) in std::iter::once((default, false)).chain(self.order.iter().copied()) {
            if idx == 0 {
                continue;
            }
            let e = *self
                .formats
                .get(idx - 1)
                .ok_or(BuildError::Invalid("format index"))?;
            for c in e.children() {
                let map = match c.local() {
                    "font" => &mut self.font_order,
                    "border" | "leftBorder" | "rightBorder" | "topBorder" | "bottomBorder"
                    | "drawingBorder" => &mut self.line_order,
                    _ => continue,
                };
                let value = usize::try_from(integer(leaf(c)?)?)
                    .map_err(|_| BuildError::Invalid("shared table index"))?;
                if !map.contains(&value) {
                    map.try_push(value)?;
                }
            }
        }
        let default = if default == 0 {
            self.cx.ints([0])?
        } else {
            self.format(
                Some(
                    *self
                        .formats
                        .get(default - 1)
                        .ok_or(BuildError::Invalid("default format index"))?,
                ),
                false,
            )?
        };
        for (tag, dflt) in [
            ("groupsBackColor", -1),
            ("groupsColor", -3),
            ("headersBackColor", -1),
            ("headersColor", -3),
        ] {
            let color = tables::color(self.cx, self.lookup, child(self.root, tag), dflt)?;
            let index = self.colors.add(color, self.cx)?;
            self.group_colors.try_push(index)?;
        }
        for i in 0..self.order.len() {
            let (idx, infer) = *self
                .order
                .get(i)
                .ok_or(BuildError::Invalid("format index"))?;
            let e = if idx == 0 {
                None
            } else {
                Some(
                    *self
                        .formats
                        .get(idx - 1)
                        .ok_or(BuildError::Invalid("format index"))?,
                )
            };
            let record = self.format(e, infer)?;
            self.records.try_push(record)?;
        }
        Ok(default)
    }
    fn format(&mut self, e: Option<X<'_, '_>>, infer: bool) -> R<N> {
        let Some(e) = e else {
            return self.cx.ints([32768, 1]);
        };
        if e.attrs().next().is_some() || e.text().is_some_and(|s| !s.trim().is_empty()) {
            return Err(BuildError::Invalid("format shape"));
        }
        let mut fields: [Option<N>; 48] = std::array::from_fn(|_| None);
        let mut set = |bit: usize, value: N| -> R<()> {
            let slot = fields
                .get_mut(bit)
                .ok_or(BuildError::Invalid("format bit"))?;
            if slot.is_some() {
                return Err(BuildError::Invalid("duplicate format bit"));
            }
            *slot = Some(value);
            Ok(())
        };
        let mut drawing_mask = 0i64;
        let mut drawing_sides = 0u8;
        for c in e.children() {
            self.cx.check()?;
            if c.ns() != Some(NS) {
                return Err(BuildError::Invalid("format namespace"));
            }
            let tag = c.local();
            if tag.starts_with("drawingHave") {
                let side = match tag {
                    "drawingHaveLeftBorder" => 0,
                    "drawingHaveTopBorder" => 1,
                    "drawingHaveRightBorder" => 2,
                    "drawingHaveBottomBorder" => 3,
                    _ => return Err(BuildError::Unsupported("drawing border")),
                };
                if drawing_sides & (1 << side) != 0 {
                    return Err(BuildError::Invalid("duplicate drawing border"));
                }
                drawing_sides |= 1 << side;
                if boolean(leaf(c)?)? {
                    drawing_mask |= 1 << side;
                }
                continue;
            }
            if let Some(bits) = match tag {
                "border" => Some(&[1, 2, 3, 4][..]),
                "leftBorder" | "drawingBorder" => Some(&[1][..]),
                "topBorder" => Some(&[2][..]),
                "rightBorder" => Some(&[3][..]),
                "bottomBorder" => Some(&[4][..]),
                _ => None,
            } {
                let index = usize::try_from(integer(leaf(c)?)?)
                    .map_err(|_| BuildError::Invalid("line index"))?;
                let index = self
                    .line_order
                    .iter()
                    .position(|i| *i == index)
                    .ok_or(BuildError::Invalid("line index"))?;
                for bit in bits {
                    set(*bit, self.cx.int(index)?)?;
                }
                continue;
            }
            if tag == "print" {
                set(4, self.cx.int(i64::from(!boolean(leaf(c)?)?))?)?;
                continue;
            }
            let (bit, _, codec) = t::FORMAT_BITS
                .iter()
                .find(|(_, name, _)| *name == tag)
                .ok_or(BuildError::Unsupported("format element"))?;
            let value = match codec {
                Codec::Font => {
                    let idx = usize::try_from(integer(leaf(c)?)?)
                        .map_err(|_| BuildError::Invalid("font index"))?;
                    if idx >= self.fonts.len() {
                        return Err(BuildError::Invalid("font index"));
                    }
                    self.cx.int(
                        self.font_order
                            .iter()
                            .position(|i| *i == idx)
                            .ok_or(BuildError::Invalid("font index"))?,
                    )?
                }
                Codec::Color => {
                    let record = tables::color(self.cx, self.lookup, Some(c), 0)?;
                    self.cx.int(self.colors.add(record, self.cx)?)?
                }
                Codec::Int => {
                    let value = integer(leaf(c)?)?;
                    if tag == "picIndex"
                        && usize::try_from(value).ok().is_none_or(|i| {
                            i > self
                                .root
                                .children()
                                .filter(|c| c.local() == "picture")
                                .count()
                        })
                    {
                        return Err(BuildError::Invalid("format picture index"));
                    }
                    self.cx.int(value)?
                }
                Codec::Bool | Codec::NBool => self.cx.int(i64::from(
                    boolean(leaf(c)?)? ^ matches!(codec, Codec::NBool),
                ))?,
                Codec::FStr | Codec::Mask => {
                    let record = values::lstr(Some(c), self.cx)?;
                    let table = if matches!(codec, Codec::Mask) {
                        &mut self.masks
                    } else {
                        &mut self.strings
                    };
                    self.cx.int(table.add(record, self.cx)?)?
                }
                Codec::VType => {
                    let record = values::type_desc(c, self.cx, self.lookup)?;
                    self.cx.int(self.types.add(record, self.cx)?)?
                }
                Codec::Uuid => {
                    let id = brace::Guid::parse(leaf(c)?)
                        .ok_or(BuildError::Invalid("control type GUID"))?;
                    self.cx.int(self.uuids.add(self.cx.int(id)?, self.cx)?)?
                }
                Codec::Map(_) | Codec::Pattern => {
                    let value = leaf(c)?;
                    let key = (0..=255)
                        .find(|key| {
                            let mut buf = crate::graph::scalar::Text::<8>::new();
                            use std::fmt::Write;
                            let _ = write!(&mut buf, "{key}");
                            t::map_value_ref(*codec, buf.as_str()) == Some(value)
                        })
                        .ok_or(BuildError::Invalid("format enum"))?;
                    self.cx.int(key)?
                }
            };
            set(*bit as usize, value)?;
        }
        if infer {
            set(15, self.cx.int(1)?)?;
        }
        if drawing_sides != 0 {
            set(3, self.cx.int(drawing_mask)?)?;
        }
        let mask = fields
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_some())
            .fold(0u64, |mask, (bit, _)| mask | (1u64 << bit));
        let mut out = Nodes::new(self.cx)?;
        out.push(self.cx.int(mask)?)?;
        for value in fields.into_iter().flatten() {
            out.push(value)?;
        }
        out.finish(self.cx)
    }
}
