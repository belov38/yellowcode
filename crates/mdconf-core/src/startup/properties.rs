use super::{
    ManagedHead, ManagedObjects, ManagedPropSource, ManagedPropertyPlan, ManagedValueContext, Tree,
    index::text,
    values::{append, empty},
};
use crate::{LocalString, MdKind, Prop, TypeDescription, TypeRef, TypedValue, Value};
use brace::Node;
use mdschema::{Codec, StaticPropSpec, StaticValue};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget};
const CATEGORY: Category = Category::Metadata;
pub(super) static NO_PROPS: StaticValue = StaticValue::Object(&[]);
#[derive(Debug)]
pub struct ManagedDiagnostics {
    entries: BudgetedVec<(BudgetedText, u32)>,
    budget: MemoryBudget,
}
impl ManagedDiagnostics {
    pub fn new(budget: &MemoryBudget) -> Result<Self, BudgetError> {
        Ok(Self {
            entries: BudgetedVec::new(budget, CATEGORY)?,
            budget: budget.clone(),
        })
    }
    pub(super) fn memory_budget(&self) -> &MemoryBudget {
        &self.budget
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, u32)> {
        self.entries
            .iter()
            .map(|(key, count)| (key.as_str(), *count))
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn count(&self, parts: &[&str]) -> u32 {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        self.entries
            .iter()
            .find(|(key, _)| {
                let mut rest = key.as_str();
                rest.len() == len
                    && parts.iter().all(|p| match rest.strip_prefix(p) {
                        Some(r) => {
                            rest = r;
                            true
                        }
                        None => false,
                    })
            })
            .map_or(0, |(_, count)| *count)
    }
    pub fn miss(&mut self, parts: &[&str]) -> Result<(), BudgetError> {
        let key = text(&self.budget, parts)?;
        self.insert(key, 1)
    }
    pub(super) fn miss_count(&mut self, parts: &[&str], count: u32) -> Result<(), BudgetError> {
        self.insert(text(&self.budget, parts)?, count)
    }
    pub(super) fn merge(&mut self, mut other: Self) -> Result<(), BudgetError> {
        if !self.budget.same_scope(&other.budget) {
            return Err(BudgetError::Overflow);
        }
        while let Some((key, count)) = other.entries.pop() {
            self.insert(key, count)?;
        }
        Ok(())
    }
    fn insert(&mut self, key: BudgetedText, count: u32) -> Result<(), BudgetError> {
        match self
            .entries
            .binary_search_by(|(existing, _)| existing.as_str().cmp(key.as_str()))
        {
            Ok(at) => {
                let entry = self.entries.get_mut(at).ok_or(BudgetError::Overflow)?;
                entry.1 = entry.1.checked_add(count).ok_or(BudgetError::Overflow)?;
            }
            Err(at) => {
                self.entries.try_push((key, count))?;
                self.entries
                    .get_mut(at..)
                    .ok_or(BudgetError::Overflow)?
                    .rotate_right(1);
            }
        }
        Ok(())
    }
}
#[derive(Debug)]
pub struct ManagedPropertyValues {
    pub props: Vec<Prop>,
    pub synonym: LocalString,
    pub comment: String,
}
#[derive(Debug)]
pub struct ManagedDecodedProperties {
    pub values: Charged<ManagedPropertyValues>,
    pub diagnostics: ManagedDiagnostics,
}
pub(super) struct PropertyInput<'a> {
    pub tree: Tree<'a>,
    pub header: &'a [usize],
    pub props_type: &'a str,
    pub tag: &'a str,
    pub context: Option<&'a str>,
    pub specs: &'static StaticValue,
    pub owner: Option<(&'a str, &'a str)>,
    pub special: Option<(&'a str, &'a Node)>,
}
fn with_node<T>(
    tree: Tree<'_>,
    budget: &MemoryBudget,
    operation: impl FnOnce(&Node) -> Result<T, BudgetError>,
) -> Result<T, BudgetError> {
    if let Some(node) = tree.raw_node() {
        operation(node)
    } else {
        let node = tree.to_node(budget)?;
        operation(&node)
    }
}
pub(super) fn follow<'a>(mut tree: Tree<'a>, path: &StaticValue) -> Option<Tree<'a>> {
    for item in path.as_array()? {
        tree = tree.get(&[usize::try_from(item.as_u64()?).ok()?])?
    }
    Some(tree)
}
fn cut(header: &[usize], by: usize) -> &[usize] {
    header
        .get(..header.len().saturating_sub(by))
        .unwrap_or_default()
}
fn selected<'a>(input: &PropertyInput<'a>, spec: StaticPropSpec, fd: bool) -> Option<Tree<'a>> {
    if fd {
        return follow(input.tree.get(cut(input.header, 2))?, spec.field("fd")?);
    }
    if let Some(path) = spec.field("path") {
        return follow(input.tree, path);
    }
    let (path, by) = if let Some(path) = spec.field("hp") {
        (path, 1)
    } else if let Some(path) = spec.field("hpp") {
        (path, 2)
    } else {
        (spec.field("rel_header")?, 1)
    };
    follow(input.tree.get(cut(input.header, by))?, path)
}
fn field_desc(tree: Option<Tree<'_>>) -> bool {
    tree.is_some_and(|tree| {
        tree.as_list().is_some_and(|list| list.len() > 20)
            && tree.get(&[1, 2, 0]).and_then(Tree::text) == Some("Pattern")
    })
}
fn normalize_string(value: &mut String) {
    let mut offset = 0;
    while let Some(at) = value.get(offset..).and_then(|s| s.find("\r\n")) {
        let at = offset + at;
        value.remove(at);
        offset = at + 1;
    }
}
fn description(
    budget: &MemoryBudget,
    value: Charged<Value>,
) -> Result<Charged<Value>, BudgetError> {
    let normalized = value.map(|mut value| {
        let result: Result<(), BudgetError> = (|| {
            let mut stack = BudgetedVec::new(budget, CATEGORY)?;
            stack.try_push(&mut value)?;
            while let Some(value) = stack.pop() {
                match value {
                    Value::Text(text) => normalize_string(text),
                    Value::LStr(text) => {
                        for (_, text) in &mut text.0 {
                            normalize_string(text)
                        }
                    }
                    Value::StandardAttributes(attrs) => {
                        for attr in attrs {
                            for prop in &mut attr.props {
                                stack.try_push(&mut prop.value)?
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        })();
        (value, result)
    });
    normalized.1?;
    Ok(normalized.map(|(value, _)| value))
}
impl ManagedValueContext<'_> {
    pub fn object_properties(
        &self,
        kind: MdKind,
        name: &str,
        raw: &Node,
    ) -> Result<ManagedDecodedProperties, BudgetError> {
        self.object_properties_with_sets(kind, name, raw, None)
    }
    pub(super) fn object_properties_with_sets(
        &self,
        kind: MdKind,
        name: &str,
        raw: &Node,
        sets: Option<&ManagedObjects>,
    ) -> Result<ManagedDecodedProperties, BudgetError> {
        let Some(spec) = super::kind_spec(kind.as_str()) else {
            return self.no_properties();
        };
        let Some(header) = spec.header() else {
            return self.no_properties();
        };
        let mut path =
            BudgetedVec::try_with_capacity(self.memory_budget(), CATEGORY, header.len())?;
        for index in header.iter() {
            path.try_push(index)?
        }
        let props_type = text(self.memory_budget(), &[kind.as_str(), "Properties"])?;
        self.properties(
            PropertyInput {
                tree: Tree::new(kind.as_str(), raw),
                header: &path,
                props_type: props_type.as_str(),
                tag: kind.as_str(),
                context: None,
                specs: spec.field("props").unwrap_or(&NO_PROPS),
                owner: Some((kind.as_str(), name)),
                special: Some((kind.as_str(), raw)),
            },
            sets,
        )
    }
    pub fn subobject_properties(
        &self,
        kind: crate::SubKind,
        id: brace::Guid,
        raw: &Node,
    ) -> Result<ManagedDecodedProperties, BudgetError> {
        let kind = match kind {
            crate::SubKind::Form => "Form",
            crate::SubKind::Template => "Template",
        };
        let Some(spec) = super::kind_spec(kind) else {
            return self.no_properties();
        };
        let stored = self
            .index
            .sub_hdr
            .get(&id)
            .filter(|header| !header.is_empty());
        let mut path = BudgetedVec::new(self.memory_budget(), CATEGORY)?;
        if stored.is_none() {
            let Some(header) = spec.header() else {
                return self.no_properties();
            };
            for index in header.iter() {
                path.try_push(index)?;
            }
        }
        let header = stored.map(|path| path.as_slice()).unwrap_or(&path);
        let props_type = text(self.memory_budget(), &[kind, "Properties"])?;
        self.properties_retained(
            PropertyInput {
                tree: Tree::new(kind, raw),
                header,
                props_type: props_type.as_str(),
                tag: kind,
                context: None,
                specs: spec.field("props").unwrap_or(&NO_PROPS),
                owner: None,
                special: None,
            },
            None,
            false,
        )
    }
    fn no_properties(&self) -> Result<ManagedDecodedProperties, BudgetError> {
        Ok(ManagedDecodedProperties {
            values: empty(
                self.memory_budget(),
                ManagedPropertyValues {
                    props: Vec::new(),
                    synonym: LocalString::default(),
                    comment: String::new(),
                },
            )?,
            diagnostics: ManagedDiagnostics::new(self.memory_budget())?,
        })
    }
    pub(super) fn properties(
        &self,
        input: PropertyInput<'_>,
        sets: Option<&ManagedObjects>,
    ) -> Result<ManagedDecodedProperties, BudgetError> {
        self.properties_retained(input, sets, true)
    }
    fn properties_retained(
        &self,
        input: PropertyInput<'_>,
        sets: Option<&ManagedObjects>,
        retain_header: bool,
    ) -> Result<ManagedDecodedProperties, BudgetError> {
        let budget = self.memory_budget();
        let plan = ManagedPropertyPlan::build(
            budget,
            input.props_type,
            input.tag,
            input.context,
            input.specs,
        )?;
        self.properties_with_plan(input, sets, retain_header, plan)
    }
    pub(super) fn properties_with_plan(
        &self,
        input: PropertyInput<'_>,
        sets: Option<&ManagedObjects>,
        retain_header: bool,
        plan: ManagedPropertyPlan,
    ) -> Result<ManagedDecodedProperties, BudgetError> {
        let budget = self.memory_budget();
        let mut diagnostics = ManagedDiagnostics::new(budget)?;
        let mut props =
            BudgetedVec::try_with_capacity(budget, CATEGORY, plan.len())?.into_charged_vec();
        let mut synonym = empty(budget, LocalString::default())?;
        let mut comment = empty(budget, String::new())?;
        for (name, source) in plan.iter() {
            let (spec, fd) = match source {
                ManagedPropSource::Head(head) => {
                    let at = match head {
                        ManagedHead::Name => 2,
                        ManagedHead::Synonym => 3,
                        ManagedHead::Comment => 4,
                    };
                    let raw = input
                        .tree
                        .get_path(input.header.iter().copied().chain([at]));
                    let (codec, value) = match head {
                        ManagedHead::Name => (
                            Codec::Str,
                            text(budget, &[raw.and_then(Tree::text).unwrap_or_default()])?
                                .into_string()?
                                .map(Value::Text),
                        ),
                        ManagedHead::Synonym => {
                            let Some(raw) = raw else { continue };
                            let first = with_node(raw, budget, |node| self.local_string(node))?;
                            let Some(first) = first else { continue };
                            let value = description(budget, first.map(Value::LStr))?;
                            if retain_header {
                                let duplicate =
                                    with_node(raw, budget, |node| self.local_string(node))?
                                        .ok_or(BudgetError::Overflow)?;
                                synonym = duplicate.map(|mut local| {
                                    for (_, text) in &mut local.0 {
                                        normalize_string(text);
                                    }
                                    local
                                });
                            }
                            (Codec::LStr, value)
                        }
                        ManagedHead::Comment => {
                            if raw.is_some_and(Tree::is_list) {
                                diagnostics.miss(&["repr:", input.props_type, ".Comment"])?
                            }
                            let repr = match raw {
                                Some(raw) => with_node(raw, budget, |node| self.python_text(node))?,
                                None => text(budget, &[""])?,
                            };
                            if retain_header {
                                comment = super::values::description_text(budget, repr.as_str())?
                                    .into_string()?;
                            }
                            (
                                Codec::Str,
                                super::values::description_text(budget, repr.as_str())?
                                    .into_string()?
                                    .map(Value::Text),
                            )
                        }
                    };
                    props = append(
                        props,
                        text(budget, &[name])?
                            .into_string()?
                            .try_zip(value)?
                            .map(|(name, value)| Prop { name, codec, value }),
                    )?;
                    continue;
                }
                ManagedPropSource::Spec(spec) => (spec, false),
                ManagedPropSource::Fallback { fd, hp } => {
                    let fd = fd.filter(|_| {
                        input.header.len() >= 3 && field_desc(input.tree.get(cut(input.header, 2)))
                    });
                    if let Some(fd) = fd {
                        (fd, true)
                    } else if let Some(hp) = hp {
                        (hp, false)
                    } else {
                        diagnostics.miss(&["unknown:", input.props_type, ".", name])?;
                        continue;
                    }
                }
            };
            let raw = selected(&input, spec, fd);
            let masked =
                if let Some(condition) = spec.field("false_if").and_then(StaticValue::as_array) {
                    let expected = condition.get(1).and_then(StaticValue::as_str);
                    match condition.first().and_then(|path| follow(input.tree, path)) {
                        Some(node) => {
                            let actual = with_node(node, budget, |node| self.python_text(node))?;
                            Some(actual.as_str()) == expected
                        }
                        None => expected == Some("None"),
                    }
                } else {
                    false
                };
            if masked {
                props = append(
                    props,
                    text(budget, &[name])?
                        .into_string()?
                        .try_zip(empty(budget, Value::Bool(false))?)?
                        .map(|(name, value)| Prop {
                            name,
                            codec: spec.codec(),
                            value,
                        }),
                )?;
                continue;
            }
            if matches!(name, "StandardAttributes" | "StandardTabularSections")
                && raw.is_some_and(|raw| {
                    raw.as_list().is_some_and(|list| {
                        list.len() == 1 && list.get(0).and_then(Tree::text) == Some("0")
                    })
                })
            {
                continue;
            }
            let path2 = if let Some(path) = spec.field("path2") {
                Some(match follow(input.tree, path) {
                    Some(node) => with_node(node, budget, |node| self.python_text(node))?,
                    None => text(budget, &[""])?,
                })
            } else {
                None
            };
            let value = match raw {
                Some(raw) => with_node(raw, budget, |node| {
                    self.property_value(
                        name,
                        spec,
                        node,
                        input.owner,
                        path2.as_ref().map(BudgetedText::as_str),
                    )
                })?,
                None => None,
            };
            let Some(value) = value else {
                diagnostics.miss(&["fail:", input.props_type, ".", name])?;
                continue;
            };
            if matches!(spec.codec(), Codec::Int | Codec::Str | Codec::Event)
                && raw.is_some_and(Tree::is_list)
            {
                diagnostics.miss(&["repr:", input.props_type, ".", name])?
            }
            let mut value = description(budget, value)?;
            if name == "FillValue" {
                let pattern = input
                    .tree
                    .get_path(cut(input.header, 1).iter().copied().chain([2]));
                if let Some(pattern) = pattern {
                    let description = with_node(pattern, budget, |node| self.type_desc(node))?;
                    if let Some(description) = description {
                        let wanted = match &*value {
                            Value::Typed(TypedValue::Str(_)) => Some("xs:string"),
                            Value::Typed(TypedValue::Number(_)) => Some("xs:decimal"),
                            Value::Typed(TypedValue::Bool(_)) => Some("xs:boolean"),
                            Value::Typed(TypedValue::Date(_)) => Some("xs:dateTime"),
                            _ => None,
                        };
                        if let Some(wanted) = wanted
                            && !self.allows(&description, sets, wanted)?
                        {
                            drop(value);
                            value = empty(budget, Value::Typed(TypedValue::Nil))?
                        }
                    }
                }
            }
            if let Some((kind, raw)) = input.special
                && let Some(special) = super::special_prop_text(kind, name, raw)
            {
                drop(value);
                value = text(budget, &[special])?.into_string()?.map(Value::Text);
            }
            props = append(
                props,
                text(budget, &[name])?
                    .into_string()?
                    .try_zip(value)?
                    .map(|(name, value)| Prop {
                        name,
                        codec: spec.codec(),
                        value,
                    }),
            )?;
        }
        Ok(ManagedDecodedProperties {
            values: props
                .try_zip(synonym)?
                .try_zip(comment)?
                .map(|((props, synonym), comment)| ManagedPropertyValues {
                    props,
                    synonym,
                    comment,
                }),
            diagnostics,
        })
    }
    fn property_value(
        &self,
        name: &str,
        spec: StaticPropSpec,
        node: &Node,
        owner: Option<(&str, &str)>,
        path2: Option<&str>,
    ) -> Result<Option<Charged<Value>>, BudgetError> {
        if name == "StandardTabularSections" && spec.codec() == Codec::Unknown {
            return match owner {
                Some(owner) => self.standard_tabular_sections(owner, node),
                None => Ok(None),
            };
        }
        match spec.codec() {
            Codec::MdList
            | Codec::FoContent
            | Codec::FieldList
            | Codec::Picture
            | Codec::Purposes
            | Codec::CaContent
            | Codec::CpLinks
            | Codec::LinkByType
            | Codec::CParams => self.collection_value(spec.codec(), name, node, owner),
            Codec::StdAttrs
            | Codec::Characteristics
            | Codec::StyleValue
            | Codec::XdtoType
            | Codec::XdtoPackages => self.bag_value(spec, name, node, owner),
            _ => self.leaf_value(spec, node, path2),
        }
    }
    fn set_target<'a>(
        &self,
        sets: &'a ManagedObjects,
        reference: &TypeRef,
    ) -> Option<&'a TypeDescription> {
        let name = reference.qname.strip_prefix("cfg:DefinedType.")?;
        let by_id = reference
            .type_id
            .and_then(|id| self.index.type_owners.get(&id))
            .and_then(|owner| sets.object(owner.object))
            .filter(|o| o.kind == MdKind::DefinedType)
            .and_then(|o| match o.prop("Type") {
                Some(Value::Type(ty)) => Some(ty),
                _ => None,
            });
        by_id.or_else(|| {
            sets.as_slice().iter().rev().find_map(|object| {
                if object.kind != MdKind::DefinedType || object.name != name {
                    return None;
                }
                match object.prop("Type") {
                    Some(Value::Type(ty)) => Some(ty),
                    _ => None,
                }
            })
        })
    }
    fn allows(
        &self,
        description: &TypeDescription,
        sets: Option<&ManagedObjects>,
        wanted: &str,
    ) -> Result<bool, BudgetError> {
        struct Frame<'a> {
            refs: &'a [TypeRef],
            next: usize,
            owner: Option<&'a str>,
        }
        let mut stack = BudgetedVec::new(self.memory_budget(), CATEGORY)?;
        let mut done = BudgetedVec::new(self.memory_budget(), CATEGORY)?;
        stack.try_push(Frame {
            refs: &description.types,
            next: 0,
            owner: None,
        })?;
        while let Some(frame) = stack.last_mut() {
            let Some(reference) = frame.refs.get(frame.next) else {
                if let Some(frame) = stack.pop()
                    && let Some(owner) = frame.owner
                {
                    done.try_push(owner)?
                }
                continue;
            };
            frame.next = frame.next.checked_add(1).ok_or(BudgetError::Overflow)?;
            if done.contains(&reference.qname.as_str()) {
                continue;
            }
            let target = sets
                .and_then(|sets| self.set_target(sets, reference))
                .filter(|ty| !ty.types.is_empty());
            if let Some(target) = target
                && !stack
                    .iter()
                    .any(|frame| frame.owner == Some(reference.qname.as_str()))
            {
                stack.try_push(Frame {
                    refs: &target.types,
                    next: 0,
                    owner: Some(&reference.qname),
                })?;
            } else if !reference.set && reference.qname == wanted {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
