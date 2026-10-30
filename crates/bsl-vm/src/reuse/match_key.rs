use super::*;
use crate::ObjRef;

struct Reader<'a> {
    atoms: &'a [Atom],
    next: usize,
    work: u64,
    stack: Vec<(ObjRef, usize)>,
}

fn field_matches(value: &Str, folded: &str) -> bool {
    std::char::decode_utf16(value.units().iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .map(|c| {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(one), None) => one,
                _ => c,
            }
        })
        .eq(folded.chars())
}

impl Reader<'_> {
    fn charge(&mut self, ex: &Execution, units: u64) -> Result<(), Exception> {
        self.work = self.work.saturating_add(units);
        ex.check_charge(self.work)
    }

    fn value(&mut self, ex: &Execution, value: &Value) -> Result<bool, Exception> {
        self.charge(ex, 1)?;
        let same = match (value, self.atoms.get(self.next)) {
            (Value::Undefined, Some(Atom::Undefined)) | (Value::Null, Some(Atom::Null)) => true,
            (Value::Bool(a), Some(Atom::Bool(b))) => a == b,
            (Value::Date(a), Some(Atom::Date(b))) => a == b,
            (Value::Number(a), Some(Atom::Number(b))) => {
                self.charge(ex, a.size_hint_limbs().max(b.size_hint_limbs()) / 64)?;
                a == b
            }
            (Value::String(a), Some(Atom::String(b))) => {
                self.charge(ex, a.len().max(b.len()) as u64 / 64)?;
                a == b
            }
            (Value::Object(object), Some(Atom::Structure(expected))) => {
                let expected = *expected;
                let mut count = 0;
                if !object.reuse_fields(&mut |_, _| {
                    self.charge(ex, 1)?;
                    count += 1;
                    Ok(())
                })? || count != expected
                {
                    return Ok(false);
                }
                reserve(
                    ex,
                    (self.stack.capacity().saturating_mul(2).saturating_add(1) as u64)
                        .saturating_mul(size_of::<(ObjRef, usize)>() as u64),
                )?;
                self.stack
                    .try_reserve_exact(1)
                    .map_err(|_| ex.out_of_memory())?;
                self.stack.push((object.clone(), count));
                true
            }
            _ => false,
        };
        self.next += usize::from(same);
        Ok(same)
    }

    fn children(&mut self, ex: &Execution) -> Result<bool, Exception> {
        while let Some((object, remaining)) = self.stack.last_mut() {
            if *remaining == 0 {
                self.stack.pop();
                continue;
            }
            *remaining -= 1;
            let object = object.clone();
            let Some(Atom::Field(name)) = self.atoms.get(self.next) else {
                return Ok(false);
            };
            self.next += 1;
            let mut found = false;
            let mut same = false;
            if !object.reuse_fields(&mut |field, value| {
                self.charge(ex, 1 + field.len() as u64 / 64)?;
                if field_matches(field, name) {
                    found = true;
                    same = self.value(ex, value)?;
                }
                Ok(())
            })? || !found
                || !same
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn default(
        &mut self,
        ex: &Execution,
        value: Option<&bsl_compiler::ir::Const>,
    ) -> Result<bool, Exception> {
        use bsl_compiler::ir::Const;
        self.charge(ex, 1)?;
        let same = match (value, self.atoms.get(self.next)) {
            (None | Some(Const::Undefined), Some(Atom::Undefined))
            | (Some(Const::Null), Some(Atom::Null)) => true,
            (Some(Const::Bool(a)), Some(Atom::Bool(b))) => a == b,
            (Some(Const::Date(a)), Some(Atom::Date(b))) => a == b,
            (Some(Const::Number(a)), Some(Atom::Number(b))) => {
                self.charge(ex, a.size_hint_limbs().max(b.size_hint_limbs()) / 64)?;
                a == b
            }
            (Some(Const::String(a)), Some(Atom::String(b))) => {
                self.charge(ex, a.len().max(b.len()) as u64 / 64)?;
                &**a == b.units()
            }
            _ => false,
        };
        self.next += usize::from(same);
        Ok(same)
    }

    fn arguments(
        &mut self,
        ex: &Execution,
        module: &CompiledModule,
        method: u32,
        argc: usize,
    ) -> Result<bool, Exception> {
        let base = ex.exec.stack.len().saturating_sub(argc);
        for (index, param) in module.methods[method as usize].params.iter().enumerate() {
            if index >= argc || crate::exec::is_omitted(&ex.exec.stack[base + index]) {
                if !self.default(
                    ex,
                    param.default.and_then(|i| module.consts.get(i as usize)),
                )? {
                    return Ok(false);
                }
            } else if !self.value(ex, crate::exec::arg_initial(&ex.exec.stack[base + index]))?
                || !self.children(ex)?
            {
                return Ok(false);
            }
        }
        Ok(self.next == self.atoms.len())
    }
}

pub(super) fn hit(
    ex: &Execution,
    module: &Arc<CompiledModule>,
    method: u32,
    argc: usize,
) -> (Result<Option<Value>, Exception>, u64) {
    let mut work = 0u64;
    let result = (|| {
        for (key, value) in &ex.request_reuse.entries {
            work = work.saturating_add(1);
            ex.check_charge(work)?;
            if key.module.as_ptr() != Arc::as_ptr(module) || key.method != method {
                continue;
            }
            let mut reader = Reader {
                atoms: &key.atoms,
                next: 0,
                work,
                stack: Vec::new(),
            };
            let same = reader.arguments(ex, module, method, argc);
            work = reader.work;
            if same? {
                drop(reader);
                return copy_value(ex, value).map(Some);
            }
        }
        Ok(None)
    })();
    (result, work)
}
