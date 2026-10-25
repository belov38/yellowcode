use super::{
    ManagedDiagnostics, ManagedPropertyPlan, ManagedValueContext, Tree,
    index::text,
    properties::PropertyInput,
    values::{append, empty},
};
use crate::objects::recalc::{DIM_HEADER, DIM_UUID, GENERATED, RECALC_DIM_CLASS, RECALC_HEADER};
use crate::{GeneratedType, MdRef, RecalcDimension, Recalculation, Value};
use brace::{Guid, Node};
use mdschema::StaticValue as V;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged};
const CATEGORY: Category = Category::Metadata;
static RECALC_PROPS: V = V::Object(&[(
    "DataLockControlMode",
    V::Object(&[
        ("codec", V::String("enum")),
        ("path", V::Array(&[V::Integer(1), V::Integer(8)])),
        (
            "map",
            V::Object(&[
                ("0", V::String("Automatic")),
                ("1", V::String("Managed")),
                ("2", V::String("AutomaticAndManaged")),
            ]),
        ),
    ]),
)]);
static DIM_PROPS: V = V::Object(&[
    (
        "RegisterDimension",
        V::Object(&[
            ("codec", V::String("ref")),
            ("path", V::Array(&[V::Integer(0), V::Integer(2)])),
        ]),
    ),
    (
        "LeadingRegisterData",
        V::Object(&[
            ("codec", V::String("mdlist")),
            ("path", V::Array(&[V::Integer(0), V::Integer(3)])),
        ]),
    ),
]);
#[derive(Debug)]
pub struct ManagedDecodedRecalculation {
    pub values: Charged<Recalculation>,
    pub diagnostics: ManagedDiagnostics,
}
impl ManagedValueContext<'_> {
    pub fn recalculation(
        &self,
        owner: (&str, &str),
        id: Guid,
        raw: &Node,
    ) -> Result<Option<ManagedDecodedRecalculation>, BudgetError> {
        if raw.get(&RECALC_HEADER).and_then(Node::as_list).is_none() {
            return Ok(None);
        }
        let budget = self.memory_budget();
        let tree = Tree::new("Recalculation", raw);
        let p = self.properties_with_plan(
            PropertyInput {
                tree,
                header: &RECALC_HEADER,
                props_type: "RecalculationProperties",
                tag: "Recalculation",
                context: None,
                specs: &RECALC_PROPS,
                owner: Some(owner),
                special: None,
            },
            None,
            true,
            ManagedPropertyPlan::explicit(budget, &RECALC_PROPS)?,
        )?;
        let mut diagnostics = p.diagnostics;
        let name = text(
            budget,
            &[tree
                .get_path(RECALC_HEADER.into_iter().chain([2]))
                .and_then(Tree::text)
                .unwrap_or_default()],
        )?
        .into_string()?;
        let mut generated =
            BudgetedVec::try_with_capacity(budget, CATEGORY, GENERATED.len())?.into_charged_vec();
        for (category, type_path, value_path) in GENERATED {
            let (Some(type_id), Some(value_id)) = (
                raw.get(&type_path).and_then(Node::as_guid),
                raw.get(&value_path).and_then(Node::as_guid),
            ) else {
                continue;
            };
            generated = append(
                generated,
                text(budget, &[category])?
                    .into_string()?
                    .map(|category| GeneratedType {
                        category,
                        type_id,
                        value_id,
                    }),
            )?;
        }
        let items = tree.find_collection(RECALC_DIM_CLASS);
        let mut dimensions =
            BudgetedVec::try_with_capacity(budget, CATEGORY, items.map_or(0, |v| v.len()))?
                .into_charged_vec();
        if let Some(items) = items {
            for item in items.iter() {
                if item.get(&DIM_HEADER).and_then(Tree::as_list).is_none() {
                    diagnostics.miss(&["unknown:RecalculationChildObjects.Dimension"])?;
                    continue;
                }
                let p = self.properties_with_plan(
                    PropertyInput {
                        tree: item,
                        header: &DIM_HEADER,
                        props_type: "RecalculationDimensionProperties",
                        tag: "Dimension",
                        context: None,
                        specs: &DIM_PROPS,
                        owner: Some(owner),
                        special: None,
                    },
                    None,
                    true,
                    ManagedPropertyPlan::explicit(budget, &DIM_PROPS)?,
                )?;
                diagnostics.merge(p.diagnostics)?;
                let find = |name| {
                    p.values
                        .props
                        .iter()
                        .find(|prop| prop.name == name)
                        .map(|prop| &prop.value)
                };
                let reference = match find("RegisterDimension") {
                    Some(Value::Ref(reference)) => self.copy_reference(reference)?,
                    _ => empty(budget, MdRef::default())?,
                };
                let leading = match find("LeadingRegisterData") {
                    Some(Value::RefList(values)) => values.as_slice(),
                    _ => &[],
                };
                let mut refs = BudgetedVec::try_with_capacity(budget, CATEGORY, leading.len())?
                    .into_charged_vec();
                for value in leading {
                    refs = append(refs, self.copy_reference(value)?)?;
                }
                let name = text(
                    budget,
                    &[item
                        .get_path(DIM_HEADER.into_iter().chain([2]))
                        .and_then(Tree::text)
                        .unwrap_or_default()],
                )?
                .into_string()?;
                let id = item
                    .get(&DIM_UUID)
                    .and_then(Tree::as_guid)
                    .unwrap_or_default();
                let value = p
                    .values
                    .try_zip(name)?
                    .try_zip(reference)?
                    .try_zip(refs)?
                    .map(|(((p, name), register_dimension), leading_register_data)| {
                        RecalcDimension {
                            id,
                            name,
                            synonym: p.synonym,
                            comment: p.comment,
                            props: p.props,
                            register_dimension,
                            leading_register_data,
                        }
                    });
                dimensions = append(dimensions, value)?;
            }
        }
        Ok(Some(ManagedDecodedRecalculation {
            values: p
                .values
                .try_zip(name)?
                .try_zip(generated)?
                .try_zip(dimensions)?
                .map(|(((p, name), generated_types), dimensions)| Recalculation {
                    id,
                    name,
                    synonym: p.synonym,
                    comment: p.comment,
                    props: p.props,
                    generated_types,
                    dimensions,
                    module: None,
                }),
            diagnostics,
        }))
    }
    fn copy_reference(&self, reference: &MdRef) -> Result<Charged<MdRef>, BudgetError> {
        Ok(text(self.memory_budget(), &[&reference.name])?
            .into_string()?
            .map(|name| MdRef {
                id: reference.id,
                name,
            }))
    }
}
