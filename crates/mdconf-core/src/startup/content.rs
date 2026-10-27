use super::{
    ManagedDiagnostics, ManagedValueContext, Tree,
    defined_types::DefinedTypeLookup,
    index::text,
    properties::{NO_PROPS, PropertyInput, follow},
    values::{append, empty},
};
use crate::{Child, GeneratedType, LocalString, MdKind, Prop};
use brace::{Guid, Node};
use mdschema::{StaticValue, static_schema};
use memory_budget::{BudgetError, BudgetedVec, Category, Charged};
const CATEGORY: Category = Category::Metadata;
#[derive(Debug)]
pub struct ManagedObjectContents {
    pub props: Vec<Prop>,
    pub synonym: LocalString,
    pub comment: String,
    pub generated_types: Vec<GeneratedType>,
    pub this_node: Option<Guid>,
    pub children: Vec<Child>,
}
#[derive(Debug)]
pub struct ManagedDecodedContent {
    pub values: Charged<ManagedObjectContents>,
    pub diagnostics: ManagedDiagnostics,
}
#[derive(Clone, Copy)]
struct Task<'a> {
    tree: Tree<'a>,
    spec: &'static StaticValue,
    parent: Option<&'static str>,
}
struct Frame<'a> {
    tasks: BudgetedVec<Task<'a>>,
    next: usize,
    output: Charged<Vec<Child>>,
    pending: Option<Charged<Child>>,
}
impl ManagedValueContext<'_> {
    pub fn object_content(
        &self,
        kind: MdKind,
        name: &str,
        raw: &Node,
    ) -> Result<ManagedDecodedContent, BudgetError> {
        self.object_content_with_sets(kind, name, raw, None)
    }
    pub(super) fn object_content_with_sets(
        &self,
        kind: MdKind,
        name: &str,
        raw: &Node,
        sets: Option<&dyn DefinedTypeLookup>,
    ) -> Result<ManagedDecodedContent, BudgetError> {
        let budget = self.memory_budget();
        let props = self.object_properties_with_sets(kind, name, raw, sets)?;
        let mut diagnostics = props.diagnostics;
        let tree = Tree::new(kind.as_str(), raw);
        let spec = super::kind_spec(kind.as_str()).filter(|spec| spec.header().is_some());
        let generated = self.generated(
            tree,
            spec.and_then(|s| s.field("internal_info")),
            kind.as_str(),
            false,
        )?;
        let this_node = spec
            .and_then(|s| s.field("this_node"))
            .and_then(|path| follow(tree, path))
            .and_then(Tree::as_guid);
        let co_type = text(budget, &[kind.as_str(), "ChildObjects"])?;
        let tasks = if spec.is_some() {
            self.child_tasks(
                tree,
                spec.and_then(|s| s.field("children")),
                spec.and_then(|s| s.field("subobjects")),
                co_type.as_str(),
                None,
                &mut diagnostics,
            )?
        } else {
            BudgetedVec::new(budget, CATEGORY)?
        };
        let children = self.child_graph(tasks, (kind.as_str(), name), sets, &mut diagnostics)?;
        Ok(ManagedDecodedContent {
            values: props.values.try_zip(generated)?.try_zip(children)?.map(
                |((p, generated_types), children)| ManagedObjectContents {
                    props: p.props,
                    synonym: p.synonym,
                    comment: p.comment,
                    generated_types,
                    this_node,
                    children,
                },
            ),
            diagnostics,
        })
    }
    fn generated(
        &self,
        tree: Tree<'_>,
        info: Option<&'static StaticValue>,
        kind: &str,
        child: bool,
    ) -> Result<Charged<Vec<GeneratedType>>, BudgetError> {
        let budget = self.memory_budget();
        let mut cats = BudgetedVec::<(&str, &StaticValue)>::new(budget, CATEGORY)?;
        for entry in info.and_then(StaticValue::as_array).unwrap_or_default() {
            let Some(category) = entry.get("category").and_then(StaticValue::as_str) else {
                continue;
            };
            if let Some(slot) = cats.iter_mut().find(|(key, _)| *key == category) {
                slot.1 = entry;
            } else {
                cats.try_push((category, entry))?;
            }
        }
        let mut order = BudgetedVec::new(budget, CATEGORY)?;
        if child {
            if kind == "TabularSection" {
                order.try_push("TabularSection")?;
                order.try_push("TabularSectionRow")?;
            }
        } else {
            for item in static_schema()
                .table("gen.json")
                .and_then(|v| v.get("gentypes"))
                .and_then(|v| v.get(kind))
                .and_then(StaticValue::as_array)
                .unwrap_or_default()
            {
                if let Some(category) = item.as_str() {
                    order.try_push(category)?;
                }
            }
        }
        let mut out = BudgetedVec::try_with_capacity(
            budget,
            CATEGORY,
            cats.len()
                .checked_add(order.len())
                .ok_or(BudgetError::Overflow)?,
        )?
        .into_charged_vec();
        for category in order
            .iter()
            .copied()
            .chain(cats.iter().map(|(c, _)| *c).filter(|c| !order.contains(c)))
        {
            let Some((_, entry)) = cats.iter().find(|(c, _)| *c == category) else {
                continue;
            };
            let guid = |key| {
                entry
                    .get(key)
                    .and_then(|path| follow(tree, path))
                    .and_then(Tree::as_guid)
            };
            let (Some(type_id), Some(value_id)) = (guid("type_id"), guid("value_id")) else {
                continue;
            };
            out = append(
                out,
                text(budget, &[category])?
                    .into_string()?
                    .map(|category| GeneratedType {
                        category,
                        type_id,
                        value_id,
                    }),
            )?;
        }
        Ok(out)
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Borrowed child source, schema, owner context and diagnostics remain separate"
    )]
    fn child_tasks<'a>(
        &self,
        tree: Tree<'a>,
        specs: Option<&'static StaticValue>,
        subobjects: Option<&'static StaticValue>,
        co_type: &str,
        parent: Option<&'static str>,
        diagnostics: &mut ManagedDiagnostics,
    ) -> Result<BudgetedVec<Task<'a>>, BudgetError> {
        let mut tasks = BudgetedVec::new(self.memory_budget(), CATEGORY)?;
        let specs = specs.and_then(StaticValue::as_object).unwrap_or_default();
        for row in static_schema()
            .table("xdto_md.json")
            .and_then(|v| v.get(co_type))
            .and_then(StaticValue::as_array)
            .unwrap_or_default()
        {
            let Some(tag) = row
                .as_array()
                .and_then(|row| row.first())
                .and_then(StaticValue::as_str)
            else {
                continue;
            };
            if let Some((class, spec)) = specs
                .iter()
                .rev()
                .find(|(_, s)| s.get("tag").and_then(StaticValue::as_str) == Some(tag))
            {
                if let Some(items) = tree.find_collection(class) {
                    for tree in items.iter() {
                        if crate::objects::decode::child_tag(tag).is_some()
                            && spec.get("header").and_then(StaticValue::as_array).is_some()
                        {
                            tasks.try_push(Task { tree, spec, parent })?;
                        } else {
                            diagnostics.miss(&["unknown:", co_type, ".", tag])?;
                        }
                    }
                }
            } else if matches!(tag, "Form" | "Template")
                && subobjects.and_then(|v| v.get(tag)).is_none()
            {
                diagnostics.miss(&["unknown:", co_type, ".", tag])?;
            }
        }
        Ok(tasks)
    }
    fn child_graph<'a>(
        &self,
        tasks: BudgetedVec<Task<'a>>,
        owner: (&str, &str),
        sets: Option<&dyn DefinedTypeLookup>,
        diagnostics: &mut ManagedDiagnostics,
    ) -> Result<Charged<Vec<Child>>, BudgetError> {
        let budget = self.memory_budget();
        let frame = |tasks: BudgetedVec<Task<'a>>| -> Result<Frame<'a>, BudgetError> {
            Ok(Frame {
                output: BudgetedVec::try_with_capacity(budget, CATEGORY, tasks.len())?
                    .into_charged_vec(),
                tasks,
                next: 0,
                pending: None,
            })
        };
        let mut stack = BudgetedVec::new(budget, CATEGORY)?;
        stack.try_push(frame(tasks)?)?;
        while let Some(mut current) = stack.pop() {
            if let Some(task) = current.tasks.get(current.next).copied() {
                current.next = current.next.checked_add(1).ok_or(BudgetError::Overflow)?;
                let tag = task
                    .spec
                    .get("tag")
                    .and_then(StaticValue::as_str)
                    .ok_or(BudgetError::Overflow)?;
                let model_tag =
                    crate::objects::decode::child_tag(tag).ok_or(BudgetError::Overflow)?;
                let mut header = BudgetedVec::new(budget, CATEGORY)?;
                for item in task
                    .spec
                    .get("header")
                    .and_then(StaticValue::as_array)
                    .ok_or(BudgetError::Overflow)?
                {
                    header.try_push(
                        usize::try_from(item.as_u64().ok_or(BudgetError::Overflow)?)
                            .map_err(|_| BudgetError::Overflow)?,
                    )?;
                }
                let id = task
                    .spec
                    .get("uuid")
                    .and_then(|path| follow(task.tree, path))
                    .and_then(Tree::as_guid)
                    .unwrap_or_default();
                let context = match task.parent {
                    Some(parent) => text(budget, &[owner.0, ".", parent, ".", tag])?,
                    None => text(budget, &[owner.0, ".", tag])?,
                };
                let props_type = text(budget, &[tag, "Properties"])?;
                let p = self.properties(
                    PropertyInput {
                        tree: task.tree,
                        header: &header,
                        props_type: props_type.as_str(),
                        tag,
                        context: Some(context.as_str()),
                        specs: task.spec.get("props").unwrap_or(&NO_PROPS),
                        owner: Some(owner),
                        special: None,
                    },
                    sets,
                )?;
                diagnostics.merge(p.diagnostics)?;
                let name = task
                    .tree
                    .get_path(header.iter().copied().chain([2]))
                    .and_then(Tree::text)
                    .unwrap_or_default();
                let generated =
                    self.generated(task.tree, task.spec.get("internal_info"), tag, true)?;
                let child = p
                    .values
                    .try_zip(text(budget, &[name])?.into_string()?)?
                    .try_zip(generated)?
                    .map(|((p, name), generated_types)| Child {
                        tag: model_tag,
                        id,
                        name,
                        synonym: p.synonym,
                        comment: p.comment,
                        props: p.props,
                        generated_types,
                        children: Vec::new(),
                        module: None,
                    });
                let co_type = text(budget, &[tag, "ChildObjects"])?;
                let nested = if tag == "TabularSection"
                    || task
                        .spec
                        .get("children")
                        .and_then(StaticValue::as_object)
                        .is_some_and(|v| !v.is_empty())
                {
                    self.child_tasks(
                        task.tree,
                        task.spec.get("children"),
                        None,
                        co_type.as_str(),
                        Some(tag),
                        diagnostics,
                    )?
                } else {
                    BudgetedVec::new(budget, CATEGORY)?
                };
                current.pending = Some(child);
                stack.try_push(current)?;
                stack.try_push(frame(nested)?)?;
            } else if let Some(mut parent) = stack.pop() {
                let child = parent
                    .pending
                    .take()
                    .ok_or(BudgetError::Overflow)?
                    .try_zip(current.output)?
                    .map(|(mut child, children)| {
                        child.children = children;
                        child
                    });
                parent.output = append(parent.output, child)?;
                stack.try_push(parent)?;
            } else {
                return Ok(current.output);
            }
        }
        empty(budget, Vec::new())
    }
}
