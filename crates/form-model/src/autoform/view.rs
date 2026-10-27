use mdconf::{ChildTag, MdKind, MdObject, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratedValue<'a> {
    Text(&'a str),
    Bool(bool),
    Other,
}
fn value(value: &Value) -> GeneratedValue<'_> {
    match value {
        Value::Text(text) => GeneratedValue::Text(text),
        Value::Bool(value) => GeneratedValue::Bool(*value),
        _ => GeneratedValue::Other,
    }
}
pub trait GeneratedObject {
    type Child<'a>: GeneratedChild
    where
        Self: 'a;
    fn kind(&self) -> MdKind;
    fn name(&self) -> &str;
    fn prop(&self, name: &str) -> Option<GeneratedValue<'_>>;
    fn children(&self) -> impl Iterator<Item = Self::Child<'_>>;
    fn attributes(&self) -> impl Iterator<Item = Self::Child<'_>> {
        self.children()
            .filter(|child| child.tag() == ChildTag::Attribute)
    }
    fn dimensions(&self) -> impl Iterator<Item = Self::Child<'_>> {
        self.children()
            .filter(|child| child.tag() == ChildTag::Dimension)
    }
    fn resources(&self) -> impl Iterator<Item = Self::Child<'_>> {
        self.children()
            .filter(|child| child.tag() == ChildTag::Resource)
    }
    fn tabular_sections(&self) -> impl Iterator<Item = Self::Child<'_>> {
        self.children()
            .filter(|child| child.tag() == ChildTag::TabularSection)
    }
}
pub trait GeneratedChild {
    type Child<'a>: GeneratedChild
    where
        Self: 'a;
    fn tag(&self) -> ChildTag;
    fn name(&self) -> &str;
    fn prop(&self, name: &str) -> Option<GeneratedValue<'_>>;
    fn is_boolean(&self) -> bool;
    fn children(&self) -> impl Iterator<Item = Self::Child<'_>>;
}
impl GeneratedObject for MdObject {
    type Child<'a> = &'a mdconf::Child;
    fn kind(&self) -> MdKind {
        self.kind
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn prop(&self, name: &str) -> Option<GeneratedValue<'_>> {
        self.prop(name).map(value)
    }
    fn children(&self) -> impl Iterator<Item = Self::Child<'_>> {
        self.children.iter()
    }
}
impl GeneratedChild for &mdconf::Child {
    type Child<'a>
        = &'a mdconf::Child
    where
        Self: 'a;
    fn tag(&self) -> ChildTag {
        self.tag
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn prop(&self, name: &str) -> Option<GeneratedValue<'_>> {
        mdconf::Child::prop(self, name).map(value)
    }
    fn is_boolean(&self) -> bool {
        self.type_desc()
            .is_some_and(|ty| matches!(ty.types.as_slice(), [ty] if ty.name.is_qname("xs:boolean")))
    }
    fn children(&self) -> impl Iterator<Item = Self::Child<'_>> {
        self.children.iter()
    }
}
