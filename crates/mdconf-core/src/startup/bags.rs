use super::{
    ManagedValueContext,
    collections::{count, slice},
    index::text,
    leaf::decimal,
    values::{append, empty},
    xml::Xml,
};
use crate::{Prop, StandardAttribute, Value};
use brace::Node;
use mdschema::{Codec, StaticPropSpec, static_schema};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged};
const CATEGORY: Category = Category::Metadata;
const T_DTREF: &str = "5c14e26f-099b-4d37-84a6-b433d87400da";
const T_FIXARR: &str = "4500381b-db30-4a10-9db4-990038032acf";
const T_MDREF: &str = "157fa490-4ce9-11d4-9415-008048da11f9";
macro_rules! present {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}
fn payload(node: &Node) -> &Node {
    match node.as_list() {
        Some([a, t, p])
            if a.text() == Some("#") && !matches!(t.text(), Some(T_DTREF | T_FIXARR)) =>
        {
            p
        }
        _ => node,
    }
}
fn second(node: &Node) -> Option<&Node> {
    match node.as_list() {
        Some(nodes) => nodes.get(1),
        None => Some(node),
    }
}
fn std_attr(kind: &str, sid: &str) -> Option<&'static str> {
    static_schema()
        .table("stdattrs.json")
        .and_then(|s| s.get(kind))
        .and_then(|s| s.get(sid))
        .and_then(|s| s.as_str())
        .or_else(|| crate::objects::overlay::std_attr(kind, sid))
}
impl ManagedValueContext<'_> {
    pub fn bag_value(
        &self,
        spec: StaticPropSpec,
        tag: &str,
        node: &Node,
        owner: Option<(&str, &str)>,
    ) -> Result<Option<Charged<Value>>, BudgetError> {
        let value = match spec.codec() {
            Codec::StdAttrs => {
                let owner = match spec
                    .field("std")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                {
                    Some(kind) => Some((kind, "")),
                    None => owner,
                };
                let [_, inner] = present!(node.as_list()) else {
                    return Ok(None);
                };
                present!(self.std_attr_list(
                    present!(inner.as_list()),
                    owner.map_or("", |o| o.0),
                    owner
                )?)
                .map(Value::StandardAttributes)
            }
            Codec::XdtoType => present!(self.xdto_type(tag, node)?)
                .into_charged()
                .map(Value::Xml),
            Codec::XdtoPackages => present!(self.xdto_packages(tag, node)?)
                .into_charged()
                .map(Value::Xml),
            Codec::Characteristics => present!(self.characteristics(tag, node)?)
                .into_charged()
                .map(Value::Xml),
            Codec::StyleValue => present!(self.style_value(tag, node)?)
                .into_charged()
                .map(Value::Xml),
            _ => return Ok(None),
        };
        Ok(Some(value))
    }
    pub(super) fn std_attr_list(
        &self,
        inner: &[Node],
        kind: &str,
        owner: Option<(&str, &str)>,
    ) -> Result<Option<Charged<Vec<StandardAttribute>>>, BudgetError> {
        let n = present!(inner.get(1).and_then(count));
        if n > inner.len().saturating_sub(2) / 3 {
            return Ok(None);
        }
        let mut out =
            BudgetedVec::try_with_capacity(self.memory_budget(), CATEGORY, n)?.into_charged_vec();
        for i in 0..n {
            let id = present!(inner.get(2 + 3 * i).and_then(Node::as_list));
            let sid = present!(id.first().and_then(Node::text));
            let name = present!(self.standard_name(kind, sid, id.get(1).and_then(Node::text))?);
            let bag = present!(inner.get(4 + 3 * i).and_then(Node::as_list));
            let k = present!(bag.get(1).and_then(count));
            if k > bag.len().saturating_sub(2) / 2 {
                return Ok(None);
            }
            let mut props = BudgetedVec::try_with_capacity(self.memory_budget(), CATEGORY, k)?
                .into_charged_vec();
            for j in 0..k {
                let pg = present!(bag.get(2 + 2 * j).and_then(Node::text));
                let val = present!(bag.get(3 + 2 * j));
                let raw = present!(
                    static_schema()
                        .table("stdattr_bag.json")
                        .and_then(|s| s.get("props"))
                        .and_then(|s| s.get(pg))
                );
                let spec = present!(StaticPropSpec::from_static(raw));
                let xml = present!(raw.get("xml").and_then(|s| s.as_str()));
                let tag = text(self.memory_budget(), &["xr:", xml])?;
                let value = match spec.codec() {
                    Codec::Bool => empty(
                        self.memory_budget(),
                        Value::Bool(present!(second(val)).text() == Some("1")),
                    )?,
                    Codec::Str => present!(self.leaf_value(spec, present!(second(val)), None)?),
                    Codec::Enum | Codec::Ref => {
                        present!(self.leaf_value(spec, present!(second(payload(val))), None)?)
                    }
                    Codec::TValue => match self.account_type(tag.as_str(), val)? {
                        Some(value) => value,
                        None => present!(self.leaf_value(spec, val, None)?),
                    },
                    Codec::CpLinks | Codec::LinkByType | Codec::CParams => present!(
                        self.collection_value(spec.codec(), tag.as_str(), payload(val), owner)?
                    ),
                    _ => present!(self.leaf_value(spec, payload(val), None)?),
                };
                let prop = text(self.memory_budget(), &[xml])?
                    .into_string()?
                    .try_zip(value)?
                    .map(|(name, value)| Prop {
                        name,
                        codec: spec.codec(),
                        value,
                    });
                props = append(props, prop)?;
            }
            let item = name
                .into_string()?
                .try_zip(props)?
                .map(|(name, props)| StandardAttribute { name, props });
            out = append(out, item)?;
        }
        Ok(Some(out))
    }
    fn standard_name(
        &self,
        kind: &str,
        sid: &str,
        class: Option<&str>,
    ) -> Result<Option<BudgetedText>, BudgetError> {
        if kind == "AccountingRegister" {
            let prefix = match class {
                Some("91162600-3161-4326-89a0-4a7cecd5092a") => Some("ExtDimension"),
                Some("b3b48b29-d652-47ab-9d21-7e06768c31b5") => Some("ExtDimensionType"),
                _ => None,
            };
            if let Some(prefix) = prefix {
                let number = sid.parse::<u32>().ok().and_then(|n| n.checked_add(1));
                if let Some(number) = number {
                    let mut buffer = [0; 24];
                    return Ok(Some(text(
                        self.memory_budget(),
                        &[prefix, decimal(i64::from(number), &mut buffer)?],
                    )?));
                }
            }
        }
        Ok(Some(text(
            self.memory_budget(),
            &[present!(std_attr(kind, sid))],
        )?))
    }
    fn account_type(&self, tag: &str, node: &Node) -> Result<Option<Charged<Value>>, BudgetError> {
        let [a, t, code] = present!(node.as_list()) else {
            return Ok(None);
        };
        if a.text() != Some("#") || t.text() != Some("872f7198-7083-4e3e-b57e-a2a9802c769e") {
            return Ok(None);
        }
        let name = match present!(code.text()) {
            "0" => "Active",
            "1" => "Passive",
            "2" => "ActivePassive",
            _ => return Ok(None),
        };
        Ok(Some(
            Xml::leaf(self.memory_budget(), tag, name, 1)?
                .attr(self.memory_budget(), "xsi:type", "ent:AccountType")?
                .into_charged()
                .map(Value::Xml),
        ))
    }
    fn xdto_type(&self, tag: &str, node: &Node) -> Result<Option<Xml>, BudgetError> {
        let ns = present!(node.at(1).and_then(Node::text));
        let name = present!(node.at(2).and_then(Node::text));
        if ns.is_empty() {
            return Ok(Some(Xml::leaf(self.memory_budget(), tag, name, 0)?));
        }
        let prefix = crate::xml_out::NS_DECL.split_whitespace().find_map(|d| {
            let (p, u) = d.strip_prefix("xmlns:")?.split_once('=')?;
            (u.trim_matches('"') == ns).then_some(p)
        });
        if let Some(prefix) = prefix {
            let value = text(self.memory_budget(), &[prefix, ":", name])?;
            return Ok(Some(Xml::leaf(
                self.memory_budget(),
                tag,
                value.as_str(),
                0,
            )?));
        }
        let (prefix, attribute) = if tag == "XDTOReturningValueType" {
            ("d6p1", "xmlns:d6p1")
        } else {
            ("d8p1", "xmlns:d8p1")
        };
        let value = text(self.memory_budget(), &[prefix, ":", name])?;
        Ok(Some(
            Xml::leaf(self.memory_budget(), tag, value.as_str(), 1)?.attr(
                self.memory_budget(),
                attribute,
                ns,
            )?,
        ))
    }
    fn xdto_packages(&self, tag: &str, node: &Node) -> Result<Option<Xml>, BudgetError> {
        let refs = present!(node.at(3).and_then(Node::as_list));
        let uris = present!(node.at(5).and_then(Node::as_list));
        let n = present!(refs.get(1).and_then(count));
        let k = present!(uris.first().and_then(count));
        let refs = slice(refs, 2, n);
        let uris = slice(uris, 1, k);
        let cap = refs
            .len()
            .checked_add(uris.len())
            .ok_or(BudgetError::Overflow)?;
        let mut root = Xml::new(self.memory_budget(), tag, 0, cap)?;
        for reference in refs {
            if reference.at(0).and_then(Node::text) != Some("#")
                || reference.at(1).and_then(Node::text) != Some(T_MDREF)
            {
                return Ok(None);
            }
            let id = present!(
                reference
                    .at(2)
                    .and_then(|n| n.at(1))
                    .and_then(Node::as_guid)
            );
            let name = present!(self.index.names.get(&id));
            root = root.child(self.xdto_item("xr:MDObjectRef", name.as_str())?)?;
        }
        for uri in uris {
            root = root.child(self.xdto_item("xs:string", present!(uri.text()))?)?
        }
        Ok(Some(root))
    }
    fn xdto_item(&self, kind: &str, value: &str) -> Result<Xml, BudgetError> {
        Xml::new(self.memory_budget(), "xr:Item", 0, 3)?
            .child(Xml::new(self.memory_budget(), "xr:Presentation", 0, 0)?)?
            .child(Xml::leaf(self.memory_budget(), "xr:CheckState", "0", 0)?)?
            .child(Xml::leaf(self.memory_budget(), "xr:Value", value, 1)?.attr(
                self.memory_budget(),
                "xsi:type",
                kind,
            )?)
    }
    fn char_field(&self, field: &Node, owner: &str) -> Result<Option<BudgetedText>, BudgetError> {
        let r = present!(field.at(1).and_then(Node::as_list));
        let first = present!(r.first().and_then(Node::text));
        if first == "-1" {
            return Ok(Some(text(self.memory_budget(), &["-1"])?));
        }
        if first == "0" {
            let name = match r.get(1) {
                Some(value) => {
                    let id = present!(value.as_guid());
                    present!(self.index.names.get(&id)).as_str()
                }
                None => "0",
            };
            return Ok(Some(text(self.memory_budget(), &[name])?));
        }
        let first_kind = present!(owner.split('.').next());
        let mut parts = owner.rsplit('.');
        let _ = parts.next();
        let kind = if owner.split('.').count() >= 4 {
            present!(parts.next())
        } else {
            first_kind
        };
        let name = static_schema()
            .table("stdattrs.json")
            .and_then(|s| s.get(kind))
            .and_then(|s| s.get(first))
            .and_then(|s| s.as_str())
            .or_else(|| {
                (kind == "TabularSection")
                    .then(|| {
                        static_schema()
                            .table("stdattrs.json")?
                            .get(first_kind)?
                            .get(first)?
                            .as_str()
                    })
                    .flatten()
            });
        Ok(Some(text(
            self.memory_budget(),
            &[owner, ".StandardAttribute.", present!(name)],
        )?))
    }
    fn characteristics(&self, tag: &str, node: &Node) -> Result<Option<Xml>, BudgetError> {
        let items = present!(node.at(1).and_then(Node::as_list));
        let n = present!(items.first().and_then(count));
        let items = slice(items, 1, n);
        let mut root = Xml::new(self.memory_budget(), tag, 0, items.len())?;
        for item in items {
            if item.at(1).and_then(Node::text) != Some("fe839d42-d094-40ba-b903-75bccc21ba30") {
                return Ok(None);
            }
            let c = present!(item.at(2).and_then(Node::as_list));
            let from = |at: usize| {
                c.get(at)?
                    .at(1)?
                    .as_guid()
                    .and_then(|id| self.index.names.get(&id))
            };
            let tfrom = present!(from(1));
            let vfrom = present!(from(2));
            let mut types = Xml::new(self.memory_budget(), "xr:CharacteristicTypes", 1, 5)?.attr(
                self.memory_budget(),
                "from",
                tfrom.as_str(),
            )?;
            for (name, at) in [("xr:KeyField", 8), ("xr:TypesFilterField", 6)] {
                let value = present!(self.char_field(present!(c.get(at)), tfrom.as_str())?);
                types = types.child(Xml::leaf(self.memory_budget(), name, value.as_str(), 0)?)?;
            }
            let tv = present!(self.typed_value(present!(c.get(7)))?);
            types = types.child(Xml::typed(
                self.memory_budget(),
                "xr:TypesFilterValue",
                &tv,
            )?)?;
            for (name, at) in [("xr:DataPathField", 9), ("xr:MultipleValuesUseField", 10)] {
                let value = present!(self.char_field(present!(c.get(at)), tfrom.as_str())?);
                types = types.child(Xml::leaf(self.memory_budget(), name, value.as_str(), 0)?)?;
            }
            let mut values = Xml::new(self.memory_budget(), "xr:CharacteristicValues", 1, 5)?
                .attr(self.memory_budget(), "from", vfrom.as_str())?;
            for (name, at) in [
                ("xr:ObjectField", 3),
                ("xr:TypeField", 4),
                ("xr:ValueField", 5),
                ("xr:MultipleValuesKeyField", 11),
                ("xr:MultipleValuesOrderField", 12),
            ] {
                let value = present!(self.char_field(present!(c.get(at)), vfrom.as_str())?);
                values = values.child(Xml::leaf(self.memory_budget(), name, value.as_str(), 0)?)?;
            }
            root = root.child(
                Xml::new(self.memory_budget(), "xr:Characteristic", 0, 2)?
                    .child(types)?
                    .child(values)?,
            )?;
        }
        Ok(Some(root))
    }
}
