use super::encode_stylefont::{StyleFontEncodeError, height_tenths};
use crate::model::Value;
use StyleFontEncodeError::*;
use brace::Node;
type Result<T> = std::result::Result<T, StyleFontEncodeError>;
const T_FONT: &str = "9cd510c8-abfc-11d4-9434-004095e12fc7";
const POSITIONS: [usize; 9] = [4, 5, 6, 11, 12, 13, 14, 15, 17];
pub(crate) struct AbsoluteFontPrefixes<'a> {
    pub outer0: &'a Node,
    pub outer2: &'a Node,
    pub opaque: [&'a Node; 9],
    pub outer4: Option<&'a Node>,
    pub tail19: Option<&'a Node>,
}
impl<'a> AbsoluteFontPrefixes<'a> {
    pub(crate) fn from_raw(raw: &'a Node) -> Result<Self> {
        let l = raw.as_list().ok_or(MissingContext)?;
        let b = l.get(3).and_then(Node::as_list).ok_or(MissingContext)?;
        let mut opaque = [l.first().ok_or(MissingContext)?; 9];
        for (slot, pos) in opaque.iter_mut().zip(POSITIONS) {
            *slot = b.get(pos).ok_or(MissingContext)?;
        }
        let p = Self {
            outer0: l.first().ok_or(MissingContext)?,
            outer2: l.get(2).ok_or(MissingContext)?,
            opaque,
            outer4: l.get(4),
            tail19: b.get(19),
        };
        p.validate()?;
        Ok(p)
    }
    fn validate(&self) -> Result<()> {
        for n in [self.outer0, self.outer2]
            .into_iter()
            .chain(self.opaque)
            .chain(self.outer4)
            .chain(self.tail19)
        {
            if matches!(n, Node::List(_)) {
                return Err(InvalidPrefix);
            }
        }
        Ok(())
    }
}
fn vector(size: usize) -> Result<Vec<Node>> {
    let mut out = Vec::new();
    out.try_reserve_exact(size).map_err(|_| ResourceLimit)?;
    Ok(out)
}
pub(crate) fn encode_stylefont_absolute(
    tag: &str,
    value: &Value,
    prefixes: &AbsoluteFontPrefixes<'_>,
) -> Result<Node> {
    let Value::Xml(root) = value else {
        return Err(WrongValue);
    };
    if root.name != tag || root.expanded || root.text.is_some() || !root.children.is_empty() {
        return Err(InvalidShape);
    }
    let names = [
        "xsi:type",
        "faceName",
        "height",
        "bold",
        "italic",
        "underline",
        "strikeout",
        "kind",
        "scale",
    ];
    if root.attrs.len() != names.len()
        || root
            .attrs
            .iter()
            .zip(names)
            .any(|((k, _), expected)| k != expected)
    {
        return Err(InvalidShape);
    }
    let [
        (_, ty),
        (_, face),
        (_, height),
        (_, bold),
        (_, italic),
        (_, underline),
        (_, strikeout),
        (_, kind),
        (_, scale),
    ] = root.attrs.as_slice()
    else {
        return Err(InvalidShape);
    };
    if ty != "v8ui:Font" || kind != "Absolute" {
        return Err(InvalidShape);
    }
    let height = height_tenths(height)?;
    let boolean = |text: &str| match text {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(InvalidShape),
    };
    let bold = boolean(bold)?;
    let italic = boolean(italic)?;
    let underline = boolean(underline)?;
    let strikeout = boolean(strikeout)?;
    prefixes.validate()?;
    let mut payload = vector(20)?;
    let mask = 1 | 2 | 4 | 8 | 16 | 32 | 512;
    payload.extend([
        Node::int(8),
        Node::int(0),
        Node::int(mask),
        Node::int(height),
    ]);
    payload.extend(prefixes.opaque[..3].iter().map(|n| (*n).clone()));
    payload.push(Node::int(if bold { 700 } else { 400 }));
    payload.extend([italic, underline, strikeout].map(|b| Node::int(i64::from(b))));
    payload.extend(prefixes.opaque[3..8].iter().map(|n| (*n).clone()));
    payload.extend([
        Node::str(face),
        prefixes.opaque[8].clone(),
        Node::atom(scale),
    ]);
    payload.extend(prefixes.tail19.cloned());
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
