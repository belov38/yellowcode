use super::index::{GuidText, text};
use super::{ManagedIndex, ManagedPredefined};
use crate::{
    BinaryQualifiers, DateFractions, NumberQualifiers, StringQualifiers, TypeDescription, TypeRef,
    TypedValue,
};
use brace::{Guid, Node};
use mdschema::static_schema;
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget};
const CATEGORY: Category = Category::Metadata;
const T_DTREF: &str = "5c14e26f-099b-4d37-84a6-b433d87400da";
const T_TYPEDESC: &str = "f5c65050-3bbb-11d5-b988-0050bae0a95d";
const T_FIXARR: &str = "4500381b-db30-4a10-9db4-990038032acf";
const ZERO: &str = "00000000-0000-0000-0000-000000000000";
macro_rules! present {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => return Ok(None),
        }
    };
}

pub struct ManagedValueContext<'a> {
    pub(super) index: &'a ManagedIndex,
    predef: &'a ManagedPredefined,
}
impl<'a> ManagedValueContext<'a> {
    pub fn new(
        index: &'a ManagedIndex,
        predef: &'a ManagedPredefined,
    ) -> Result<Self, BudgetError> {
        if !index.memory_budget().same_instance(predef.memory_budget()) {
            return Err(BudgetError::Limit(memory_budget::MemoryLimit {
                category: CATEGORY,
                requested: 0,
                reason: memory_budget::LimitReason::InvalidSize,
            }));
        }
        Ok(Self { index, predef })
    }
    pub fn memory_budget(&self) -> &MemoryBudget {
        self.index.memory_budget()
    }
    pub fn type_desc(&self, pat: &Node) -> Result<Option<Charged<TypeDescription>>, BudgetError> {
        self.type_desc_with_platform(pat, |raw| static_schema().platform_type(raw))
    }
    fn type_desc_with_platform<'p>(
        &self,
        pat: &Node,
        platform: impl Fn(&str) -> Option<mdschema::BorrowedPlatformType<'p>>,
    ) -> Result<Option<Charged<TypeDescription>>, BudgetError> {
        let list = present!(pat.as_list());
        if list.first().and_then(Node::text) != Some("Pattern") {
            return Ok(None);
        }
        let items = present!(list.get(1..));
        let mut out =
            BudgetedVec::<TypeRef>::try_with_capacity(self.memory_budget(), CATEGORY, items.len())?
                .into_charged_vec()
                .map(|types| TypeDescription {
                    types,
                    number: None,
                    string: None,
                    date: None,
                    binary: None,
                });
        for item in items {
            let item = present!(item.as_list());
            let code = present!(item.first().and_then(Node::text));
            let (name, set, type_id) = match code {
                "B" => (self.fixed_name("xs:boolean")?, false, None),
                "S" => {
                    let (length, fixed) = present!(len_fixed(item));
                    out = out.map(|mut d| {
                        d.string = Some(StringQualifiers { length, fixed });
                        d
                    });
                    (self.fixed_name("xs:string")?, false, None)
                }
                "N" => {
                    let digits = present!(item.get(1).and_then(num));
                    let fraction = present!(item.get(2).and_then(num));
                    let nonnegative = present!(item.get(3).and_then(Node::text)) == "1";
                    out = out.map(|mut d| {
                        d.number = Some(NumberQualifiers {
                            digits,
                            fraction,
                            nonnegative,
                        });
                        d
                    });
                    (self.fixed_name("xs:decimal")?, false, None)
                }
                "D" => {
                    let date = match item.get(1).and_then(Node::text) {
                        Some("D") => DateFractions::Date,
                        Some("T") => DateFractions::Time,
                        _ => DateFractions::DateTime,
                    };
                    out = out.map(|mut d| {
                        d.date = Some(date);
                        d
                    });
                    (self.fixed_name("xs:dateTime")?, false, None)
                }
                "R" => {
                    let (length, fixed) = present!(len_fixed(item));
                    out = out.map(|mut d| {
                        d.binary = Some(BinaryQualifiers { length, fixed });
                        d
                    });
                    (self.fixed_name("xs:base64Binary")?, false, None)
                }
                "#" => {
                    let raw = present!(item.get(1).and_then(Node::text));
                    let id = present!(Guid::parse(raw));
                    if let Some(name) = self.index.types.get(&id) {
                        let expanded = present!(mdschema::ExpandedTypeName::new(
                            mdschema::FixedNamespace::Cfg.uri(),
                            name.as_str()
                        ));
                        (
                            self.type_name(expanded)?,
                            name.as_str().split('.').next().is_some_and(|category| {
                                crate::objects::types::SET_CATEGORIES.contains(&category)
                            }),
                            Some(id),
                        )
                    } else if raw == T_TYPEDESC {
                        (self.fixed_name("v8:TypeDescription")?, false, None)
                    } else {
                        let platform = present!(platform(raw));
                        (self.type_name(platform.name)?, platform.set, None)
                    }
                }
                _ => return Ok(None),
            };
            if out.types.len() == out.types.capacity() {
                return Err(BudgetError::Overflow);
            }
            out = out.try_zip(name)?.map(|(mut d, name)| {
                d.types.push(TypeRef { name, set, type_id });
                d
            });
        }
        Ok(Some(out))
    }
    fn fixed_name(&self, qname: &str) -> Result<Charged<crate::XmlTypeName>, BudgetError> {
        let name = mdschema::ExpandedTypeName::from_qname(qname).ok_or(BudgetError::Overflow)?;
        self.type_name(name)
    }
    fn type_name(
        &self,
        name: mdschema::ExpandedTypeName<'_>,
    ) -> Result<Charged<crate::XmlTypeName>, BudgetError> {
        mdschema::ExpandedTypeName::new(name.namespace_uri, name.local)
            .ok_or(BudgetError::Overflow)?;
        let local = text(self.memory_budget(), &[name.local])?.into_string()?;
        if let Some(ns) = mdschema::FixedNamespace::from_uri(name.namespace_uri) {
            Ok(local.map(|local| crate::XmlTypeName {
                namespace: crate::TypeNamespace::Fixed(ns),
                local,
            }))
        } else {
            let uri = text(self.memory_budget(), &[name.namespace_uri])?.into_string()?;
            Ok(local.try_zip(uri)?.map(|(local, uri)| crate::XmlTypeName {
                namespace: crate::TypeNamespace::Uri(uri),
                local,
            }))
        }
    }
    pub fn design_time_ref(&self, node: &Node) -> Result<Option<BudgetedText>, BudgetError> {
        let list = present!(node.as_list());
        let object = present!(list.get(1).and_then(Node::text));
        let value = present!(list.get(2).and_then(Node::text));
        if object == ZERO {
            return Ok(Some(text(self.memory_budget(), &[""])?));
        }
        let id = present!(Guid::parse(object));
        let on = if let Some(name) = self.index.names.get(&id) {
            text(self.memory_budget(), &[name.as_str()])?
        } else if let Some(name) = self.index.types.get(&id) {
            let (prefix, rest) = present!(name.as_str().split_once('.'));
            text(
                self.memory_budget(),
                &[prefix.strip_suffix("Ref").unwrap_or(prefix), ".", rest],
            )?
        } else if value == ZERO {
            let id = GuidText::new(id);
            return Ok(Some(text(
                self.memory_budget(),
                &[id.as_str()?, ".", ZERO],
            )?));
        } else {
            return Ok(None);
        };
        if value == ZERO {
            return Ok(Some(text(
                self.memory_budget(),
                &[on.as_str(), ".EmptyRef"],
            )?));
        }
        let id = present!(Guid::parse(value));
        if let Some(name) = self.index.names.get(&id) {
            return Ok(Some(text(self.memory_budget(), &[name.as_str()])?));
        }
        let name = present!(self.predef.get(&id).filter(|s| !s.is_empty()));
        Ok(Some(text(self.memory_budget(), &[on.as_str(), ".", name])?))
    }
    pub fn typed_value(&self, node: &Node) -> Result<Option<Charged<TypedValue>>, BudgetError> {
        let mut stack = BudgetedVec::<ArrayFrame<'_>>::new(self.memory_budget(), CATEGORY)?;
        let mut next = node;
        loop {
            let head = present!(self.typed_head(next)?);
            let mut value = match head {
                Head::Value(value) => value,
                Head::Array(nodes) => {
                    let values = BudgetedVec::try_with_capacity(
                        self.memory_budget(),
                        CATEGORY,
                        nodes.len(),
                    )?
                    .into_charged_vec();
                    if let Some(first) = nodes.first() {
                        stack.try_push(ArrayFrame {
                            nodes,
                            next: 1,
                            values,
                        })?;
                        next = first;
                        continue;
                    }
                    values.map(TypedValue::FixedArray)
                }
            };
            loop {
                let Some(mut frame) = stack.pop() else {
                    return Ok(Some(value));
                };
                frame.values = append(frame.values, value)?;
                if let Some(node) = frame.nodes.get(frame.next) {
                    frame.next += 1;
                    next = node;
                    stack.try_push(frame)?;
                    break;
                }
                value = frame.values.map(TypedValue::FixedArray);
            }
        }
    }
    fn typed_head<'n>(&self, node: &'n Node) -> Result<Option<Head<'n>>, BudgetError> {
        let list = present!(node.as_list());
        let arg = || list.get(1).and_then(Node::text);
        let value = match present!(list.first().and_then(Node::text)) {
            "U" => empty(self.memory_budget(), TypedValue::Nil)?,
            "S" => description_text(self.memory_budget(), present!(arg()))?
                .into_string()?
                .map(TypedValue::Str),
            "N" => text(self.memory_budget(), &[present!(arg())])?
                .into_string()?
                .map(TypedValue::Number),
            "B" => empty(
                self.memory_budget(),
                TypedValue::Bool(present!(arg()) == "1"),
            )?,
            "D" => {
                let value = present!(arg());
                if value.len() != 14 || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return Ok(None);
                }
                text(
                    self.memory_budget(),
                    &[
                        present!(value.get(..4)),
                        "-",
                        present!(value.get(4..6)),
                        "-",
                        present!(value.get(6..8)),
                        "T",
                        present!(value.get(8..10)),
                        ":",
                        present!(value.get(10..12)),
                        ":",
                        present!(value.get(12..14)),
                    ],
                )?
                .into_string()?
                .map(TypedValue::Date)
            }
            "#" => match present!(arg()) {
                T_DTREF => present!(self.design_time_ref(present!(list.get(2)))?)
                    .into_string()?
                    .map(TypedValue::DesignTimeRef),
                T_TYPEDESC => {
                    let pattern = present!(list.get(2).and_then(Node::as_list));
                    if !matches!(pattern, [p] if p.text() == Some("Pattern")) {
                        return Ok(None);
                    }
                    empty(self.memory_budget(), TypedValue::EmptyTypeDescription)?
                }
                T_FIXARR => {
                    let array = present!(list.get(2).and_then(Node::as_list));
                    let count = present!(
                        array
                            .first()
                            .and_then(|n| n.text()?.trim().parse::<i64>().ok())
                            .and_then(|n| usize::try_from(n).ok())
                    );
                    let tail = array.get(1..).unwrap_or(&[]);
                    return Ok(Some(Head::Array(present!(
                        tail.get(..count.min(tail.len()))
                    ))));
                }
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        Ok(Some(Head::Value(value)))
    }
}
enum Head<'a> {
    Value(Charged<TypedValue>),
    Array(&'a [Node]),
}
struct ArrayFrame<'a> {
    nodes: &'a [Node],
    next: usize,
    values: Charged<Vec<TypedValue>>,
}
fn num(node: &Node) -> Option<u32> {
    node.text()?.trim().parse().ok()
}
fn len_fixed(items: &[Node]) -> Option<(u32, bool)> {
    let length = match items.get(1) {
        Some(n) => num(n)?,
        None => 0,
    };
    let variable = match items.get(2) {
        Some(n) => n.text()? == "1",
        None => true,
    };
    Some((length, !variable))
}
pub fn empty<T>(budget: &MemoryBudget, value: T) -> Result<Charged<T>, BudgetError> {
    Ok(BudgetedVec::<u8>::new(budget, CATEGORY)?
        .into_charged_vec()
        .map(|_| value))
}
pub fn append<T>(
    values: Charged<Vec<T>>,
    value: Charged<T>,
) -> Result<Charged<Vec<T>>, BudgetError> {
    if values.len() == values.capacity() {
        return Err(BudgetError::Overflow);
    }
    Ok(values.try_zip(value)?.map(|(mut values, value)| {
        values.push(value);
        values
    }))
}
pub(super) fn description_text(
    budget: &MemoryBudget,
    value: &str,
) -> Result<BudgetedText, BudgetError> {
    let len = value
        .len()
        .checked_sub(value.matches("\r\n").count())
        .ok_or(BudgetError::Overflow)?;
    let limit = u64::try_from(len).map_err(|_| BudgetError::Overflow)?;
    let mut out = BudgetedText::try_with_capacity(budget, CATEGORY, limit, len)?;
    for (i, part) in value.split("\r\n").enumerate() {
        if i != 0 {
            out.try_push_str("\n")?;
        }
        out.try_push_str(part)?;
    }
    Ok(out)
}
