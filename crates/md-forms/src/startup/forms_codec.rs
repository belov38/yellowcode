use crate::forms::FormShared;
use crate::forms::shared::Shared as FormState;
use crate::startup::{
    ManagedDiagnostics, ManagedForm, ManagedObjectsError,
    index::{GuidText, text},
};
use crate::{ManagedEntries, ManagedEntrySource};
use brace::Guid;
use memory_budget::{BudgetError, Charged, Shared};

#[doc(hidden)]
pub fn form<S: ManagedEntrySource + ?Sized>(
    entries: &ManagedEntries<'_, S>,
    id: Guid,
    shared: &Shared<FormState>,
    keep: bool,
    diagnostics: &mut ManagedDiagnostics,
) -> Result<Option<Charged<crate::forms::Form>>, ManagedObjectsError<S::Error>> {
    let guid = GuidText::new(id);
    let name = text(entries.memory_budget(), &[guid.as_str()?, ".0"])?;
    let data = match entries.doc(name.as_str()) {
        Ok(data) => data,
        Err(error) if entries.recoverable_content_error(&error) => None,
        Err(error) => return Err(error.into()),
    };
    let result = match data {
        Some(data) => {
            let decoded = ManagedForm::decode(&data, keep)?;
            form_misses(&decoded, &guid, diagnostics)?;
            decoded.form.map(|form| {
                form.map(|mut form| {
                    form.shared = Some(FormShared::Managed(shared.clone().into_erased()));
                    form
                })
            })
        }
        None => None,
    };
    if !keep {
        entries.forget(name.as_str());
    }
    Ok(result)
}
fn form_misses(
    decoded: &ManagedForm,
    guid: &GuidText,
    diagnostics: &mut ManagedDiagnostics,
) -> Result<(), BudgetError> {
    let Some(serialization) = decoded.serialization else {
        if decoded.failed {
            diagnostics.miss(&["form:failed"])?;
        }
        return Ok(());
    };
    serialization.with_text(|variant| {
        if decoded.failed {
            diagnostics.miss(&["form:failed"])?;
            if !decoded.supported {
                let key = ["form:serialization:", variant];
                if diagnostics.count(&key) < crate::forms::SERIALIZATION_UUIDS {
                    diagnostics.miss(&[
                        "form:serialization-uuid:",
                        variant,
                        ":",
                        guid.as_str()?,
                    ])?;
                }
                diagnostics.miss(&key)?;
            } else if !serialization.is_4_59() {
                diagnostics.miss(&["form:failed:", variant])?;
            }
        } else if decoded.form.is_some() && !serialization.is_4_59() {
            diagnostics.miss(&["form:xml-unsupported:", variant])?;
        }
        Ok(())
    })
}
