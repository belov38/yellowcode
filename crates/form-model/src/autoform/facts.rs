use super::{GeneratedChild, GeneratedObject, GeneratedValue};
use mdconf::{ChildTag, MdKind};

const OBJECT_PROPS: [&str; 7] = [
    "Hierarchical",
    "HierarchyType",
    "InformationRegisterPeriodicity",
    "WriteMode",
    "CodeLength",
    "DescriptionLength",
    "NumberLength",
];
const CHILD_PROPS: [&str; 2] = ["Use", "Type"];
const MAX_DEPTH: usize = 128;
const TAGS: [ChildTag; 16] = [
    ChildTag::Attribute,
    ChildTag::TabularSection,
    ChildTag::Dimension,
    ChildTag::Resource,
    ChildTag::EnumValue,
    ChildTag::Command,
    ChildTag::AddressingAttribute,
    ChildTag::AccountingFlag,
    ChildTag::ExtDimensionAccountingFlag,
    ChildTag::Column,
    ChildTag::URLTemplate,
    ChildTag::Method,
    ChildTag::Operation,
    ChildTag::Parameter,
    ChildTag::IntegrationServiceChannel,
    ChildTag::Function,
];

#[derive(Debug, PartialEq, Eq)]
pub enum FactWriteError<E> {
    Sink(E),
    TooDeep,
    Overflow,
}
#[derive(Debug, PartialEq, Eq)]
pub struct InvalidFacts;

pub struct GeneratedFacts {
    kind: MdKind,
    name: String,
    props: [Option<FactValue>; 7],
    children: Vec<ChildFacts>,
}
pub struct ChildFacts {
    tag: ChildTag,
    name: String,
    props: [Option<FactValue>; 2],
    boolean: bool,
    children: Vec<ChildFacts>,
}
impl GeneratedObject for GeneratedFacts {
    type Child<'a>
        = &'a ChildFacts
    where
        Self: 'a;
    fn kind(&self) -> MdKind {
        self.kind
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn prop(&self, name: &str) -> Option<GeneratedValue<'_>> {
        OBJECT_PROPS
            .iter()
            .position(|key| *key == name)
            .and_then(|i| self.props.get(i)?.as_ref().map(FactValue::view))
    }
    fn children(&self) -> impl Iterator<Item = Self::Child<'_>> {
        self.children.iter()
    }
}
impl GeneratedChild for &ChildFacts {
    type Child<'a>
        = &'a ChildFacts
    where
        Self: 'a;
    fn tag(&self) -> ChildTag {
        self.tag
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn prop(&self, name: &str) -> Option<GeneratedValue<'_>> {
        CHILD_PROPS
            .iter()
            .position(|key| *key == name)
            .and_then(|i| self.props.get(i)?.as_ref().map(FactValue::view))
    }
    fn is_boolean(&self) -> bool {
        self.boolean
    }
    fn children(&self) -> impl Iterator<Item = Self::Child<'_>> {
        self.children.iter()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FactValue {
    Text(String),
    Bool(bool),
    Other,
}
impl FactValue {
    fn view(&self) -> GeneratedValue<'_> {
        match self {
            Self::Text(text) => GeneratedValue::Text(text),
            Self::Bool(value) => GeneratedValue::Bool(*value),
            Self::Other => GeneratedValue::Other,
        }
    }
}

