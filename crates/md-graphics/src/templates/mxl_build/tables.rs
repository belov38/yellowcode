use super::{
    BuildError, InverseLookup, R,
    nodes::{Cx, N, Nodes},
    xml::*,
};
use brace::Node;
use memory_budget::{BudgetedVec, Category};
pub(super) struct Intern {
    pub values: BudgetedVec<N>,
}
impl Intern {
    pub fn new(cx: Cx<'_>) -> R<Self> {
        Ok(Self {
            values: BudgetedVec::new(cx.budget, Category::Metadata)?,
        })
    }
    pub fn add(&mut self, value: N, cx: Cx<'_>) -> R<usize> {
        cx.check()?;
        if let Some(i) = self.values.iter().position(|v| **v == *value) {
            return Ok(i);
        }
        if self.values.len() >= 4096 {
            return Err(BuildError::Invalid("shared table limit"));
        }
        let i = self.values.len();
        self.values.try_push(value)?;
        Ok(i)
    }
    pub fn write(&mut self, out: &mut Nodes, cx: Cx<'_>) -> R<()> {
        out.push(cx.int(self.values.len())?)?;
        self.values.as_mut_slice().reverse();
        while let Some(n) = self.values.pop() {
            out.push(n)?;
        }
        Ok(())
    }
}
pub(super) fn inverse(table: &str, name: &str) -> Option<&'static str> {
    for file in ["templates/tables.json", "ui.json", "std_pictures.json"] {
        if let Some(values) = mdschema::static_schema()
            .table(file)
            .and_then(|v| v.get(table))
            .and_then(|v| v.as_object())
        {
            for (key, _) in values {
                if (if table == "std_pictures" {
                    super::super::context::std_picture(key)
                } else {
                    super::super::context::merged(table, key)
                }) == Some(name)
                {
                    return Some(key);
                }
            }
        }
    }
    None
}
fn named_ref(cx: Cx<'_>, lookup: &dyn InverseLookup, table: &str, kind: &str, name: &str) -> R<N> {
    if let Some(key) = inverse(table, name) {
        return if key.starts_with('-') {
            cx.list([cx.atom(key)?])
        } else {
            cx.list([cx.int(0)?, cx.atom(key)?])
        };
    }
    let fullname = crate::allocation::Allocation::Managed(cx.budget).text(&[kind, ".", name])?;
    let id = lookup
        .object_id(&fullname)
        .ok_or(BuildError::Invalid("unresolved object reference"))?;
    cx.list([cx.int(0)?, cx.int(id)?])
}
pub(super) fn style_ref(
    cx: Cx<'_>,
    lookup: &dyn InverseLookup,
    table: &str,
    e: X<'_, '_>,
    v: &str,
) -> R<N> {
    let (ns, name) = qname(e, v)?;
    if ns != "http://v8.1c.ru/8.1/data/ui/style" {
        return Err(BuildError::Invalid("style namespace"));
    }
    named_ref(cx, lookup, table, "StyleItem", name)
}
pub(super) fn color(
    cx: Cx<'_>,
    lookup: &dyn InverseLookup,
    e: Option<X<'_, '_>>,
    default: i64,
) -> R<N> {
    let Some(e) = e else {
        return cx.list([cx.int(4)?, cx.int(3)?, cx.ints([default])?, cx.int(3)?]);
    };
    shape(e, &[], &[], &[])?;
    let value = leaf(e)?.trim();
    let (kind, reference) = if value == "auto" {
        (4, cx.ints([0])?)
    } else if let Some(hex) = value.strip_prefix('#') {
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(BuildError::Invalid("RGB color"));
        }
        let rgb = u32::from_str_radix(hex, 16).map_err(|_| BuildError::Invalid("RGB color"))?;
        (
            0,
            cx.ints([i64::from(
                ((rgb & 255) << 16) | (rgb & 0xff00) | (rgb >> 16),
            )])?,
        )
    } else {
        let (ns, name) = qname(e, value)?;
        match ns {
            "http://v8.1c.ru/8.1/data/ui/style" => {
                (3, style_ref(cx, lookup, "style_colors", e, value)?)
            }
            "http://v8.1c.ru/8.1/data/ui/colors/web" => (
                2,
                cx.list([cx.atom(
                    inverse("web_colors", name).ok_or(BuildError::Invalid("web color"))?,
                )?])?,
            ),
            "http://v8.1c.ru/8.1/data/ui/colors/windows" => (
                1,
                cx.list([cx.atom(
                    inverse("win_colors", name).ok_or(BuildError::Invalid("system color"))?,
                )?])?,
            ),
            _ => return Err(BuildError::Invalid("color namespace")),
        }
    };
    cx.list([cx.int(4)?, cx.int(kind)?, reference, cx.int(kind)?])
}
pub(super) fn picture_ref(cx: Cx<'_>, lookup: &dyn InverseLookup, e: X<'_, '_>, v: &str) -> R<N> {
    let (ns, name) = qname(e, v)?;
    if ns != "http://v8.1c.ru/8.1/data/ui" {
        return Err(BuildError::Invalid("picture namespace"));
    }
    let fullname =
        crate::allocation::Allocation::Managed(cx.budget).text(&["CommonPicture.", name])?;
    if let Some(id) = lookup.object_id(&fullname) {
        return cx.list([cx.int(0)?, cx.int(id)?]);
    }
    named_ref(cx, lookup, "std_pictures", "CommonPicture", name)
}
pub(super) fn resolved_color(node: &Node) -> Option<i64> {
    let items = node.as_list()?;
    if items.get(1)?.text() != Some("3") {
        return None;
    }
    let reference = items.get(2)?.as_list()?;
    if reference.len() != 1 {
        return None;
    }
    let key = reference.first()?.text()?;
    mdschema::static_schema()
        .table("templates/tables.json")?
        .get("resolved_style_colors")?
        .get(key)?
        .as_i64()
}
