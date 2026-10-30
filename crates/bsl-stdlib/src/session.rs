pub(crate) fn copy<T: Copy>(source: &[T]) -> Result<Vec<T>, std::collections::TryReserveError> {
    let mut result = Vec::new();
    result.try_reserve_exact(source.len())?;
    result.extend_from_slice(source);
    Ok(result)
}

pub(crate) fn shared_bytes<T: ?Sized>(value: &T) -> u64 {
    std::alloc::Layout::new::<[usize; 2]>()
        .extend(std::alloc::Layout::for_value(value))
        .map_or(u64::MAX, |(layout, _)| layout.pad_to_align().size() as u64)
}

pub(crate) struct Leaf<T> {
    pub value: T,
    pub restore_bytes: u64,
}
impl<T: bsl_vm::SharedObject + Send + Sync> bsl_vm::SessionObject for Leaf<T> {
    fn retained_memory(&self, _: &mut dyn bsl_vm::RetainedMemory) -> Result<(), bsl_vm::Exception> {
        Ok(())
    }
    fn create(
        &self,
        ex: &mut bsl_vm::Execution,
        _: &mut bsl_vm::SessionImports,
    ) -> Result<bsl_vm::Value, bsl_vm::Exception> {
        self.value.to_value(ex)
    }
    fn fill(
        &self,
        ex: &mut bsl_vm::Execution,
        _: &bsl_vm::Value,
        children: Vec<bsl_vm::Value>,
    ) -> Result<(), bsl_vm::Exception> {
        if children.is_empty() {
            Ok(())
        } else {
            Err(ex.out_of_memory())
        }
    }
    fn clear(&self, _: &bsl_vm::Value) {}
    fn restore_bytes(&self, _: usize) -> u64 {
        self.restore_bytes
    }
}