struct Writer<F>(F);
impl<E, F: FnMut(&[u8]) -> Result<(), E>> Writer<F> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), FactWriteError<E>> {
        (self.0)(bytes).map_err(FactWriteError::Sink)
    }
    fn count(&mut self, length: usize) -> Result<(), FactWriteError<E>> {
        self.put(
            &u64::try_from(length)
                .map_err(|_| FactWriteError::Overflow)?
                .to_le_bytes(),
        )
    }
    fn text(&mut self, text: &str) -> Result<(), FactWriteError<E>> {
        self.count(text.len())?;
        self.put(text.as_bytes())
    }
    fn value(&mut self, value: Option<GeneratedValue<'_>>) -> Result<(), FactWriteError<E>> {
        match value {
            None => self.put(&[0]),
            Some(GeneratedValue::Text(text)) => {
                self.put(&[1])?;
                self.text(text)
            }
            Some(GeneratedValue::Bool(false)) => self.put(&[2]),
            Some(GeneratedValue::Bool(true)) => self.put(&[3]),
            Some(GeneratedValue::Other) => self.put(&[4]),
        }
    }
    fn child(&mut self, child: impl GeneratedChild, depth: usize) -> Result<(), FactWriteError<E>> {
        if depth >= MAX_DEPTH {
            return Err(FactWriteError::TooDeep);
        }
        let tag = TAGS
            .iter()
            .position(|tag| *tag == child.tag())
            .ok_or(FactWriteError::Overflow)?;
        self.put(&[tag as u8])?;
        self.text(child.name())?;
        for key in CHILD_PROPS {
            self.value(child.prop(key))?;
        }
        self.put(&[u8::from(child.is_boolean())])?;
        self.count(child.children().count())?;
        for nested in child.children() {
            self.child(nested, depth + 1)?;
        }
        Ok(())
    }
}
pub fn encode<E>(
    object: &impl GeneratedObject,
    sink: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), FactWriteError<E>> {
    let mut out = Writer(sink);
    out.put(b"GOF1")?;
    out.text(object.kind().as_str())?;
    out.text(object.name())?;
    for key in OBJECT_PROPS {
        out.value(object.prop(key))?;
    }
    out.count(object.children().count())?;
    for child in object.children() {
        out.child(child, 0)?;
    }
    Ok(())
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], InvalidFacts> {
        let bytes = self.0.get(..length).ok_or(InvalidFacts)?;
        self.0 = self.0.get(length..).ok_or(InvalidFacts)?;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8, InvalidFacts> {
        self.take(1)?.first().copied().ok_or(InvalidFacts)
    }
    fn count(&mut self) -> Result<usize, InvalidFacts> {
        let bytes = self.take(8)?.try_into().map_err(|_| InvalidFacts)?;
        usize::try_from(u64::from_le_bytes(bytes)).map_err(|_| InvalidFacts)
    }
    fn text(&mut self) -> Result<&'a str, InvalidFacts> {
        let length = self.count()?;
        std::str::from_utf8(self.take(length)?).map_err(|_| InvalidFacts)
    }
    fn value(&mut self) -> Result<Option<FactValue>, InvalidFacts> {
        Ok(match self.byte()? {
            0 => None,
            1 => Some(FactValue::Text(self.text()?.to_owned())),
            2 => Some(FactValue::Bool(false)),
            3 => Some(FactValue::Bool(true)),
            4 => Some(FactValue::Other),
            _ => return Err(InvalidFacts),
        })
    }
    fn values<const N: usize>(&mut self) -> Result<[Option<FactValue>; N], InvalidFacts> {
        let mut values = std::array::from_fn(|_| None);
        for value in &mut values {
            *value = self.value()?;
        }
        Ok(values)
    }
    fn children(&mut self, depth: usize) -> Result<Vec<ChildFacts>, InvalidFacts> {
        let count = self.count()?;
        if count > self.0.len() / 20 || (count != 0 && depth >= MAX_DEPTH) {
            return Err(InvalidFacts);
        }
        let mut children = Vec::new();
        for _ in 0..count {
            let tag = *TAGS.get(usize::from(self.byte()?)).ok_or(InvalidFacts)?;
            let name = self.text()?.to_owned();
            let props = self.values()?;
            let boolean = match self.byte()? {
                0 => false,
                1 => true,
                _ => return Err(InvalidFacts),
            };
            if boolean && props.get(1) != Some(&Some(FactValue::Other)) {
                return Err(InvalidFacts);
            }
            let nested = self.children(depth + 1)?;
            children.push(ChildFacts {
                tag,
                name,
                props,
                boolean,
                children: nested,
            });
        }
        Ok(children)
    }
}
pub fn decode(bytes: &[u8]) -> Result<GeneratedFacts, InvalidFacts> {
    let mut input = Reader(bytes);
    if input.take(4)? != b"GOF1" {
        return Err(InvalidFacts);
    }
    let kind = MdKind::from_name(input.text()?).ok_or(InvalidFacts)?;
    let name = input.text()?.to_owned();
    let props = input.values()?;
    let children = input.children(0)?;
    if !input.0.is_empty() {
        return Err(InvalidFacts);
    }
    Ok(GeneratedFacts {
        kind,
        name,
        props,
        children,
    })
}
