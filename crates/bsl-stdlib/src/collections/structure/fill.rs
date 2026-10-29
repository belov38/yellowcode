use super::*;
use std::mem::size_of;

fn text_bytes(text: &Str) -> u64 {
    ((size_of::<Vec<u16>>() + 2 * size_of::<usize>()) as u64)
        .saturating_add((text.backing_capacity_units() as u64).saturating_mul(2))
}

fn direct_value_bytes(value: &Value) -> u64 {
    match value {
        Value::Number(number) => (number.backing_capacity_limbs() as u64).saturating_mul(4),
        Value::String(text) => text_bytes(text),
        Value::Object(object) => {
            let wrapper = std::alloc::Layout::new::<[usize; 2]>()
                .extend(std::alloc::Layout::for_value(object.as_ref()))
                .map_or(u64::MAX, |(layout, _)| layout.pad_to_align().size() as u64);
            crate::values::binary_data_bytes_direct(value).map_or(wrapper, |bytes| {
                wrapper
                    .saturating_add(bytes.len() as u64)
                    .saturating_add((2 * size_of::<usize>()) as u64)
            })
        }
        _ => 0,
    }
}

fn source_bytes(fields: &Fields, ex: &mut Execution) -> CallResult<u64> {
    ex.charge(
        (fields.entries.len() as u64)
            .saturating_add(fields.by_name.capacity() as u64)
            .saturating_add(fields.by_id.capacity() as u64),
    )?;
    let mut live = ((size_of::<Structure>() + 2 * size_of::<usize>()) as u64)
        .saturating_add(
            (fields.entries.capacity() as u64).saturating_mul(size_of::<Field>() as u64),
        )
        .saturating_add(
            (fields.by_name.capacity() as u64)
                .saturating_mul(2)
                .saturating_mul(size_of::<(String, usize)>() as u64 + 1),
        )
        .saturating_add(
            (fields.by_id.capacity() as u64)
                .saturating_mul(2)
                .saturating_mul(size_of::<(u32, usize)>() as u64 + 1),
        )
        .saturating_add(64);
    for name in fields.by_name.keys() {
        live = live.saturating_add(name.capacity() as u64);
    }
    for field in &fields.entries {
        live = live
            .saturating_add(text_bytes(&field.key))
            .saturating_add(direct_value_bytes(&field.value));
    }
    Ok(live)
}

#[allow(
    dead_code,
    reason = "internal property inventory for the pending Task10 consumer"
)]
impl Structure {
    pub(crate) fn property_names(&self, ex: &mut Execution, retained: u64) -> CallResult<Vec<Str>> {
        let fields = self.fields.borrow();
        let live = source_bytes(&fields, ex)?
            .saturating_add(retained)
            .saturating_add(size_of::<Vec<Str>>() as u64);
        ex.charge(fields.entries.len() as u64)?;
        admit_live(
            ex,
            live.saturating_add(
                (fields.entries.len() as u64).saturating_mul(size_of::<Str>() as u64),
            ),
        )?;
        let mut result = super::super::vec_with_capacity::<Str>(ex, fields.entries.len() as u64)?;
        admit_live(
            ex,
            live.saturating_add((result.capacity() as u64).saturating_mul(size_of::<Str>() as u64)),
        )?;
        result.extend(fields.entries.iter().map(|field| field.key.clone()));
        Ok(result)
    }
}

fn admit_live(ex: &mut Execution, live: u64) -> CallResult<()> {
    if live == u64::MAX {
        return Err(ex.out_of_memory().into());
    }
    Ok(ex.reserve(live)?)
}

#[allow(
    dead_code,
    reason = "internal value lookup for the pending Task10 consumer"
)]
impl Structure {
    pub(crate) fn property_value(
        &self,
        ex: &mut Execution,
        name: &Str,
        retained: u64,
    ) -> CallResult<Option<Value>> {
        ex.charge(1)?;
        let fields = self.fields.borrow();
        let base = source_bytes(&fields, ex)?
            .saturating_add(text_bytes(name))
            .saturating_add(retained)
            .saturating_add(
                (size_of::<Str>() + size_of::<String>() + size_of::<Option<Value>>()) as u64,
            );
        ex.charge(name.len() as u64 + 1)?;
        let requested = name
            .len()
            .checked_mul(4)
            .ok_or_else(|| ex.out_of_memory())?;
        admit_live(ex, base.saturating_add(requested as u64))?;
        let mut folded = String::new();
        folded
            .try_reserve_exact(requested)
            .map_err(|_| ex.out_of_memory())?;
        let live = base.saturating_add(folded.capacity() as u64);
        admit_live(ex, live)?;
        for unit in char::decode_utf16(name.units().iter().copied()) {
            let ch = unit.unwrap_or(char::REPLACEMENT_CHARACTER);
            let mut lower = ch.to_lowercase();
            let ch = match (lower.next(), lower.next()) {
                (Some(ch), None) => ch,
                _ => ch,
            };
            folded.push(ch);
        }
        ex.charge(folded.len() as u64 + 1)?;
        let Some(&index) = fields.by_name.get(&folded) else {
            return Ok(None);
        };
        let value = &fields.entries[index].value;
        if let Value::Number(number) = value {
            ex.charge(number.size_hint_limbs() / 64 + 1)?;
            let copy = number
                .try_clone_with(|limbs| {
                    admit_live(ex, live.saturating_add((limbs as u64).saturating_mul(4)))
                })
                .map_err(|error| match error {
                    bsl_prim::number::NumberAllocError::Reserve(error) => error,
                    _ => ex.out_of_memory().into(),
                })?;
            admit_live(
                ex,
                live.saturating_add((copy.backing_capacity_limbs() as u64).saturating_mul(4)),
            )?;
            return Ok(Some(Value::Number(copy)));
        }
        Ok(Some(value.clone()))
    }
}
