use super::{ManagedValueContext, index::text, values::append, xml::Xml};
use crate::{MdRef, Value};
use brace::Node;
use mdschema::Codec;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged};
const CATEGORY: Category = Category::Metadata;
const T_MDREF: &str = "157fa490-4ce9-11d4-9415-008048da11f9";
const ZERO: &str = "00000000-0000-0000-0000-000000000000";
macro_rules! present {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}
pub(super) fn count(node: &Node) -> Option<usize> {
    usize::try_from(node.text()?.trim().parse::<i64>().ok()?).ok()
}
pub(super) fn slice(nodes: &[Node], from: usize, count: usize) -> &[Node] {
    let tail = nodes.get(from..).unwrap_or(&[]);
    tail.get(..count.min(tail.len())).unwrap_or(tail)
}
impl ManagedValueContext<'_> {
    pub fn collection_value(
        &self,
        codec: Codec,
        tag: &str,
        node: &Node,
        owner: Option<(&str, &str)>,
    ) -> Result<Option<Charged<Value>>, BudgetError> {
        let value = match codec {
            Codec::MdList | Codec::FoContent => {
                let list = present!(node.as_list());
                let n = present!(list.get(1).and_then(count));
                let items = slice(list, 2, n);
                let mut refs =
                    BudgetedVec::try_with_capacity(self.memory_budget(), CATEGORY, items.len())?
                        .into_charged_vec();
                for item in items {
                    let item = if codec == Codec::FoContent {
                        present!(item.at(2).and_then(|n| n.at(1)))
                    } else {
                        item
                    };
                    let reference = present!(self.object_ref(item)?);
                    refs = append(refs, reference)?;
                }
                refs.map(Value::RefList)
            }
            Codec::FieldList => {
                let list = present!(node.at(1).and_then(Node::as_list));
                let n = present!(list.get(1).and_then(count));
                let (kind, name) = present!(owner);
                let items = slice(list, 2, n);
                let mut fields =
                    BudgetedVec::try_with_capacity(self.memory_budget(), CATEGORY, items.len())?
                        .into_charged_vec();
                for item in items {
                    let field = present!(self.field_ref(kind, name, present!(item.at(2)))?);
                    fields = append(fields, field.into_string()?)?;
                }
                fields.map(Value::FieldList)
            }
            Codec::Picture => present!(self.picture(tag, node)?)
                .into_charged()
                .map(Value::Xml),
            Codec::Purposes => {
                let list = present!(node.as_list());
                let n = present!(list.first().and_then(count));
                let items = slice(list, 1, n);
                let mut root = Xml::new(self.memory_budget(), tag, 0, items.len())?;
                for item in items {
                    let value = match present!(item.at(2).and_then(Node::text)) {
                        "1" => "PlatformApplication",
                        "2" => "MobilePlatformApplication",
                        _ => return Ok(None),
                    };
                    let child = Xml::leaf(self.memory_budget(), "v8:Value", value, 1)?.attr(
                        self.memory_budget(),
                        "xsi:type",
                        "app:ApplicationUsePurpose",
                    )?;
                    root = root.child(child)?;
                }
                root.into_charged().map(Value::Xml)
            }
            Codec::CaContent => {
                let list = present!(node.as_list());
                let n = present!(list.get(1).and_then(count));
                if n > list.len().saturating_sub(2) / 2 {
                    return Ok(None);
                }
                let mut root = Xml::new(self.memory_budget(), tag, 0, n)?;
                for i in 0..n {
                    let id = present!(list.get(2 + 2 * i).and_then(Node::as_guid));
                    let name = present!(self.index.names.get(&id));
                    let sep = present!(list.get(3 + 2 * i));
                    let use_ = match present!(sep.at(1).and_then(Node::text)) {
                        "0" => "Auto",
                        "1" => "Use",
                        "2" => "DontUse",
                        _ => return Ok(None),
                    };
                    let cs = present!(sep.at(2).and_then(Node::text));
                    let mut conditional =
                        Xml::new(self.memory_budget(), "xr:ConditionalSeparation", 0, 0)?;
                    if cs != ZERO {
                        let id = present!(brace::Guid::parse(cs));
                        let name = present!(self.index.names.get(&id));
                        conditional = conditional.text(self.memory_budget(), name.as_str())?;
                    }
                    let child = Xml::new(self.memory_budget(), "xr:Item", 0, 3)?
                        .child(Xml::leaf(
                            self.memory_budget(),
                            "xr:Metadata",
                            name.as_str(),
                            0,
                        )?)?
                        .child(Xml::leaf(self.memory_budget(), "xr:Use", use_, 0)?)?
                        .child(conditional)?;
                    root = root.child(child)?;
                }
                root.into_charged().map(Value::Xml)
            }
            Codec::CpLinks => present!(self.cp_links(tag, node, present!(owner))?)
                .into_charged()
                .map(Value::Xml),
            Codec::LinkByType => {
                let (kind, name) = present!(owner);
                let list = present!(node.as_list());
                let n = present!(list.get(1).and_then(count));
                if n == 0 {
                    return Ok(Some(
                        Xml::new(self.memory_budget(), tag, 0, 0)?
                            .into_charged()
                            .map(Value::Xml),
                    ));
                }
                let end = present!(2usize.checked_add(n));
                let path = present!(self.field_path(kind, name, present!(list.get(2..end)))?);
                let item = present!(list.get(end).and_then(Node::text));
                Xml::new(self.memory_budget(), tag, 0, 2)?
                    .child(Xml::leaf(
                        self.memory_budget(),
                        "xr:DataPath",
                        path.as_str(),
                        0,
                    )?)?
                    .child(Xml::leaf(self.memory_budget(), "xr:LinkItem", item, 0)?)?
                    .into_charged()
                    .map(Value::Xml)
            }
            Codec::CParams => {
                let list = present!(node.as_list());
                let n = present!(list.get(1).and_then(count));
                if n > list.len().saturating_sub(2) / 2 {
                    return Ok(None);
                }
                let mut root = Xml::new(self.memory_budget(), tag, 0, n)?;
                for i in 0..n {
                    let name = present!(list.get(2 + 2 * i).and_then(Node::text));
                    let value = present!(self.typed_value(present!(list.get(3 + 2 * i)))?);
                    let child = Xml::new(self.memory_budget(), "app:item", 1, 1)?
                        .attr(self.memory_budget(), "name", name)?
                        .child(Xml::typed(self.memory_budget(), "app:value", &value)?)?;
                    root = root.child(child)?;
                }
                root.into_charged().map(Value::Xml)
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }
    fn object_ref(&self, node: &Node) -> Result<Option<Charged<MdRef>>, BudgetError> {
        if node.at(0).and_then(Node::text) != Some("#")
            || node.at(1).and_then(Node::text) != Some(T_MDREF)
        {
            return Ok(None);
        }
        let id = present!(node.at(2).and_then(|n| n.at(1)).and_then(Node::as_guid));
        let name = present!(self.index.names.get(&id));
        Ok(Some(
            text(self.memory_budget(), &[name.as_str()])?
                .into_string()?
                .map(|name| MdRef { id: Some(id), name }),
        ))
    }
    fn picture(&self, tag: &str, node: &Node) -> Result<Option<Xml>, BudgetError> {
        let list = present!(node.as_list());
        if present!(list.get(1).and_then(Node::text)) == "0" {
            return Ok(Some(Xml::new(self.memory_budget(), tag, 0, 0)?));
        }
        let refs = present!(list.get(2).and_then(Node::as_list));
        let first = present!(refs.first().and_then(Node::text));
        let name = if first == "0" && refs.len() > 1 {
            let raw = present!(refs.get(1).and_then(Node::text));
            if let Some(name) = brace::Guid::parse(raw).and_then(|id| self.index.names.get(&id)) {
                text(self.memory_budget(), &[name.as_str()])?
            } else {
                text(
                    self.memory_budget(),
                    &[
                        "StdPicture.",
                        present!(ui("std_pictures", raw).filter(|s| !s.is_empty())),
                    ],
                )?
            }
        } else {
            text(
                self.memory_budget(),
                &[
                    "StdPicture.",
                    present!(ui("std_pictures", first).filter(|s| !s.is_empty())),
                ],
            )?
        };
        let transparent = if present!(list.get(6).and_then(Node::text)) == "1" {
            "true"
        } else {
            "false"
        };
        Ok(Some(
            Xml::new(self.memory_budget(), tag, 0, 2)?
                .child(Xml::leaf(self.memory_budget(), "xr:Ref", name.as_str(), 0)?)?
                .child(Xml::leaf(
                    self.memory_budget(),
                    "xr:LoadTransparent",
                    transparent,
                    0,
                )?)?,
        ))
    }
    fn cp_links(
        &self,
        tag: &str,
        node: &Node,
        (kind, owner): (&str, &str),
    ) -> Result<Option<Xml>, BudgetError> {
        let list = present!(node.as_list());
        let n = present!(list.get(1).and_then(count));
        if n > list.len().saturating_sub(2) / 3 {
            return Ok(None);
        }
        let mut root = Xml::new(self.memory_budget(), tag, 0, n)?;
        let mut i = 2usize;
        for _ in 0..n {
            let name = present!(list.get(i).and_then(Node::text));
            let n = present!(list.get(present!(i.checked_add(1))).and_then(count));
            let start = present!(i.checked_add(2));
            let end = present!(start.checked_add(n));
            let path = present!(self.field_path(kind, owner, present!(list.get(start..end)))?);
            let change = if present!(list.get(end).and_then(Node::text)) == "1" {
                "DontChange"
            } else {
                "Clear"
            };
            i = present!(end.checked_add(1));
            let child = Xml::new(self.memory_budget(), "xr:Link", 0, 3)?
                .child(Xml::leaf(self.memory_budget(), "xr:Name", name, 0)?)?
                .child(
                    Xml::leaf(self.memory_budget(), "xr:DataPath", path.as_str(), 1)?.attr(
                        self.memory_budget(),
                        "xsi:type",
                        "xs:string",
                    )?,
                )?
                .child(Xml::leaf(
                    self.memory_budget(),
                    "xr:ValueChange",
                    change,
                    0,
                )?)?;
            root = root.child(child)?;
        }
        Ok(Some(root))
    }
}
fn ui(table: &str, key: &str) -> Option<&'static str> {
    mdschema::static_schema()
        .table("ui.json")?
        .get(table)?
        .get(key)?
        .as_str()
}
