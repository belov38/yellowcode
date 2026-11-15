use super::{Column, FIELD_BASE, SetCore, Source, member, refusal, type_of};
use crate::model::{StdAttr, TypeAtom};
use crate::read_budget;
use crate::session::internal;
use crate::types::Category;
use bsl_prim::{Str, fold_name};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, MemberKind,
    RetainedMemory, TypeId, Value,
};
use ibschema::fields::FieldValue;
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) enum Slot {
    Value(Value),
    Stored(Option<FieldValue>),
}

pub(crate) fn opaque(core: &SetCore, col: &Column) -> bool {
    core.field(col).is_some_and(|f| {
        f.types
            .atoms
            .iter()
            .any(|a| matches!(a, TypeAtom::ValueStorage | TypeAtom::Binary))
    })
}

pub(crate) struct Record {
    core: Rc<SetCore>,
    ty: TypeId,
    pub(crate) cols: Rc<Vec<Column>>,
    cells: RefCell<Vec<Slot>>,
}

impl Record {
    pub(crate) fn new(
        ex: &mut Execution,
        core: &Rc<SetCore>,
        cols: Rc<Vec<Column>>,
        cells: Vec<Slot>,
    ) -> CallResult<Rc<Record>> {
        let ty = type_of(&core.app, core.obj, Category::Record)?;
        let bytes = cells.iter().fold(
            read_budget::rc_size::<Record>()
                .saturating_add(read_budget::slots::<Slot>(cells.capacity())),
            |n, c| n.saturating_add(slot_heap(c)),
        );
        read_budget::reserve(ex, bytes)?;
        Ok(Rc::new(Record {
            core: core.clone(),
            ty,
            cols,
            cells: RefCell::new(cells),
        }))
    }

    pub(crate) fn blank(
        ex: &mut Execution,
        core: &Rc<SetCore>,
        cols: Rc<Vec<Column>>,
    ) -> CallResult<Rc<Record>> {
        let mut cells = Vec::with_capacity(cols.len());
        for col in cols.iter() {
            cells.push(if opaque(core, col) {
                Slot::Stored(None)
            } else {
                match core.field(col) {
                    Some(f) => Slot::Value(crate::values::field::default_of(&core.app, f)?),
                    None => Slot::Value(Value::Undefined),
                }
            });
        }
        Record::new(ex, core, cols, cells)
    }

    fn refusal(&self, member: &str) -> HostError {
        refusal(self.core.def(), "Запись", member)
    }

    fn column(&self, ex: &Execution, m: MemberId) -> Result<usize, HostError> {
        if m.0 >= FIELD_BASE {
            return usize::try_from(m.0 - FIELD_BASE)
                .ok()
                .filter(|i| *i < self.cols.len())
                .ok_or_else(|| internal("unknown field of a record"));
        }
        let Some((false, ru, en)) = member(ex, self.ty, m) else {
            return Err(internal("unknown member of a record"));
        };
        let std = crate::read::std_attr_named(ru).or_else(|| crate::read::std_attr_named(en));
        self.cols
            .iter()
            .position(|c| c.std.is_some() && c.std == std)
            .ok_or_else(|| crate::meta::no_field(ru))
    }

    pub(crate) fn value(&self, i: usize) -> Result<Value, HostError> {
        let storage = self
            .cols
            .get(i)
            .and_then(|c| self.core.field(c))
            .is_some_and(|f| matches!(f.types.single(), Some(TypeAtom::ValueStorage)));
        match self.cells.borrow().get(i) {
            Some(Slot::Value(v)) => Ok(v.clone()),
            Some(Slot::Stored(None)) if storage => Ok(crate::values::field::empty_storage()),
            Some(Slot::Stored(Some(FieldValue::Storage(b)))) if storage => {
                crate::values::field::storage_of(&self.core.app, b)
            }
            Some(Slot::Stored(_)) => Err(self.opaque_refusal(i)),
            None => Err(internal("no such column of a record")),
        }
    }

