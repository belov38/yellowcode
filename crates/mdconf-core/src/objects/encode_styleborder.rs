use crate::model::{Value, XmlElem};
use brace::Node;
use mdschema::UiTables;
const T_BORDER: &str = "4d10ca00-111a-4d43-9c96-92cd773716de";
const T_ENUM: &str = "48312c09-257f-4b29-b280-284dd89efc1e";
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StyleBorderEncodeError {
    WrongValue,
    InvalidShape,
    InvalidPrefix,
    MissingAbsoluteContext,
    UnknownBorder,
    AmbiguousBorder,
    ResourceLimit,
}
use StyleBorderEncodeError::*;
type Result<T> = std::result::Result<T, StyleBorderEncodeError>;
pub(crate) struct AbsoluteBorderPrefixes<'a> {
    pub reference0: &'a Node,
    pub opaque5: &'a Node,
}
pub(crate) struct StyleBorderPrefixes<'a> {
    pub outer0: &'a Node,
    pub outer2: &'a Node,
    pub outer4: Option<&'a Node>,
    pub absolute: Option<AbsoluteBorderPrefixes<'a>>,
}
fn scalar(n: &Node) -> Result<()> {
    if matches!(n, Node::List(_)) {
        Err(InvalidPrefix)
    } else {
        Ok(())
    }
}
fn list(n: &Node) -> Result<&[Node]> {
    n.as_list().ok_or(InvalidShape)
}
fn at(n: &[Node], k: usize) -> Result<&Node> {
    n.get(k).ok_or(InvalidShape)
}
fn nodes<const N: usize>(a: [Node; N]) -> Result<Node> {
    let mut v = Vec::new();
    v.try_reserve_exact(N).map_err(|_| ResourceLimit)?;
    v.extend(a);
    Ok(Node::list(v))
}
impl<'a> StyleBorderPrefixes<'a> {
    pub(crate) fn from_raw(raw: &'a Node, absolute: bool) -> Result<Self> {
        let l = list(raw)?;
        let outer0 = at(l, 0)?;
        let outer2 = at(l, 2)?;
        let absolute = if absolute {
            let b = list(at(l, 3)?)?;
            Some(AbsoluteBorderPrefixes {
                reference0: at(list(at(b, 2)?)?, 0)?,
                opaque5: at(b, 5)?,
            })
        } else {
            None
        };
        let p = Self {
            outer0,
            outer2,
            outer4: l.get(4),
            absolute,
        };
        p.validate(p.absolute.is_some())?;
        Ok(p)
    }
    fn validate(&self, absolute: bool) -> Result<()> {
        scalar(self.outer0)?;
        scalar(self.outer2)?;
        if let Some(n) = self.outer4 {
            scalar(n)?
        }
        if absolute {
            let p = self.absolute.as_ref().ok_or(MissingAbsoluteContext)?;
            scalar(p.reference0)?;
            scalar(p.opaque5)?;
        }
        Ok(())
    }
}
fn lookup<'a>(entries: impl Iterator<Item = (&'a str, &'a str)>, wanted: &str) -> Result<i64> {
    let mut found = None;
    for (k, v) in entries {
        if v != wanted {
            continue;
        }
        let Ok(n) = k.parse::<i64>() else { continue };
        if n.to_string() != k {
            continue;
        }
        if found.is_some_and(|previous| previous != n) {
            return Err(AmbiguousBorder);
        }
        found = Some(n)
    }
    found.ok_or(UnknownBorder)
}
enum Form<'a> {
    Reference(&'a str),
    Absolute { width: &'a str, style: &'a str },
}
fn form<'a>(root: &'a XmlElem, tag: &str) -> Result<Form<'a>> {
    let [(typekey, typevalue), (key, value)] = root.attrs.as_slice() else {
        return Err(InvalidShape);
    };
    if root.name != tag
        || root.expanded
        || root.text.is_some()
        || typekey != "xsi:type"
        || typevalue != "v8ui:Border"
    {
        return Err(InvalidShape);
    }
    match key.as_str() {
        "ref" if root.children.is_empty() => Ok(Form::Reference(
            value.strip_prefix("style:").ok_or(InvalidShape)?,
        )),
        "width" => {
            let [style] = root.children.as_slice() else {
                return Err(InvalidShape);
            };
            let [(k, v)] = style.attrs.as_slice() else {
                return Err(InvalidShape);
            };
            if style.name != "v8ui:style"
                || style.expanded
                || !style.children.is_empty()
                || k != "xsi:type"
                || v != "v8ui:ControlBorderType"
            {
                return Err(InvalidShape);
            }
            Ok(Form::Absolute {
                width: value,
                style: style.text.as_deref().ok_or(InvalidShape)?,
            })
        }
        _ => Err(InvalidShape),
    }
}
pub(crate) fn encode_styleborder(
    tables: &UiTables,
    tag: &str,
    value: &Value,
    prefixes: &StyleBorderPrefixes<'_>,
) -> Result<Node> {
    let Value::Xml(root) = value else {
        return Err(WrongValue);
    };
    let f = form(root, tag)?;
    prefixes.validate(matches!(f, Form::Absolute { .. }))?;
    let payload = match f {
        Form::Reference(name) => {
            let key = lookup(
                tables
                    .style_borders
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str())),
                name,
            )?;
            nodes([Node::int(3), Node::int(1), nodes([Node::int(key)])?])?
        }
        Form::Absolute { width, style } => {
            let key = lookup(
                tables
                    .border_styles
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str())),
                style,
            )?;
            let p = prefixes.absolute.as_ref().ok_or(MissingAbsoluteContext)?;
            nodes([
                Node::int(3),
                Node::int(0),
                nodes([p.reference0.clone()])?,
                Node::int(key),
                Node::atom(width),
                p.opaque5.clone(),
                Node::atom(T_ENUM),
            ])?
        }
    };
    let mut out = Vec::new();
    out.try_reserve_exact(5).map_err(|_| ResourceLimit)?;
    out.extend([
        prefixes.outer0.clone(),
        Node::atom(T_BORDER),
        prefixes.outer2.clone(),
        payload,
    ]);
    out.extend(prefixes.outer4.cloned());
    Ok(Node::list(out))
}
