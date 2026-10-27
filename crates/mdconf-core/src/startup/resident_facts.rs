use super::{ManagedDiagnostics, SelectedObject};
use crate::{
    Child, ChildTag, GeneratedType, LocalString, MdKind, MdObject, Prop, Recalculation, SubKind,
    SubObject, TypeDescription, TypeRef, Value,
};
use brace::Guid;
use memory_budget::{BudgetError, BudgetedVec, Category, Charged, MemoryBudget};

#[derive(Debug)]
pub struct ObjectFacts {
    pub id: Guid,
    pub kind: MdKind,
    pub name: String,
    pub synonym: LocalString,
    pub generated_types: Vec<GeneratedType>,
    pub this_node: Option<Guid>,
    pub description: Option<TypeDescription>,
    pub managed_form: bool,
    pub children: BudgetedVec<ChildFacts>,
    pub forms: BudgetedVec<SubObjectFacts>,
    pub templates: BudgetedVec<SubObjectFacts>,
    pub recalculations: BudgetedVec<RecalculationFacts>,
}
#[derive(Debug)]
pub struct ChildFacts {
    pub id: Guid,
    pub tag: ChildTag,
    pub name: String,
    pub synonym: LocalString,
    pub generated_types: Vec<GeneratedType>,
    pub description: Option<TypeDescription>,
    pub children: BudgetedVec<ChildFacts>,
}
#[derive(Debug)]
pub struct SubObjectFacts {
    pub id: Guid,
    pub kind: SubKind,
    pub name: String,
    pub synonym: Option<LocalString>,
    pub managed_form: bool,
}
#[derive(Debug)]
pub struct RecalculationFacts {
    pub id: Guid,
    pub name: String,
    pub synonym: LocalString,
    pub generated_types: Vec<GeneratedType>,
}
#[derive(Debug)]
pub struct SelectedFacts {
    pub object: Charged<ObjectFacts>,
    pub diagnostics: ManagedDiagnostics,
}
impl SelectedObject {
    pub fn into_facts(self) -> Result<SelectedFacts, BudgetError> {
        let budget = self.object.reservation().budget().clone();
        let object = self
            .object
            .try_map(|object| ObjectFacts::project(object, &budget))
            .map_err(|error| *error)?;
        let retained = object.moved_bytes()?;
        let released = object
            .reservation()
            .bytes()
            .checked_sub(retained)
            .ok_or(BudgetError::Overflow)?;
        object
            .reservation()
            .shrink(released)
            .map_err(BudgetError::from)?;
        Ok(SelectedFacts {
            object,
            diagnostics: self.diagnostics,
        })
    }
}
fn managed(props: &[Prop]) -> bool {
    props
        .iter()
        .find(|p| p.name == "FormType")
        .is_some_and(|p| matches!(&p.value, Value::Text(t) if t == "Managed"))
}
fn description(props: Vec<Prop>) -> Option<TypeDescription> {
    props
        .into_iter()
        .find(|p| p.name == "Type")
        .and_then(|p| match p.value {
            Value::Type(value) => Some(value),
            _ => None,
        })
}
fn project_vec<T, U>(
    input: Vec<T>,
    budget: &MemoryBudget,
    mut project: impl FnMut(T) -> Result<U, BudgetError>,
) -> Result<BudgetedVec<U>, BudgetError> {
    let mut out = BudgetedVec::try_with_capacity(budget, Category::Metadata, input.len())?;
    for value in input {
        out.try_push(project(value)?)?;
    }
    Ok(out)
}
impl ObjectFacts {
    fn project(object: MdObject, budget: &MemoryBudget) -> Result<Self, BudgetError> {
        Ok(Self {
            id: object.id,
            kind: object.kind,
            name: object.name,
            synonym: object.synonym,
            generated_types: object.generated_types,
            this_node: object.this_node,
            managed_form: managed(&object.props),
            description: description(object.props),
            children: project_vec(object.children, budget, |child| {
                ChildFacts::project(child, budget)
            })?,
            forms: project_vec(object.forms, budget, SubObjectFacts::project)?,
            templates: project_vec(object.templates, budget, SubObjectFacts::project)?,
            recalculations: project_vec(object.recalculations, budget, |row| {
                Ok(RecalculationFacts::project(row))
            })?,
        })
    }
    fn moved_bytes(&self) -> Result<u64, BudgetError> {
        let mut bytes = Bytes(0);
        bytes.named(&self.name, &self.synonym, &self.generated_types)?;
        bytes.description(self.description.as_ref())?;
        for child in self.children.iter() {
            child.moved_bytes(&mut bytes)?;
        }
        for sub in self.forms.iter().chain(self.templates.iter()) {
            bytes.add(sub.name.capacity())?;
            if let Some(synonym) = &sub.synonym {
                bytes.synonym(synonym)?;
            }
        }
        for row in self.recalculations.iter() {
            bytes.named(&row.name, &row.synonym, &row.generated_types)?;
        }
        u64::try_from(bytes.0).map_err(|_| BudgetError::Overflow)
    }
}
impl ChildFacts {
    fn project(child: Child, budget: &MemoryBudget) -> Result<Self, BudgetError> {
        Ok(Self {
            id: child.id,
            tag: child.tag,
            name: child.name,
            synonym: child.synonym,
            generated_types: child.generated_types,
            description: description(child.props),
            children: project_vec(child.children, budget, |child| Self::project(child, budget))?,
        })
    }
    fn moved_bytes(&self, bytes: &mut Bytes) -> Result<(), BudgetError> {
        bytes.named(&self.name, &self.synonym, &self.generated_types)?;
        bytes.description(self.description.as_ref())?;
        for child in self.children.iter() {
            child.moved_bytes(bytes)?;
        }
        Ok(())
    }
}
impl SubObjectFacts {
    fn project(sub: SubObject) -> Result<Self, BudgetError> {
        let managed_form = managed(&sub.props);
        let synonym = sub
            .props
            .into_iter()
            .find(|p| p.name == "Synonym")
            .and_then(|p| match p.value {
                Value::LStr(value) => Some(value),
                _ => None,
            });
        Ok(Self {
            id: sub.id,
            kind: sub.kind,
            name: sub.name,
            synonym,
            managed_form,
        })
    }
}
impl RecalculationFacts {
    fn project(row: Recalculation) -> Self {
        Self {
            id: row.id,
            name: row.name,
            synonym: row.synonym,
            generated_types: row.generated_types,
        }
    }
}
struct Bytes(usize);
impl Bytes {
    fn add(&mut self, count: usize) -> Result<(), BudgetError> {
        self.0 = self.0.checked_add(count).ok_or(BudgetError::Overflow)?;
        Ok(())
    }
    fn vector<T>(&mut self, values: &Vec<T>) -> Result<(), BudgetError> {
        self.add(
            values
                .capacity()
                .checked_mul(std::mem::size_of::<T>())
                .ok_or(BudgetError::Overflow)?,
        )
    }
    fn synonym(&mut self, synonym: &LocalString) -> Result<(), BudgetError> {
        self.vector(&synonym.0)?;
        for (language, text) in &synonym.0 {
            self.add(language.capacity())?;
            self.add(text.capacity())?;
        }
        Ok(())
    }
    fn named(
        &mut self,
        name: &String,
        synonym: &LocalString,
        generated: &Vec<GeneratedType>,
    ) -> Result<(), BudgetError> {
        self.add(name.capacity())?;
        self.synonym(synonym)?;
        self.vector(generated)?;
        for item in generated {
            self.add(item.category.capacity())?;
        }
        Ok(())
    }
    fn description(&mut self, value: Option<&TypeDescription>) -> Result<(), BudgetError> {
        if let Some(value) = value {
            self.vector::<TypeRef>(&value.types)?;
            for reference in &value.types {
                self.add(reference.name.local.capacity())?;
                if let crate::TypeNamespace::Uri(uri) = &reference.name.namespace {
                    self.add(uri.capacity())?;
                }
            }
        }
        Ok(())
    }
}
