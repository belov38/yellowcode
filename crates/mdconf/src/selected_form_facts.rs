use crate::{ManagedEntrySource, forms, startup::ManagedViewerContext};
use brace::Guid;
use memory_budget::{BudgetError, BudgetedWriter, Category, Charged};

#[derive(Debug)]
pub struct FormFactFrame {
    bytes: Charged<Vec<u8>>,
    pub has_shared: bool,
}
impl FormFactFrame {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

pub fn write_form_facts<S: ManagedEntrySource>(
    context: &ManagedViewerContext<S>,
    form: &Charged<forms::Form>,
) -> Result<FormFactFrame, BudgetError> {
    write_form_facts_checked(context, form, || Ok(()))
}

pub fn write_form_facts_checked<S: ManagedEntrySource, E: From<BudgetError>>(
    context: &ManagedViewerContext<S>,
    form: &Charged<forms::Form>,
    mut check: impl FnMut() -> Result<(), E>,
) -> Result<FormFactFrame, E> {
    check()?;
    let budget = context.index().memory_budget();
    if !form.reservation().budget().same_scope(budget) {
        return Err(BudgetError::Overflow.into());
    }
    let shared = forms::shared_facts(form);
    if form.shared.is_some()
        && shared.is_none_or(|v| v.managed_budget().is_none_or(|b| !b.same_scope(budget)))
    {
        return Err(BudgetError::Overflow.into());
    }
    let mut out = Encoder {
        writer: BudgetedWriter::new(
            budget,
            memory_budget::sealed_heap::ANALYSIS_HEAP_BYTES as u64,
        )?,
        check: &mut check,
    };
    let v85 = shared.is_some_and(|v| v.v85());
    let compat = forms::shared::form_compat(context.settings().compatibility_mode.0, v85);
    out.raw(b"YCF1")?;
    out.raw(&[
        u8::from(v85),
        u8::from(compat == mdschema::forms::FormCompat::V8_3_12),
        0,
        0,
    ])?;
    let index = context.index();
    out.number(index.names.len())?;
    for (id, name) in index.names.iter() {
        out.guid(id)?;
        out.blob(name.as_str().as_bytes())?;
    }
    out.number(index.types.len())?;
    for (id, name) in index.types.iter() {
        out.guid(id)?;
        out.blob(name.as_str().as_bytes())?;
    }
    out.number(context.predefined().len())?;
    for (id, name) in context.predefined().iter() {
        out.guid(id)?;
        out.blob(name.as_bytes())?;
    }
    out.number(index.sub_hdr.len())?;
    for (id, path) in index.sub_hdr.iter() {
        out.guid(id)?;
        out.number(path.len())?;
        for value in path.iter() {
            out.number(*value)?;
        }
    }
    let order = shared.map(|v| v.names_order()).unwrap_or_default();
    out.number(order.len())?;
    for id in order {
        out.guid(id)?;
    }
    out.number(shared.map_or(0, |v| v.child_patterns().count()))?;
    if let Some(shared) = shared {
        for (id, pattern) in shared.child_patterns() {
            out.guid(id)?;
            let bytes = brace::write_node_budgeted(pattern, budget, Category::Response)?;
            out.blob(&bytes)?;
        }
    }
    (out.check)()?;
    Ok(FormFactFrame {
        bytes: out.writer.into_buffer().into_charged_vec(),
        has_shared: shared.is_some(),
    })
}
struct Encoder<'a, F> {
    writer: BudgetedWriter,
    check: &'a mut F,
}
impl<E: From<BudgetError>, F: FnMut() -> Result<(), E>> Encoder<'_, F> {
    fn raw(&mut self, bytes: &[u8]) -> Result<(), E> {
        (self.check)()?;
        for chunk in bytes.chunks(64 << 10) {
            (self.check)()?;
            self.writer.try_write(chunk)?;
        }
        Ok(())
    }
    fn number(&mut self, value: usize) -> Result<(), E> {
        self.raw(
            &u64::try_from(value)
                .map_err(|_| BudgetError::Overflow)?
                .to_le_bytes(),
        )
    }
    fn guid(&mut self, value: &Guid) -> Result<(), E> {
        self.raw(&value.to_bytes_le())
    }
    fn blob(&mut self, bytes: &[u8]) -> Result<(), E> {
        self.number(bytes.len())?;
        self.raw(bytes)
    }
}
