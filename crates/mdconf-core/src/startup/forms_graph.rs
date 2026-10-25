use super::{ManagedObjectsError, managed_modules::ManagedModuleGraph};
use crate::startup::ManagedIndex;
use crate::{ManagedEntries, ManagedEntrySource, MdKind, SubKind};
use memory_budget::{BudgetError, BudgetedVec, Category};
const CATEGORY: Category = Category::Metadata;

impl ManagedModuleGraph {
    #[doc(hidden)]
    pub fn load_forms_with<S: ManagedEntrySource + ?Sized, C: crate::codec::ManagedCodecs>(
        mut self,
        entries: &ManagedEntries<'_, S>,
        index: &ManagedIndex,
        keep: bool,
        codecs: &C,
    ) -> Result<Self, ManagedObjectsError<S::Error>> {
        let budget = entries.memory_budget();
        if !self
            .objects
            .objects
            .reservation()
            .budget()
            .same_scope(budget)
            || !index.memory_budget().same_scope(budget)
        {
            return Err(BudgetError::Overflow.into());
        }
        let shared = codecs.form_state_with_retention(entries, index, keep)?;
        let mut todo = BudgetedVec::new(budget, CATEGORY)?;
        for (at, location) in self.objects.locations.iter().enumerate() {
            if location.parent.is_none() {
                todo.try_push(at)?;
            }
        }
        while let Some(at) = todo.pop() {
            let object = self.objects.resolve(at).ok_or(BudgetError::Overflow)?;
            let (id, kind) = (object.id, object.kind);
            if kind == MdKind::CommonForm {
                if object.form.is_some() {
                    return Err(BudgetError::Overflow.into());
                }
                if let Some(form) =
                    codecs.form(entries, id, &shared, keep, &mut self.diagnostics)?
                {
                    self.objects = self.objects.install(
                        at,
                        form,
                        |object| object.form.is_none(),
                        |object, form| {
                            object.form = Some(form);
                            None
                        },
                    )?;
                }
            }
            let count = self
                .objects
                .resolve(at)
                .ok_or(BudgetError::Overflow)?
                .forms
                .len();
            for child_at in 0..count {
                let child = self
                    .objects
                    .resolve(at)
                    .and_then(|object| object.forms.get(child_at))
                    .ok_or(BudgetError::Overflow)?;
                if child.kind != SubKind::Form {
                    continue;
                }
                if child.form.is_some() {
                    return Err(BudgetError::Overflow.into());
                }
                let id = child.id;
                if let Some(form) =
                    codecs.form(entries, id, &shared, keep, &mut self.diagnostics)?
                {
                    self.objects = self.objects.install(
                        at,
                        form,
                        |object| {
                            object
                                .forms
                                .get(child_at)
                                .is_some_and(|form| form.form.is_none())
                        },
                        |object, form| {
                            let Some(child) = object.forms.get_mut(child_at) else {
                                return Some(form);
                            };
                            child.form = Some(form);
                            None
                        },
                    )?;
                }
            }
            for (child_at, location) in self.objects.locations.iter().enumerate() {
                if location.parent == Some(at) {
                    todo.try_push(child_at)?;
                }
            }
        }
        Ok(self)
    }
}
