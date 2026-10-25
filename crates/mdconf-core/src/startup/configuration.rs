use super::{
    ManagedDiagnostics, ManagedValueContext,
    collections::{count, slice},
    index::text,
    values::append,
    xml::Xml,
};
use crate::{Prop, Value};
use brace::{Guid, Node};
use mdschema::{Codec, StaticConfigPath, StaticConfigProp, StaticPropSpec, StaticValue as V};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget};
const CATEGORY: Category = Category::Metadata;
const PART2: [usize; 5] = [4, 1, 1, 1, 1];
type Contained = Vec<(Guid, Option<Guid>)>;
#[derive(Debug)]
pub struct ManagedConfigurationProperties {
    pub props: Charged<Vec<Prop>>,
    pub contained_objects: Charged<Contained>,
    pub diagnostics: ManagedDiagnostics,
}
enum Failure {
    Memory(BudgetError),
    Bad(BudgetedText),
}
impl From<BudgetError> for Failure {
    fn from(error: BudgetError) -> Self {
        Self::Memory(error)
    }
}
type R<T> = Result<T, Failure>;
fn required<T>(value: Option<T>, budget: &MemoryBudget, why: &[&str]) -> R<T> {
    match value {
        Some(value) => Ok(value),
        None => Err(Failure::Bad(text(budget, why)?)),
    }
}
fn first_object_id<'a>(
    budget: &MemoryBudget,
    root: &'a Node,
) -> Result<Option<&'a str>, BudgetError> {
    let mut stack = BudgetedVec::new(budget, CATEGORY)?;
    stack.try_push(root)?;
    while let Some(node) = stack.pop() {
        let Some(list) = node.as_list() else { continue };
        if let [a, b, c] = list
            && a.text() == Some("1")
            && b.text() == Some("0")
            && c.text().is_some_and(|value| value.chars().count() == 36)
        {
            return Ok(c.text());
        }
        for child in list.iter().filter(|node| node.is_list()).rev() {
            stack.try_push(child)?;
        }
    }
    Ok(None)
}
fn contained(budget: &MemoryBudget, raw: &Node) -> R<Charged<Contained>> {
    let list = required(raw.as_list(), budget, &["internal_info"])?;
    let n = required(list.get(2).and_then(count), budget, &["internal_info"])?;
    let parts = slice(list, 3, n);
    let mut out = BudgetedVec::try_with_capacity(budget, CATEGORY, parts.len())?.into_charged_vec();
    for part in parts {
        let part = required(part.as_list(), budget, &["internal_info"])?;
        let class = required(
            part.first().and_then(Node::text).and_then(Guid::parse),
            budget,
            &["internal_info"],
        )?;
        let root = required(part.get(1), budget, &["internal_info"])?;
        let object = match first_object_id(budget, root)? {
            Some(value) => Some(required(Guid::parse(value), budget, &["internal_info"])?),
            None => None,
        };
        out = append(out, super::values::empty(budget, (class, object))?)?;
    }
    Ok(out)
}
fn prop(
    budget: &MemoryBudget,
    name: &str,
    codec: Codec,
    value: Charged<Value>,
) -> Result<Charged<Prop>, BudgetError> {
    Ok(text(budget, &[name])?
        .into_string()?
        .try_zip(value)?
        .map(|(name, value)| Prop { name, codec, value }))
}
fn simple_spec(name: &str) -> Option<StaticPropSpec> {
    static BOOL: V = V::Object(&[("codec", V::String("bool"))]);
    static INT: V = V::Object(&[("codec", V::String("int"))]);
    static STR: V = V::Object(&[("codec", V::String("str"))]);
    static LSTR: V = V::Object(&[("codec", V::String("lstr"))]);
    static REF: V = V::Object(&[("codec", V::String("ref"))]);
    StaticPropSpec::from_static(match name {
        "bool" => &BOOL,
        "int" => &INT,
        "str" => &STR,
        "lstr" => &LSTR,
        "ref" => &REF,
        _ => return None,
    })
}
impl ManagedValueContext<'_> {
    fn config_value(
        &self,
        spec: StaticConfigProp,
        node: Option<&Node>,
    ) -> R<Option<Charged<Prop>>> {
        let budget = self.memory_budget();
        let name = spec.name();
        let codec = spec.codec();
        let simple = if let Some(spec) = simple_spec(codec) {
            let node = required(node, budget, &[name, ":", codec])?;
            Some((spec.codec(), self.leaf_value(spec, node, None)?))
        } else if let Some(codec) = match codec {
            "mdlist" => Some(Codec::MdList),
            "purposes" => Some(Codec::Purposes),
            _ => None,
        } {
            let node = required(node, budget, &[name, ":", spec.codec()])?;
            Some((codec, self.collection_value(codec, name, node, None)?))
        } else {
            None
        };
        if let Some((codec, value)) = simple {
            let value = required(value, budget, &[name, ":", spec.codec()])?;
            return Ok(Some(prop(budget, name, codec, value)?));
        }
        let (codec, value) = match codec {
            "enum" => {
                let code = required(node.and_then(Node::text), budget, &[name, ":enum"])?;
                let value = required(
                    spec.extra()
                        .get("map")
                        .and_then(|map| map.get(code))
                        .and_then(V::as_str),
                    budget,
                    &[name, ":enum:", code],
                )?;
                if spec.extra().get("omit").and_then(V::as_str) == Some(code) {
                    return Ok(None);
                }
                (
                    Codec::Enum,
                    text(budget, &[value])?.into_string()?.map(Value::Text),
                )
            }
            "const" => {
                let value = required(
                    spec.extra().get("value").and_then(V::as_str),
                    budget,
                    &[name, ":const"],
                )?;
                (
                    Codec::Enum,
                    text(budget, &[value])?.into_string()?.map(Value::Text),
                )
            }
            "compat" => {
                let n = required(
                    node.and_then(Node::text)
                        .and_then(|value| value.trim().parse::<i64>().ok())
                        .and_then(|value| u32::try_from(value).ok()),
                    budget,
                    &[name, ":compat"],
                )?;
                let value = match n {
                    0 => text(budget, &["Version8_2_16"])?,
                    1 => text(budget, &["Version8_1"])?,
                    2 => text(budget, &["Version8_2_13"])?,
                    _ => {
                        let mut value = text(budget, &["Version"])?;
                        let mut buffer = [0; 24];
                        for (at, number) in [n / 10000, (n / 100) % 100, n % 100]
                            .into_iter()
                            .enumerate()
                        {
                            if at > 0 {
                                value.try_push_str("_")?;
                            }
                            value.try_push_str(super::leaf::decimal(
                                i64::from(number),
                                &mut buffer,
                            )?)?;
                        }
                        value
                    }
                };
                (Codec::Enum, value.into_string()?.map(Value::Text))
            }
            "empty" => (
                Codec::Unknown,
                Xml::new(budget, name, 0, 0)?.into_charged().map(Value::Xml),
            ),
            "functionalities" => {
                let list = required(node.and_then(Node::as_list), budget, &[name, ":value"])?;
                let n = required(list.get(1).and_then(count), budget, &[name, ":value"])?;
                let items = slice(list, 2, n);
                let mut root = Xml::new(budget, name, 0, items.len())?;
                for item in items {
                    let item = required(item.as_list(), budget, &[name, ":value"])?;
                    let id =
                        required(item.first().and_then(Node::text), budget, &[name, ":value"])?;
                    let used =
                        required(item.get(1), budget, &[name, ":value"])?.text() == Some("1");
                    let functionality = required(
                        spec.extra()
                            .get("map")
                            .and_then(|map| map.get(id))
                            .and_then(V::as_str),
                        budget,
                        &[name, ":functionality:", id],
                    )?;
                    root = root.child(
                        Xml::new(budget, "app:functionality", 0, 2)?
                            .child(Xml::leaf(budget, "app:functionality", functionality, 0)?)?
                            .child(Xml::leaf(
                                budget,
                                "app:use",
                                if used { "true" } else { "false" },
                                0,
                            )?)?,
                    )?;
                }
                (Codec::Unknown, root.into_charged().map(Value::Xml))
            }
            "strlist" => {
                let list = required(node.and_then(Node::as_list), budget, &[name, ":value"])?;
                let n = required(list.first().and_then(count), budget, &[name, ":value"])?;
                let items = slice(list, 1, n);
                let mut root = Xml::new(budget, name, 0, items.len())?;
                for item in items {
                    let value = required(
                        if item.is_list() {
                            item.at(1).and_then(Node::text)
                        } else {
                            item.text()
                        },
                        budget,
                        &[name, ":value"],
                    )?;
                    root = root.child(Xml::leaf(budget, "v8:Value", value, 1)?.attr(
                        budget,
                        "xsi:type",
                        "xs:string",
                    )?)?;
                }
                (Codec::Unknown, root.into_charged().map(Value::Xml))
            }
            other => return Err(Failure::Bad(text(budget, &[name, ":codec:", other])?)),
        };
        Ok(Some(prop(budget, name, codec, value)?))
    }
    fn decode_config(
        &self,
        raw: &Node,
        diagnostics: &mut ManagedDiagnostics,
    ) -> R<(Charged<Contained>, Charged<Vec<Prop>>)> {
        let budget = self.memory_budget();
        let contained = contained(budget, raw)?;
        let body = required(raw.get(&[3, 1, 1]), budget, &["body"])?;
        let header = required(body.get(&[1, 1]), budget, &["header"])?;
        let mut props = BudgetedVec::try_with_capacity(
            budget,
            CATEGORY,
            3 + mdschema::static_schema().config_props().count(),
        )?
        .into_charged_vec();
        for (name, at) in [("Name", 2), ("Synonym", 3), ("Comment", 4)] {
            let node = required(header.at(at), budget, &["header"])?;
            if name == "Synonym" {
                match self.local_string(node)? {
                    Some(value) => {
                        props = append(
                            props,
                            prop(budget, name, Codec::LStr, value.map(Value::LStr))?,
                        )?
                    }
                    None => diagnostics.miss(&["configuration:Synonym:lstr"])?,
                }
            } else {
                let value = required(node.text(), budget, &["header"])?;
                props = append(
                    props,
                    prop(
                        budget,
                        name,
                        Codec::Str,
                        text(budget, &[value])?.into_string()?.map(Value::Text),
                    )?,
                )?;
            }
        }
        for spec in mdschema::static_schema().config_props() {
            let node = match spec.path() {
                Some(StaticConfigPath::Body(path)) => {
                    Some(path.iter().try_fold(body, |node, at| node.at(at)))
                }
                Some(StaticConfigPath::Part2) => Some(raw.get(&PART2)),
                None => None,
            };
            let node = match node {
                Some(value) => Some(required(value, budget, &[spec.name(), ":no_value"])?),
                None => None,
            };
            if let Some(value) = self.config_value(spec, node)? {
                props = append(props, value)?;
            }
        }
        Ok((contained, props))
    }
    pub fn configuration_properties(
        &self,
        raw: &Node,
    ) -> Result<ManagedConfigurationProperties, BudgetError> {
        let budget = self.memory_budget();
        let mut diagnostics = ManagedDiagnostics::new(budget)?;
        let (contained_objects, props) = match self.decode_config(raw, &mut diagnostics) {
            Ok(values) => values,
            Err(Failure::Memory(error)) => return Err(error),
            Err(Failure::Bad(why)) => {
                diagnostics.miss(&["configuration:", why.as_str()])?;
                (
                    BudgetedVec::new(budget, CATEGORY)?.into_charged_vec(),
                    BudgetedVec::new(budget, CATEGORY)?.into_charged_vec(),
                )
            }
        };
        Ok(ManagedConfigurationProperties {
            props,
            contained_objects,
            diagnostics,
        })
    }
}
