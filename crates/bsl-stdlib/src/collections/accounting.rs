use super::*;
use bsl_prim::Str;
use bsl_vm::{ObjRef, RetainedWorkspace};
use std::{alloc::Layout, any::Any};

pub fn retained_rc_size<T>() -> u64 {
    super::retained::rc_layout_for(Layout::new::<T>())
        .map_or(u64::MAX, |(layout, _)| layout.pad_to_align().size() as u64)
}

pub fn retained_shallow_bytes(value: &Value) -> u64 {
    match value {
        Value::String(text) => super::retained::text_bytes(text),
        Value::Number(number) => (number.backing_capacity_limbs() as u64).saturating_mul(4),
        Value::Object(object) => super::retained::rc_bytes(object.as_ref()),
        _ => 0,
    }
}

#[derive(Debug)]
pub enum RetainedChargeError {
    Execution(Exception),
    Unsupported,
    Borrowed,
}
impl From<Exception> for RetainedChargeError {
    fn from(error: Exception) -> Self {
        Self::Execution(error)
    }
}
pub(crate) type Result<T> = std::result::Result<T, RetainedChargeError>;

pub fn retained_charge(ex: &mut Execution, roots: &[&Value]) -> Result<u64> {
    ex.with_retained_workspace(|ex, workspace| -> Result<u64> {
        let mut a = Accounting {
            ex,
            bytes: 0,
            workspace,
        };
        for value in roots {
            a.value(value)?;
        }
        while let Some(object) = a.workspace.pop() {
            a.object(&object)?;
        }
        Ok(a.bytes)
    })?
}

pub(crate) struct Accounting<'a> {
    pub(super) ex: &'a mut Execution,
    bytes: u64,
    workspace: &'a mut RetainedWorkspace,
}
impl Accounting<'_> {
    pub(super) fn block(&mut self, kind: u8, key: usize, bytes: u64) -> Result<bool> {
        if (bytes == 0 && kind != 6) || self.workspace.contains(kind, key) {
            return Ok(false);
        }
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| self.ex.out_of_memory())?;
        self.ex.reserve(
            self.bytes
                .checked_add(self.workspace.bytes())
                .ok_or_else(|| self.ex.out_of_memory())?,
        )?;
        Ok(self.workspace.first(self.ex, kind, key)?)
    }
    pub(super) fn value(&mut self, value: &Value) -> Result<()> {
        self.ex.charge(1)?;
        match value {
            Value::String(text) => self.text(text)?,
            Value::Number(number) => {
                self.block(
                    3,
                    std::ptr::from_ref(number) as usize,
                    (number.backing_capacity_limbs() as u64).saturating_mul(4),
                )?;
            }
            Value::Object(object) if self.rc(object)? => {
                self.workspace.push(self.ex, object.clone())?;
            }
            _ => {}
        }
        Ok(())
    }
    fn object(&mut self, object: &ObjRef) -> Result<()> {
        let actual: &dyn Any = object.as_ref();
        if let Some(value) = actual.downcast_ref::<array::Array>() {
            value.retained(self)
        } else if let Some(value) = actual.downcast_ref::<structure::Structure>() {
            value.retained(self)
        } else if let Some(result) = value_table::retained_object(actual, self) {
            result
        } else if let Some(result) = value_tree::retained_object(actual, self) {
            result
        } else if let Some(result) = columns::retained_object(actual, self) {
            result
        } else if let Some(value) = actual.downcast_ref::<crate::typedesc::TypeDescription>() {
            value.retained(self)
        } else if let Some((key, bytes)) =
            crate::values::binary_retained_layout(&Value::Object(object.clone()))
        {
            self.block(4, key, bytes)?;
            Ok(())
        } else {
            Err(RetainedChargeError::Unsupported)
        }
    }
}

pub(crate) trait Charge {
    fn work(&mut self, units: u64) -> Result<()>;
    fn block(&mut self, kind: u8, identity: usize, bytes: u64) -> Result<bool>;
    fn value(&mut self, value: &Value) -> Result<()>;
    fn text(&mut self, value: &Str) -> Result<()> {
        self.work(1)?;
        self.block(
            2,
            value.backing_identity(),
            super::retained::text_bytes(value),
        )?;
        Ok(())
    }
    fn rc<T: ?Sized>(&mut self, value: &Rc<T>) -> Result<bool> {
        self.work(1)?;
        let (layout, offset) =
            super::retained::rc_layout(value.as_ref()).ok_or(RetainedChargeError::Unsupported)?;
        let data = Rc::as_ptr(value) as *const () as usize;
        self.block(0, data, layout.size() as u64)?;
        self.block(
            6,
            data.checked_sub(offset)
                .and_then(|header| header.checked_add(1))
                .ok_or(RetainedChargeError::Unsupported)?,
            0,
        )
    }
    #[allow(clippy::ptr_arg)]
    fn vector<T>(&mut self, value: &Vec<T>) -> Result<()> {
        self.work(1)?;
        let bytes = Layout::array::<T>(value.capacity())
            .map_err(|_| RetainedChargeError::Unsupported)?
            .size() as u64;
        self.block(1, value.as_ptr() as usize, bytes)?;
        Ok(())
    }
    #[allow(clippy::ptr_arg)]
    fn string(&mut self, value: &String) -> Result<()> {
        self.work(1)?;
        self.block(1, value.as_ptr() as usize, value.capacity() as u64)?;
        Ok(())
    }
    fn shared_object<T: bsl_vm::HostObject>(&mut self, value: &Rc<T>) -> Result<()> {
        self.value(&Value::Object(value.clone()))
    }
}
impl Charge for Accounting<'_> {
    fn work(&mut self, units: u64) -> Result<()> {
        self.ex.charge(units)?;
        Ok(())
    }
    fn block(&mut self, kind: u8, identity: usize, bytes: u64) -> Result<bool> {
        Accounting::block(self, kind, identity, bytes)
    }
    fn value(&mut self, value: &Value) -> Result<()> {
        Accounting::value(self, value)
    }
}

struct Projection<'a> {
    visit: &'a mut dyn bsl_vm::RetainedMemory,
}
impl Charge for Projection<'_> {
    fn work(&mut self, units: u64) -> Result<()> {
        self.visit.work(units)?;
        Ok(())
    }
    fn block(&mut self, _kind: u8, identity: usize, bytes: u64) -> Result<bool> {
        Ok(self.visit.allocation(identity, bytes)?)
    }
    fn value(&mut self, value: &Value) -> Result<()> {
        self.visit.value(value)?;
        Ok(())
    }
    fn text(&mut self, value: &Str) -> Result<()> {
        self.visit.work(1)?;
        self.visit.string(value)?;
        Ok(())
    }
}

pub(super) fn project(
    actual: &dyn Any,
    visit: &mut dyn bsl_vm::RetainedMemory,
) -> std::result::Result<bool, Exception> {
    visit.mutable();
    let mut a = Projection { visit };
    let result = value_table::retained_object(actual, &mut a)
        .or_else(|| value_tree::retained_object(actual, &mut a))
        .or_else(|| columns::retained_object(actual, &mut a));
    match result {
        Some(Ok(())) => Ok(true),
        Some(Err(RetainedChargeError::Execution(error))) => Err(error),
        Some(Err(RetainedChargeError::Unsupported | RetainedChargeError::Borrowed)) | None => {
            Ok(false)
        }
    }
}