    pub(crate) fn stored(&self, i: usize) -> Option<Option<FieldValue>> {
        match self.cells.borrow().get(i) {
            Some(Slot::Stored(v)) => Some(v.clone()),
            _ => None,
        }
    }

    fn opaque_refusal(&self, i: usize) -> HostError {
        let name = self.cols.get(i).map_or("", |c| c.name.as_str());
        self.refusal(&format!(
            "{name}: значение типа ХранилищеЗначения или ДвоичныеДанные"
        ))
    }

    pub(crate) fn assign(&self, ex: &mut Execution, i: usize, v: Value) -> CallResult<()> {
        let col = self
            .cols
            .get(i)
            .ok_or_else(|| internal("no such column of a record"))?;
        if opaque(&self.core, col) {
            return Err(self.opaque_refusal(i).into());
        }
        let (Source::Field(_), Some(f)) = (col.source, self.core.field(col)) else {
            return Err(self.refusal(&format!("{}: разделитель", col.name)).into());
        };
        if f.std == Some(StdAttr::LineNumber) {
            return Err(crate::objtab::not_writable(&col.name).into());
        }
        let cast = crate::values::typedesc::cast_to_field(&self.core.app, ex, f, v)?;
        let bytes = self.bytes().saturating_add(read_budget::value_heap(&cast));
        read_budget::reserve(ex, bytes)?;
        if let Some(slot) = self.cells.borrow_mut().get_mut(i) {
            *slot = Slot::Value(cast);
        }
        self.core.modified.set(true);
        Ok(())
    }

    fn bytes(&self) -> u64 {
        let cells = self.cells.borrow();
        cells.iter().fold(
            read_budget::rc_size::<Record>()
                .saturating_add(read_budget::slots::<Slot>(cells.capacity())),
            |n, c| n.saturating_add(slot_heap(c)),
        )
    }
}

fn slot_heap(s: &Slot) -> u64 {
    match s {
        Slot::Value(v) => read_budget::value_heap(v),
        Slot::Stored(Some(v)) => read_budget::field_heap(v),
        Slot::Stored(None) => 0,
    }
}

impl HostObject for Record {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.work(1)?;
        visit.mutable();
        let cells = self.cells.borrow();
        visit.bytes(read_budget::slots::<Slot>(cells.capacity()))?;
        for c in cells.iter() {
            match c {
                Slot::Value(v) => visit.value(v)?,
                Slot::Stored(Some(v)) => visit.bytes(read_budget::field_heap(v))?,
                Slot::Stored(None) => {}
            }
        }
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let i = self.column(ex, m)?;
        let v = self.value(i)?;
        read_budget::reserve(ex, self.bytes().saturating_add(read_budget::value_heap(&v)))?;
        Ok(v)
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let i = self.column(ex, m)?;
        self.assign(ex, i, v)
    }

    fn call(&self, ex: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        let name = member(ex, self.ty, m).map_or_else(|| m.0.to_string(), |n| n.1.to_string());
        Err(self.refusal(&name).into())
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let wanted = fold_name(name);
        let i = self
            .cols
            .iter()
            .position(|c| fold_name(&c.name) == wanted)?;
        FIELD_BASE.checked_add(u32::try_from(i).ok()?).map(MemberId)
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let i = usize::try_from(m.0.checked_sub(FIELD_BASE)?).ok()?;
        (i < self.cols.len()).then_some(MemberKind::Property {
            readable: true,
            writable: true,
        })
    }

    fn property_names(&self, ex: &mut Execution, retained: u64) -> CallResult<Option<Vec<Str>>> {
        let bytes = self
            .cols
            .iter()
            .fold(read_budget::slots::<Str>(self.cols.len()), |n, c| {
                n.saturating_add((c.name.len() as u64).saturating_mul(2).saturating_add(64))
            });
        read_budget::reserve(ex, retained.saturating_add(bytes))?;
        Ok(Some(
            self.cols
                .iter()
                .map(|c| Str::from(c.name.as_str()))
                .collect(),
        ))
    }
}
