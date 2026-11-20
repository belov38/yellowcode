use super::{
    BuildError, Encoder, R,
    nodes::{Cx, N, Nodes},
    values,
    xml::*,
};
use base64::Engine as _;
use memory_budget::{BudgetedVec, Category};
const FIELDS: &[&str] = &[
    "f",
    "formatIndex",
    "control",
    "v",
    "value",
    "d",
    "r",
    "detailParameter",
    "pictureParameter",
    "tl",
    "text",
    "tfl",
    "parameter",
    "note",
];
impl Encoder<'_, '_, '_> {
    pub(super) fn cell(&self, e: Option<X<'_, '_>>) -> R<N> {
        let cx = self.cx;
        let Some(e) = e else {
            return cx.ints([0, 0]);
        };
        let drawing = e.local() == "drawing" || e.local() == "note";
        if !drawing {
            shape(e, FIELDS, &[], &[])?;
        }
        let mut mask = 0i64;
        let mut out = Nodes::new(cx)?;
        if child(e, "f").is_some() && child(e, "formatIndex").is_some() {
            return Err(BuildError::Invalid("duplicate cell format"));
        }
        let fmt = self.cell_index(e)?;
        for (tag, bit) in [
            ("control", 1),
            ("v", 2),
            ("value", 2),
            ("d", 4),
            ("r", 4),
            ("detailParameter", 8),
            ("pictureParameter", 64),
            ("tl", 16),
            ("text", 16),
            ("tfl", 16),
            ("parameter", 16),
            ("note", 32),
        ] {
            if child(e, tag).is_some() {
                if mask & bit != 0 {
                    return Err(BuildError::Invalid("duplicate cell payload"));
                }
                mask |= bit;
            }
        }
        out.push(cx.int(mask)?)?;
        out.push(cx.int(fmt)?)?;
        if let Some(ctl) = child(e, "control") {
            out.push(cx.int(1)?)?;
            out.push(cx.list([control(ctl, cx)?])?)?;
        }
        if let Some(v) = child(e, "v").or_else(|| child(e, "value")) {
            out.push(values::typed(v, cx)?)?;
        }
        if let Some(v) = child(e, "d") {
            out.push(values::typed(v, cx)?)?;
        }
        if let Some(v) = child(e, "r") {
            out.push(cx.list([
                cx.string("#")?,
                cx.atom("3031edd8-c3df-47b2-98ca-47f628d4ec18")?,
                cx.list([cx.int(integer(leaf(v)?)?)?])?,
            ])?)?;
        }
        for tag in ["detailParameter", "pictureParameter"] {
            if let Some(v) = child(e, tag) {
                out.push(cx.string(leaf(v)?)?)?;
            }
        }
        let formatted = child(e, "tfl");
        if let Some(v) = formatted {
            out.push(plain_lstr(v, cx)?)?;
        } else if let Some(v) = child(e, "tl").or_else(|| child(e, "text")) {
            out.push(values::lstr(Some(v), cx)?)?;
        } else if let Some(v) = child(e, "parameter") {
            out.push(cx.list([
                cx.int(1)?,
                cx.int(1)?,
                cx.list([cx.string("")?, values::parameter(leaf(v)?, cx)?])?,
            ])?)?;
        }
        if let Some(note) = child(e, "note") {
            if e.local() == "note" {
                return Err(BuildError::Unsupported("nested note"));
            }
            out.push(values::lstr(child(note, "text"), cx)?)?;
            out.push(cx.int(1)?)?;
            out.push(self.drawing(note, None)?)?;
        }
        if mask & 16 != 0 {
            if let Some(v) = formatted {
                out.push(cx.int(1)?)?;
                out.push(cx.list([cx.int(1)?, values::lstr(Some(v), cx)?, cx.int(1)?])?)?;
            } else {
                out.push(cx.int(0)?)?;
            }
        }
        out.finish(cx)
    }
    pub(super) fn drawing(&self, e: X<'_, '_>, zorder: Option<usize>) -> R<N> {
        let cx = self.cx;
        if e.children()
            .any(|c| ["control", "d", "r", "pictureParameter", "note"].contains(&c.local()))
        {
            return Err(BuildError::Unsupported("cell-only drawing payload"));
        }
        let fields = [
            "drawingType",
            "id",
            "beginColumn",
            "beginRow",
            "beginColumnOffset",
            "beginRowOffset",
            "endColumn",
            "endRow",
            "endColumnOffset",
            "endRowOffset",
            "autoSize",
            "pictureIndex",
            "pictureSize",
            "zOrder",
        ];
        if e.children()
            .any(|c| !FIELDS.contains(&c.local()) && !fields.contains(&c.local()))
        {
            return Err(BuildError::Unsupported("drawing object or element"));
        }
        if e.attrs().next().is_some() {
            return Err(BuildError::Unsupported("drawing attribute"));
        }
        for c in e.children() {
            if c.ns() != Some(NS) || e.children().filter(|n| n.local() == c.local()).count() != 1 {
                return Err(BuildError::Invalid("drawing namespace or duplicate field"));
            }
        }
        if let Some(z) = child(e, "zOrder")
            && Some(usize::try_from(integer(leaf(z)?)?).map_err(|_| BuildError::Invalid("zOrder"))?)
                != zorder
        {
            return Err(BuildError::Unsupported("drawing zOrder"));
        }
        let kind = super::super::mxltables::DRAWING_TYPES
            .iter()
            .find(|(_, n)| *n == text(e, "drawingType", "").unwrap_or_default())
            .map(|(k, _)| *k)
            .ok_or(BuildError::Unsupported("drawing type"))?;
        let mut out = Nodes::new(cx)?;
        out.push(self.cell(Some(e))?)?;
        out.push(cx.int(kind)?)?;
        for tag in [
            "beginColumn",
            "beginRow",
            "beginColumnOffset",
            "beginRowOffset",
            "endColumn",
            "endRow",
            "endColumnOffset",
            "endRowOffset",
            "id",
        ] {
            out.push(cx.int(number(e, tag, 0)?)?)?;
        }
        if kind == 5 {
            let index = number(e, "pictureIndex", 0)?;
            let n = self
                .root
                .children()
                .filter(|c| c.local() == "picture")
                .count();
            if usize::try_from(index).ok().is_none_or(|i| i > n) {
                return Err(BuildError::Invalid("drawing picture index"));
            }
            out.push(cx.int(index)?)?;
            out.push(
                cx.int(
                    super::super::mxltables::PICTURE_SIZE
                        .iter()
                        .position(|n| Some(*n) == text(e, "pictureSize", "Stretch").ok())
                        .ok_or(BuildError::Invalid("picture size"))?,
                )?,
            )?;
        } else if child(e, "pictureIndex").is_some()
            || text(e, "pictureSize", "Stretch")? != "Stretch"
        {
            return Err(BuildError::Unsupported("non-picture sizing"));
        }
        out.push(cx.int(i64::from(flag(e, "autoSize", false)?))?)?;
        out.finish(cx)
    }
    pub(super) fn picture(&self, e: X<'_, '_>) -> R<N> {
        let cx = self.cx;
        shape(
            e,
            &[],
            &[],
            &["ref", "t", "tx", "ty", "src"].map(|n| (None, n)),
        )?;
        let reference = attr(e, None, "ref");
        let src = attr(e, None, "src");
        let body = e.text().unwrap_or_default().trim();
        if usize::from(reference.is_some())
            + usize::from(src.is_some())
            + usize::from(!body.is_empty())
            > 1
        {
            return Err(BuildError::Invalid("ambiguous picture payload"));
        }
        let (kind, rf, blob) = if let Some(reference) = reference {
            (
                1,
                super::tables::picture_ref(cx, self.lookup, e, reference)?,
                None,
            )
        } else if let Some(path) = src {
            validate_path(path)?;
            let mut found = None;
            for asset in self.assets {
                if super::paths::equal(asset.path, path, false, cx)? {
                    if found.is_some() {
                        return Err(BuildError::Invalid("duplicate picture asset"));
                    }
                    found = Some(asset);
                }
            }
            let asset = found.ok_or(BuildError::Invalid("missing picture asset"))?;
            (3, cx.ints([0])?, Some(encode_b64(asset.bytes, cx)?))
        } else if !body.is_empty() {
            (3, cx.ints([0])?, Some(b64(body, cx)?))
        } else {
            (0, cx.ints([0])?, None)
        };
        if kind == 0
            && (attr(e, None, "tx").is_some()
                || attr(e, None, "ty").is_some()
                || attr(e, None, "t").is_some())
        {
            return Err(BuildError::Unsupported("empty picture transparency"));
        }
        let mut out = Nodes::new(cx)?;
        out.push(cx.int(4)?)?;
        out.push(cx.int(kind)?)?;
        out.push(rf)?;
        out.push(cx.string("")?)?;
        for key in ["tx", "ty"] {
            out.push(cx.int(integer(attr(e, None, key).unwrap_or("-1"))?)?)?;
        }
        out.push(cx.int(i64::from(boolean(attr(e, None, "t").unwrap_or("true"))?))?)?;
        if let Some(blob) = blob {
            out.push(cx.list([cx.list([blob])?])?)?;
        }
        out.push(cx.int(0)?)?;
        out.push(cx.string("")?)?;
        out.finish(cx)
    }
}
pub(super) fn validate_path(path: &str) -> R<()> {
    if path.is_empty()
        || path.contains(['\\', ':', '\0'])
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err(BuildError::Invalid("unsafe asset path"));
    }
    Ok(())
}
fn b64(text: &str, cx: Cx<'_>) -> R<N> {
    let mut compact = BudgetedVec::try_with_capacity(cx.budget, Category::Metadata, text.len())?;
    for b in text.bytes().filter(|b| !b.is_ascii_whitespace()) {
        compact.try_push(b)?;
    }
    let len = base64::decoded_len_estimate(compact.len());
    let mut decoded = BudgetedVec::try_with_capacity(cx.budget, Category::Metadata, len)?;
    decoded.try_resize(len, 0)?;
    base64::engine::general_purpose::STANDARD
        .decode_slice(&*compact, &mut decoded)
        .map_err(|_| BuildError::Invalid("base64"))?;
    let a = crate::allocation::Allocation::Managed(cx.budget);
    let lf = a.replace_text(text, "\r\n", "\n")?;
    let native = a.replace_text(&lf, "\n", "\r\r\n")?;
    Ok(a.text(&["#base64:", &native])?.map(brace::Node::Atom))
}
fn encode_b64(bytes: &[u8], cx: Cx<'_>) -> R<N> {
    let len = base64::encoded_len(bytes.len(), true).ok_or(BuildError::Invalid("picture size"))?;
    let mut text = BudgetedVec::try_with_capacity(cx.budget, Category::Metadata, len)?;
    text.try_resize(len, 0)?;
    base64::engine::general_purpose::STANDARD
        .encode_slice(bytes, &mut text)
        .map_err(|_| BuildError::Invalid("picture base64"))?;
    let text = std::str::from_utf8(&text).map_err(|_| BuildError::Invalid("picture base64"))?;
    b64(text, cx)
}
fn plain_lstr(e: X<'_, '_>, cx: Cx<'_>) -> R<N> {
    shape(e, &["item"], &["item"], &[])?;
    let mut out = Nodes::new(cx)?;
    out.push(cx.int(1)?)?;
    out.push(cx.int(e.children().count())?)?;
    for it in e.children() {
        cx.check()?;
        shape(it, &["lang", "content"], &[], &[])?;
        let text = text(it, "content", "")?;
        let mut plain = BudgetedVec::try_with_capacity(cx.budget, Category::Metadata, text.len())?;
        let mut copied = 0;
        let mut opening = None;
        for (at, byte) in text.bytes().enumerate() {
            if at % 4096 == 0 {
                cx.check()?;
            }
            match byte {
                b'<' => opening = Some(at),
                b'>' => {
                    if let Some(start) = opening.take() {
                        plain_copy(
                            &mut plain,
                            text.as_bytes()
                                .get(copied..start)
                                .ok_or(BuildError::Invalid("formatted text range"))?,
                            cx,
                        )?;
                        copied = at + 1;
                    }
                }
                _ => (),
            }
        }
        plain_copy(
            &mut plain,
            text.as_bytes()
                .get(copied..)
                .ok_or(BuildError::Invalid("formatted text range"))?,
            cx,
        )?;
        cx.check()?;
        let plain =
            std::str::from_utf8(&plain).map_err(|_| BuildError::Invalid("formatted text"))?;
        cx.check()?;
        out.push(cx.list([cx.string(text_fn(it, "lang")?)?, values::crlf(plain, cx)?])?)?;
        cx.check()?;
    }
    out.finish(cx)
}
fn plain_copy(out: &mut BudgetedVec<u8>, bytes: &[u8], cx: Cx<'_>) -> R<()> {
    for chunk in bytes.chunks(4096) {
        cx.check()?;
        out.try_extend_from_slice(chunk)?;
    }
    Ok(())
}
fn text_fn<'a>(e: X<'a, '_>, tag: &str) -> R<&'a str> {
    text(e, tag, "")
}

fn control(e: X<'_, '_>, cx: Cx<'_>) -> R<N> {
    shape(e, &[], &[], &[(Some(XSI), "type")])?;
    if let Some(ty) = attr(e, Some(XSI), "type")
        && qname(e, ty)? != (XS, "base64Binary")
    {
        return Err(BuildError::Invalid("control type"));
    }
    b64(e.text().unwrap_or_default(), cx)
}
