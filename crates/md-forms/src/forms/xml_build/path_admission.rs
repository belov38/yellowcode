use super::builder::Builder;
use super::{FormBuildError, Result};
use crate::forms::ctx::{FormInfo, PathAdmission};
use crate::forms::py::{Py, Raised};
use brace::Node;
use mdconf_core::external::{TypeUse, type_use_bytes};
use memory_budget::Category;
use std::cell::RefCell;

struct Admission<'b, 'a> {
    builder: &'b Builder<'a>,
    error: RefCell<Option<FormBuildError>>,
}
impl Admission<'_, '_> {
    fn capture(&self, result: Result<()>) -> Py<()> {
        result.map_err(|error| {
            let mut stored = self.error.borrow_mut();
            if stored.is_none() {
                *stored = Some(error);
            }
            Raised
        })
    }
    fn retain(&self, bytes: Option<u64>) -> Result<()> {
        self.builder.check()?;
        let bytes = bytes.ok_or(FormBuildError::Limit("resolved form names"))?;
        if bytes != 0 {
            let charge = self
                .builder
                .memory
                .try_reserve(Category::Execution, bytes)?;
            self.builder.asset_charges.borrow_mut().push(charge);
        }
        Ok(())
    }
}
impl PathAdmission for Admission<'_, '_> {
    fn type_names(&self, pattern: &Node) -> Py<()> {
        if self.error.borrow().is_some() {
            return Err(Raised);
        }
        let result = (|| {
            let bytes = type_use_bytes(
                pattern,
                &|id| self.builder.resolution.type_name(&id),
                TypeUse::FormNames,
                &|| {
                    if (self.builder.cancelled)() {
                        return Err(mdconf_core::Error::Io(std::io::Error::from(
                            std::io::ErrorKind::Interrupted,
                        )));
                    }
                    self.builder
                        .memory
                        .check_admission(Category::Execution)
                        .map_err(mdconf_core::Error::Memory)
                },
            )
            .map_err(|e| match e {
                mdconf_core::Error::Memory(e) => FormBuildError::Memory(e),
                mdconf_core::Error::Io(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    FormBuildError::Cancelled
                }
                e => FormBuildError::Resource(e.to_string()),
            })?;
            self.retain(Some(bytes))
        })();
        self.capture(result)
    }
    fn reserve(&self, bytes: Option<u64>) -> Py<()> {
        if self.error.borrow().is_some() {
            return Err(Raised);
        }
        self.capture(self.retain(bytes))
    }
}
impl Builder<'_> {
    pub fn with_form_info<T>(
        &self,
        operation: impl FnOnce(&mut FormInfo<'_>) -> T,
    ) -> Result<Option<T>> {
        let admission = Admission {
            builder: self,
            error: RefCell::new(None),
        };
        let result = self
            .path_model
            .as_ref()
            .and_then(|model| {
                FormInfo::new_resolved(
                    self.resolution,
                    &self.path_shared,
                    model,
                    self.input.owner,
                    &admission,
                )
            })
            .map(|mut info| operation(&mut info));
        if let Some(error) = admission.error.into_inner() {
            return Err(error);
        }
        Ok(result)
    }
}
