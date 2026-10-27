use super::{ModuleDescriptor, ResidentObject, index::text};
use crate::{MdKind, ModuleKind, Prop, Value};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget};

#[derive(Debug)]
pub struct ConfigurationIdentity {
    pub name: BudgetedText,
    pub synonym: Option<BudgetedVec<(BudgetedText, BudgetedText)>>,
    pub version: Option<BudgetedText>,
}
impl ConfigurationIdentity {
    pub fn from_properties(props: &Charged<Vec<Prop>>) -> Result<Self, BudgetError> {
        Self::build(props, props.reservation().budget())
    }
    pub(super) fn build(props: &[Prop], budget: &MemoryBudget) -> Result<Self, BudgetError> {
        let property = |name| {
            props
                .iter()
                .find(|prop| prop.name == name)
                .map(|prop| &prop.value)
        };
        let name = match property("Name") {
            Some(Value::Text(name)) => name.as_str(),
            _ => "",
        };
        let version = match property("Version") {
            Some(Value::Text(version)) => Some(text(budget, &[version])?),
            _ => None,
        };
        let synonym = match property("Synonym") {
            Some(Value::LStr(synonym)) => {
                let mut pairs =
                    BudgetedVec::try_with_capacity(budget, Category::Metadata, synonym.0.len())?;
                for (language, value) in &synonym.0 {
                    pairs.try_push((text(budget, &[language])?, text(budget, &[value])?))?;
                }
                Some(pairs)
            }
            _ => None,
        };
        Ok(Self {
            name: text(budget, &[name])?,
            synonym,
            version,
        })
    }
}
#[derive(Debug)]
pub struct ViewerCounters {
    pub kinds: BudgetedVec<(MdKind, usize)>,
    pub nested_subsystems: usize,
    pub modules: usize,
    pub form_modules: usize,
    pub module_bytes: u64,
    pub module_lines: u64,
    pub forms: usize,
    pub templates: usize,
}
impl ViewerCounters {
    pub(super) fn build(
        objects: &[ResidentObject],
        modules: &[ModuleDescriptor],
        budget: &MemoryBudget,
    ) -> Result<Self, BudgetError> {
        let mut result = Self {
            kinds: BudgetedVec::try_with_capacity(budget, Category::Metadata, MdKind::all().len())?,
            nested_subsystems: 0,
            modules: 0,
            form_modules: 0,
            module_bytes: 0,
            module_lines: 0,
            forms: 0,
            templates: 0,
        };
        for row in objects {
            let object = &row.object;
            if row.parent.is_none() {
                if let Some((_, count)) = result
                    .kinds
                    .iter_mut()
                    .find(|(kind, _)| *kind == object.kind)
                {
                    add(count, 1)?;
                } else {
                    result.kinds.try_push((object.kind, 1))?;
                }
            } else {
                add(&mut result.nested_subsystems, 1)?;
            }
            if object.kind == MdKind::CommonForm && object.managed_form {
                add(&mut result.forms, 1)?;
            }
            add(
                &mut result.forms,
                object.forms.iter().filter(|form| form.managed_form).count(),
            )?;
            add(&mut result.templates, object.templates.len())?;
            if object.kind == MdKind::CommonTemplate {
                add(&mut result.templates, 1)?;
            }
        }
        result
            .kinds
            .sort_unstable_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        for module in modules {
            if module.slot.kind == ModuleKind::FormModule {
                add(&mut result.form_modules, 1)?;
            } else {
                add(&mut result.modules, 1)?;
            }
            result.module_bytes = result
                .module_bytes
                .checked_add(module.facts.bytes)
                .ok_or(BudgetError::Overflow)?;
            result.module_lines = result
                .module_lines
                .checked_add(module.facts.lines)
                .ok_or(BudgetError::Overflow)?;
        }
        Ok(result)
    }
}
fn add(value: &mut usize, by: usize) -> Result<(), BudgetError> {
    *value = value.checked_add(by).ok_or(BudgetError::Overflow)?;
    Ok(())
}
