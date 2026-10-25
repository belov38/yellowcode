use crate::model::Value;
use brace::Node;
use mdschema::UiTables;
const T_FONT: &str = "9cd510c8-abfc-11d4-9434-004095e12fc7";
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StyleFontEncodeError {
    WrongValue,
    InvalidShape,
    InvalidPrefix,
    MissingContext,
    UnknownFont,
    AmbiguousFont,
    InvalidHeight,
    ResourceLimit,
}
use StyleFontEncodeError::*;
type Result<T> = std::result::Result<T, StyleFontEncodeError>;
pub(crate) struct StyleFontPrefixes<'a> {
    pub outer0: &'a Node,
    pub outer2: &'a Node,
    pub outer4: Option<&'a Node>,
    pub tail0: &'a Node,
    pub unobserved_scale: Option<&'a Node>,
}
struct Form<'a> {
    kind: i64,
    reference: Option<&'a str>,
    face: Option<&'a str>,
    height: Option<i64>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strikeout: Option<bool>,
    scale: Option<&'a str>,
    line: Option<&'a str>,
}
pub(super) fn height_tenths(text: &str) -> Result<i64> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = match digits.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (digits, None),
    };
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || (whole.len() > 1 && whole.starts_with('0'))
    {
        return Err(InvalidHeight);
    }
    let n = if let Some(f) = fraction {
        if f.len() != 1 || !f.bytes().all(|b| b.is_ascii_digit()) {
            return Err(InvalidHeight);
        }
        text.replace('.', "")
            .parse::<i64>()
            .map_err(|_| InvalidHeight)?
    } else {
        text.parse::<i64>()
            .map_err(|_| InvalidHeight)?
            .checked_mul(10)
            .ok_or(InvalidHeight)?
    };
    if super::ui::height(&Node::int(n)).as_deref() != Some(text) {
        return Err(InvalidHeight);
    }
    Ok(n)
}
fn take<'a>(attrs: &[(&'a str, &'a str)], i: &mut usize, name: &str) -> Option<&'a str> {
    let &(k, v) = attrs.get(*i)?;
    if k != name {
        return None;
    }
    *i += 1;
    Some(v)
}
fn boolean(v: Option<&str>) -> Result<Option<bool>> {
    v.map(|s| match s {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(InvalidShape),
    })
    .transpose()
}
fn form<'a>(tag: &str, value: &'a Value) -> Result<Form<'a>> {
    let Value::Xml(root) = value else {
        return Err(WrongValue);
    };
    if root.name != tag || root.expanded || root.text.is_some() || !root.children.is_empty() {
        return Err(InvalidShape);
    }
    let mut a = [("", ""); 11];
    if root.attrs.len() > a.len() {
        return Err(InvalidShape);
    }
    for (slot, (k, v)) in a.iter_mut().zip(&root.attrs) {
        *slot = (k, v)
    }
    let attrs = a.get(..root.attrs.len()).ok_or(InvalidShape)?;
    let mut i = 0;
    if take(attrs, &mut i, "xsi:type") != Some("v8ui:Font") {
        return Err(InvalidShape);
    }
    let reference = take(attrs, &mut i, "ref");
    let face = take(attrs, &mut i, "faceName");
    let height = take(attrs, &mut i, "height")
        .map(height_tenths)
        .transpose()?;
    let bold = boolean(take(attrs, &mut i, "bold"))?;
    let italic = boolean(take(attrs, &mut i, "italic"))?;
    let underline = boolean(take(attrs, &mut i, "underline"))?;
    let strikeout = boolean(take(attrs, &mut i, "strikeout"))?;
    let kind = match take(attrs, &mut i, "kind") {
        Some("WindowsFont") => 1,
        Some("StyleItem") => 2,
        Some("AutoFont") => 3,
        _ => return Err(InvalidShape),
    };
    let reference = match (kind, reference) {
        (1, Some(r)) => Some(r.strip_prefix("sys:").ok_or(InvalidShape)?),
        (2, Some(r)) => Some(r.strip_prefix("style:").ok_or(InvalidShape)?),
        (3, None) => None,
        _ => return Err(InvalidShape),
    };
    let scale = take(attrs, &mut i, "scale");
    let line = take(attrs, &mut i, "lineHeight");
    if i != attrs.len() {
        return Err(InvalidShape);
    }
    Ok(Form {
        kind,
        reference,
        face,
        height,
        bold,
        italic,
        underline,
        strikeout,
        scale,
        line,
    })
}
impl Form<'_> {
    fn value_count(&self) -> usize {
        [
            self.face.is_some(),
            self.height.is_some(),
            self.bold.is_some(),
            self.italic.is_some(),
            self.underline.is_some(),
            self.strikeout.is_some(),
        ]
        .into_iter()
        .filter(|v| *v)
        .count()
    }
}
fn scalar(n: &Node) -> Result<()> {
    if matches!(n, Node::List(_)) {
        Err(InvalidPrefix)
    } else {
        Ok(())
    }
}
impl<'a> StyleFontPrefixes<'a> {
    pub(crate) fn from_raw(raw: &'a Node, tag: &str, value: &Value) -> Result<Self> {
        let f = form(tag, value)?;
        let l = raw.as_list().ok_or(MissingContext)?;
        let b = l.get(3).and_then(Node::as_list).ok_or(MissingContext)?;
        let offset = 3 + usize::from(f.reference.is_some()) + f.value_count();
        let p = Self {
            outer0: l.first().ok_or(MissingContext)?,
            outer2: l.get(2).ok_or(MissingContext)?,
            outer4: l.get(4),
            tail0: b.get(offset).ok_or(MissingContext)?,
            unobserved_scale: if f.scale.is_none() {
                Some(b.get(offset + 1).ok_or(MissingContext)?)
            } else {
                None
            },
        };
        p.validate(f.scale.is_none())?;
        Ok(p)
    }
    fn validate(&self, needs_scale: bool) -> Result<()> {
        scalar(self.outer0)?;
        scalar(self.outer2)?;
        if let Some(n) = self.outer4 {
            scalar(n)?
        }
        scalar(self.tail0)?;
        if needs_scale {
            scalar(self.unobserved_scale.ok_or(MissingContext)?)?
        }
        Ok(())
    }
}
fn lookup<'a>(entries: impl Iterator<Item = (&'a str, &'a str)>, name: &str) -> Result<i64> {
    let mut found = None;
    for (key, value) in entries {
        if value != name {
            continue;
        }
        let Ok(n) = key.parse::<i64>() else { continue };
        if n.to_string() != key {
            continue;
        }
        if found.is_some_and(|old| old != n) {
            return Err(AmbiguousFont);
        }
        found = Some(n)
    }
    found.ok_or(UnknownFont)
}
fn vector(size: usize) -> Result<Vec<Node>> {
    let mut out = Vec::new();
    out.try_reserve_exact(size).map_err(|_| ResourceLimit)?;
    Ok(out)
}
pub(crate) fn encode_stylefont(
    tables: &UiTables,
    tag: &str,
    value: &Value,
    prefixes: &StyleFontPrefixes<'_>,
) -> Result<Node> {
    let f = form(tag, value)?;
    prefixes.validate(f.scale.is_none())?;
    let reference = if let Some(name) = f.reference {
        let t = if f.kind == 1 {
            &tables.windows_fonts
        } else {
            &tables.style_fonts
        };
        Some(lookup(
            t.iter().map(|(k, v)| (k.as_str(), v.as_str())),
            name,
        )?)
    } else {
        None
    };
    let mut mask = 0;
    for (present, bit) in [
        (f.face.is_some(), 1),
        (f.height.is_some(), 2),
        (f.bold.is_some(), 4),
        (f.italic.is_some(), 8),
        (f.underline.is_some(), 16),
        (f.strikeout.is_some(), 32),
        (f.line.is_some(), 256),
        (f.scale.is_some(), 512),
    ] {
        if present {
            mask |= bit
        }
    }
    let mut payload = vector(
        3 + usize::from(reference.is_some()) + f.value_count() + 2 + usize::from(f.line.is_some()),
    )?;
    payload.extend([Node::int(8), Node::int(f.kind), Node::int(mask)]);
    if let Some(n) = reference {
        let mut r = vector(1)?;
        r.push(Node::int(n));
        payload.push(Node::list(r))
    }
    if let Some(n) = f.height {
        payload.push(Node::int(n))
    }
    if let Some(b) = f.bold {
        payload.push(Node::int(if b { 700 } else { 400 }))
    }
    for b in [f.italic, f.underline, f.strikeout].into_iter().flatten() {
        payload.push(Node::int(i64::from(b)))
    }
    if let Some(text) = f.face {
        payload.push(Node::str(text))
    }
    payload.push(prefixes.tail0.clone());
    payload.push(match f.scale {
        Some(text) => Node::atom(text),
        None => prefixes.unobserved_scale.ok_or(MissingContext)?.clone(),
    });
    if let Some(text) = f.line {
        payload.push(Node::atom(text))
    }
    let mut out = vector(5)?;
    out.extend([
        prefixes.outer0.clone(),
        Node::atom(T_FONT),
        prefixes.outer2.clone(),
        Node::list(payload),
    ]);
    out.extend(prefixes.outer4.cloned());
    Ok(Node::list(out))
}
