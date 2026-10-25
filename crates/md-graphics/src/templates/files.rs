use super::allocation::{Allocation, Owned, Vector};
use memory_budget::BudgetError;
pub(super) type File = (String, Vec<u8>);
pub(crate) struct FileSet<'a> {
    allocation: Allocation<'a>,
    slots: Vector<Option<Owned<File>>>,
}
impl<'a> FileSet<'a> {
    pub(crate) fn new(allocation: Allocation<'a>) -> Result<Self, BudgetError> {
        Ok(Self {
            allocation,
            slots: allocation.vector(0)?,
        })
    }
    pub(crate) fn put(&mut self, path: &[&str], bytes: Owned<Vec<u8>>) -> Result<(), BudgetError> {
        let file = self.allocation.text(path)?.zip(bytes)?;
        if let Some(slot) = self
            .slots
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|old| old.0 == file.0))
        {
            *slot = Some(file);
            return Ok(());
        }
        self.slots.push(Some(file))
    }
    pub(crate) fn finish(mut self) -> Result<Owned<Vec<File>>, BudgetError> {
        let mut output = self.allocation.vector(self.slots.len())?.finish();
        for slot in self.slots.iter_mut() {
            output = output.push(slot.take().ok_or(BudgetError::Overflow)?)?;
        }
        Ok(output)
    }
}
