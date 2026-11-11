use crate::Application;
use crate::model::{ObjId, StdAttr, TabDef};
use crate::read_budget as budget;
use crate::readtab::{TabView, column_index};
use crate::values::field::default_of;
use bsl_prim::{Number, fold_name};
use bsl_vm::{
    Arg, CallError, CallResult, Exception, Execution, HostError, HostObject, MemberId, MemberKind,
    ObjRef, RetainedMemory, TypeId, Value,
};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

const METHOD_BASE: u32 = 1 << 20;
const COLUMN_BASE: u32 = 1 << 21;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Count,
    Get,
    IndexOf,
    Add,
    Insert,
    Delete,
    Clear,
    Load,
    Total,
    Find,
    Unload,
    Later,
}

struct Method {
    ru: &'static str,
    en: &'static str,
    is_function: bool,
    min_args: u16,
    max_args: Option<u16>,
    op: Op,
}

const fn m(
    ru: &'static str,
    en: &'static str,
    is_function: bool,
    min_args: u16,
    max_args: Option<u16>,
    op: Op,
) -> Method {
    Method {
        ru,
        en,
        is_function,
        min_args,
        max_args,
        op,
    }
}

const METHODS: &[Method] = &[
    m("Количество", "Count", true, 0, Some(0), Op::Count),
    m("Получить", "Get", true, 1, Some(1), Op::Get),
    m("Индекс", "IndexOf", true, 1, Some(1), Op::IndexOf),
    m("Добавить", "Add", true, 0, Some(0), Op::Add),
    m("Вставить", "Insert", true, 1, Some(1), Op::Insert),
    m("Удалить", "Delete", false, 1, Some(1), Op::Delete),
    m("Очистить", "Clear", false, 0, Some(0), Op::Clear),
    m("Загрузить", "Load", false, 1, Some(1), Op::Load),
    m("Сдвинуть", "Move", false, 2, Some(2), Op::Later),
    m("Свернуть", "GroupBy", false, 1, Some(2), Op::Later),
    m("Выгрузить", "Unload", true, 0, Some(2), Op::Unload),
    m("ВыгрузитьКолонку", "UnloadColumn", true, 1, None, Op::Later),
    m("Найти", "Find", true, 1, Some(2), Op::Find),
    m("НайтиСтроки", "FindRows", true, 0, None, Op::Later),
    m("Итог", "Total", true, 1, Some(1), Op::Total),
    m("Сортировать", "Sort", false, 0, None, Op::Later),
];

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

fn off_bound() -> HostError {
    HostError::key(bsl_stdlib::bind::msg::RANGE)
}

pub(crate) fn not_writable(name: &str) -> HostError {
    HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
        .with_arg(name)
        .raw()
}

pub(crate) struct TabState {
    rows: Vec<Vec<Value>>,
    ids: Vec<u64>,
    next_id: u64,
    touched: bool,
}

pub(crate) struct ObjTab {
    app: Arc<Application>,
    obj: ObjId,
    tab: usize,
    ty: TypeId,
    row_ty: TypeId,
    me: Weak<ObjTab>,
    rows: RefCell<Vec<Vec<Value>>>,
    ids: RefCell<Vec<u64>>,
    next_id: Cell<u64>,
    modified: Rc<Cell<bool>>,
    touched: Cell<bool>,
    bytes: Cell<u64>,
}

impl ObjTab {
    pub(crate) fn new(
        app: &Arc<Application>,
        obj: ObjId,
        tab: usize,
        rows: Vec<Vec<Value>>,
        modified: Rc<Cell<bool>>,
    ) -> Result<Rc<ObjTab>, HostError> {
        let types = app
            .types()
            .ok_or_else(|| internal("the types are not registered"))?;
        let (Some(ty), Some(row_ty)) = (
            types.tabular_type(obj, tab, false),
            types.tabular_type(obj, tab, true),
        ) else {
            return Err(internal("no tabular section type"));
        };
        let n = rows.len() as u64;
        let bytes = Self::measure(&rows);
        Ok(Rc::new_cyclic(|me| ObjTab {
            app: app.clone(),
            obj,
            tab,
            ty,
            row_ty,
            me: me.clone(),
            rows: RefCell::new(rows),
            ids: RefCell::new((1..=n).collect()),
            next_id: Cell::new(n.saturating_add(1)),
            modified,
            touched: Cell::new(false),
            bytes: Cell::new(bytes),
        }))
    }

    fn measure(rows: &[Vec<Value>]) -> u64 {
        budget::rc_size::<ObjTab>()
            .saturating_add(budget::rc_size::<RefCell<Vec<Vec<Value>>>>())
            .saturating_add(budget::rows(rows, rows.len()))
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes.get()
    }

    fn def(&self) -> Result<&TabDef, HostError> {
        self.app
            .model()
            .obj(self.obj)
            .tabular
            .get(self.tab)
            .ok_or_else(|| internal("no such section"))
    }

    fn presentation_of(&self, ex: &mut Execution, ty: TypeId) -> CallResult<Option<bsl_prim::Str>> {
        if self.def().is_ok_and(|t| t.std_list.is_some()) {
            return Ok(Some(bsl_prim::Str::from(
                ex.runtime().types().name(ty, ex.script_variant()),
            )));
        }
        Ok(None)
    }

    fn type_name(&self) -> String {
        self.def()
            .map_or_else(|_| String::new(), |t| format!("ТабличнаяЧасть.{}", t.name))
    }

    pub(crate) fn lines(&self) -> Vec<Vec<Value>> {
        self.rows.borrow().clone()
    }

    pub(crate) fn state(&self) -> TabState {
        TabState {
            rows: self.lines(),
            ids: self.ids.borrow().clone(),
            next_id: self.next_id.get(),
            touched: self.touched.get(),
        }
    }

    pub(crate) fn set_state(&self, state: TabState) {
        let TabState {
            rows,
            ids,
            next_id,
            touched,
        } = state;
        self.bytes.set(Self::measure(&rows));
        *self.rows.borrow_mut() = rows;
        *self.ids.borrow_mut() = ids;
        self.next_id.set(next_id);
        self.touched.set(touched);
    }

    pub(crate) fn replace(&self, rows: Vec<Vec<Value>>) {
        self.put(rows);
        self.touched.set(false);
    }

    pub(crate) fn fill_from(&self, rows: Vec<Vec<Value>>) {
        self.put(rows);
        self.touched.set(true);
    }

    fn put(&self, rows: Vec<Vec<Value>>) {
        let n = rows.len() as u64;
        self.bytes.set(Self::measure(&rows));
        *self.rows.borrow_mut() = rows;
        *self.ids.borrow_mut() = (1..=n).collect();
        self.next_id.set(n.saturating_add(1));
    }

    pub(crate) fn touched(&self) -> bool {
        self.touched.get()
    }

    pub(crate) fn clear_touched(&self) {
        self.touched.set(false);
    }

    fn changed(&self) {
        self.touched.set(true);
        self.modified.set(true);
    }

    fn line_col(&self) -> Option<usize> {
        self.def()
            .ok()?
            .fields
            .iter()
            .position(|f| f.std == Some(StdAttr::LineNumber))
    }

    fn renumber(&self, from: usize) {
        let Some(col) = self.line_col() else {
            return;
        };
        for (j, row) in self.rows.borrow_mut().iter_mut().enumerate().skip(from) {
            if let Some(cell) = row.get_mut(col) {
                *cell = Value::Number(Number::from_i64(i64::try_from(j + 1).unwrap_or(i64::MAX)));
            }
        }
    }

    fn position_of(&self, id: u64, hint: usize) -> Option<usize> {
        let ids = self.ids.borrow();
        if ids.get(hint) == Some(&id) {
            Some(hint)
        } else {
            ids.iter().rposition(|x| *x == id)
        }
    }

    fn row_object(&self, ex: &mut Execution, at: usize) -> CallResult<Value> {
        let id = self.ids.borrow().get(at).copied().ok_or_else(off_bound)?;
        let me = self
            .me
            .upgrade()
            .ok_or_else(|| internal("the section is gone"))?;
        budget::reserve(
            ex,
            self.bytes
                .get()
                .saturating_add(budget::rc_size::<ObjTabRow>()),
        )?;
        Ok(Value::Object(Rc::new(ObjTabRow {
            tab: me,
            id,
            hint: Cell::new(at),
        })))
    }

    fn blank_line(&self, ex: &mut Execution) -> CallResult<Vec<Value>> {
        let def = self.def()?;
        let mut line = Vec::new();
        line.try_reserve_exact(def.fields.len())
            .map_err(|_| ex.out_of_memory())?;
        for f in &def.fields {
            line.push(match f.std {
                Some(StdAttr::LineNumber) => Value::Number(Number::from_i64(0)),
                _ => default_of(&self.app, f)?,
            });
        }
        Ok(line)
    }

    fn add(&self, ex: &mut Execution, at: Option<usize>) -> CallResult<Value> {
        ex.charge(1)?;
        let cols = self.def()?.fields.len();
        let grow = budget::slots::<Vec<Value>>(1)
            .saturating_add(budget::slots::<Value>(cols))
            .saturating_add(budget::slots::<u64>(1));
        budget::reserve(
            ex,
            self.bytes
                .get()
                .saturating_add(grow)
                .saturating_add(budget::rc_size::<ObjTabRow>()),
        )?;
        let line = self.blank_line(ex)?;
        let len = self.rows.borrow().len();
        let at = at.unwrap_or(len);
        if at > len {
            return Err(off_bound().into());
        }
        let id = self.next_id.get();
        self.next_id.set(id.saturating_add(1));
        self.rows.borrow_mut().insert(at, line);
        self.ids.borrow_mut().insert(at, id);
        self.bytes.set(self.bytes.get().saturating_add(grow));
        self.renumber(at);
        self.changed();
        self.row_object(ex, at)
    }

    fn delete(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let arg = crate::meta::arg_value(ex, args, 0)?;
        let at = match &arg {
            Value::Number(n) => {
                usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| off_bound())?
            }
            other => {
                let row = other
                    .as_object()
                    .and_then(|o| o.as_any().downcast_ref::<ObjTabRow>())
                    .filter(|r| std::ptr::eq(&*r.tab, self))
                    .ok_or_else(|| crate::meta::type_mismatch(1))?;
                self.position_of(row.id, row.hint.get())
                    .ok_or_else(off_bound)?
            }
        };
        if at >= self.rows.borrow().len() {
            return Err(off_bound().into());
        }
        self.rows.borrow_mut().remove(at);
        self.ids.borrow_mut().remove(at);
        self.renumber(at);
        self.changed();
        Ok(Value::Undefined)
    }

    fn load(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let source = crate::meta::arg_value(ex, args, 0)?;
        let table = source
            .as_object()
            .and_then(|o| {
                o.as_any()
                    .downcast_ref::<bsl_stdlib::collections::ValueTable>()
            })
            .ok_or_else(|| crate::meta::type_mismatch(1))?;
        let columns = table.columns(ex)?;
        let names = column_names(ex, &columns)?;
        let def = self.def()?;
        let mut map: Vec<(usize, usize)> = Vec::new();
        for (i, f) in def.fields.iter().enumerate() {
            if f.std == Some(StdAttr::LineNumber) {
                continue;
            }
            let wanted = fold_name(&f.name);
            if let Some(at) = names.iter().position(|n| fold_name(n) == wanted) {
                map.push((i, at));
            }
        }
        let count = table.rows(ex)?.len();
        let mut lines = Vec::new();
        let mut live = self.bytes.get();
        for r in 0..count {
            ex.charge(1)?;
            let mut line = self.blank_line(ex)?;
            for &(ours, theirs) in &map {
                let value = table.cell(ex, r, theirs)?;
                let Some(f) = def.fields.get(ours) else {
                    continue;
                };
                let cast = crate::values::typedesc::cast_to_field(&self.app, ex, f, value)?;
                live = live.saturating_add(budget::value_heap(&cast));
                budget::reserve(ex, live)?;
                if let Some(cell) = line.get_mut(ours) {
                    *cell = cast;
                }
            }
            live = live.saturating_add(budget::slots::<Value>(line.len()));
            lines.push(line);
        }
        self.put(lines);
        self.renumber(0);
        self.changed();
        Ok(Value::Undefined)
    }

    fn view_call(&self, op: Op, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let found = {
            let rows = self.rows.borrow();
            let view = TabView {
                app: &self.app,
                obj: self.obj,
                tab: self.tab,
                rows: rows.as_slice(),
                bytes: self.bytes.get(),
            };
            match op {
                Op::Total => return view.total(ex, args),
                Op::Unload => return view.unload(ex, args),
                _ => view.find_index(ex, args)?,
            }
        };
        match found {
            Some(at) => self.row_object(ex, at),
            None => Ok(Value::Undefined),
        }
    }
}

fn column_names(ex: &mut Execution, columns: &Value) -> CallResult<Vec<String>> {
    let Some(collection) = columns.as_object() else {
        return Err(internal("the columns of a table are not an object").into());
    };
    let count = collection.iter_len(ex).unwrap_or(0);
    let mut names = Vec::new();
    for i in 0..count {
        let column = collection.iter_get(ex, i)?;
        let name = ex.get_member(&column, "Имя").map_err(CallError::Exc)?;
        names.push(match name {
            Value::String(s) => s.to_string_lossy(),
            _ => String::new(),
        });
    }
    Ok(names)
}

fn visit_lines(
    rows: &[Vec<Value>],
    capacity: usize,
    visit: &mut dyn RetainedMemory,
) -> Result<(), Exception> {
    if capacity != 0 {
        visit.allocation(
            rows.as_ptr() as usize,
            budget::slots::<Vec<Value>>(capacity),
        )?;
    }
    for row in rows {
        visit.work(1)?;
        if row.capacity() != 0
            && visit.allocation(
                row.as_ptr() as usize,
                budget::slots::<Value>(row.capacity()),
            )?
        {
            for value in row {
                visit.value(value)?;
            }
        }
    }
    Ok(())
}

fn index_arg(args: &[Arg], ex: &mut Execution) -> CallResult<usize> {
    let v = crate::meta::arg_value(ex, args, 0)?;
    let Value::Number(n) = &v else {
        return Err(crate::meta::type_mismatch(1).into());
    };
    usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| off_bound().into())
}

impl HostObject for ObjTab {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.work(1)?;
        visit.mutable();
        let rows = self.rows.borrow();
        visit_lines(&rows, rows.capacity(), visit)?;
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        self.presentation_of(ex, self.ty)
    }

    fn get(&self, _: &mut Execution, _: MemberId) -> CallResult<Value> {
        Err(internal("a tabular section has no properties").into())
    }

    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(internal("a tabular section has no properties").into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let Some(method) =
            m.0.checked_sub(METHOD_BASE)
                .and_then(|i| METHODS.get(i as usize))
        else {
            return Err(internal("unknown method of a tabular section").into());
        };
        match method.op {
            Op::Count => Ok(Value::from(
                i64::try_from(self.rows.borrow().len()).unwrap_or(i64::MAX),
            )),
            Op::Get => {
                let i = index_arg(args, ex)?;
                self.row_object(ex, i)
            }
            Op::IndexOf => {
                let v = crate::meta::arg_value(ex, args, 0)?;
                let at = v
                    .as_object()
                    .and_then(|o| o.as_any().downcast_ref::<ObjTabRow>())
                    .filter(|r| std::ptr::eq(&*r.tab, self))
                    .and_then(|r| self.position_of(r.id, r.hint.get()))
                    .map_or(-1, |at| i64::try_from(at).unwrap_or(-1));
                Ok(Value::from(at))
            }
            Op::Add => self.add(ex, None),
            Op::Insert => {
                let at = index_arg(args, ex)?;
                self.add(ex, Some(at))
            }
            Op::Delete => self.delete(ex, args),
            Op::Clear => {
                self.rows.borrow_mut().clear();
                self.ids.borrow_mut().clear();
                self.bytes.set(Self::measure(&[]));
                self.changed();
                Ok(Value::Undefined)
            }
            Op::Load => self.load(ex, args),
            Op::Total | Op::Find | Op::Unload => self.view_call(method.op, ex, args),
            Op::Later => Err(HostError::not_implemented(&self.type_name(), method.ru).into()),
        }
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let n = fold_name(name);
        METHODS
            .iter()
            .position(|x| fold_name(x.ru) == n || fold_name(x.en) == n)
            .and_then(|i| u32::try_from(i).ok())
            .map(|i| MemberId(METHOD_BASE + i))
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let x = METHODS.get(m.0.checked_sub(METHOD_BASE)? as usize)?;
        Some(MemberKind::Method {
            is_function: x.is_function,
            min_args: x.min_args,
            max_args: x.max_args,
        })
    }

    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.rows.borrow().len())
    }

    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.row_object(ex, i)
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let Value::Number(n) = key else {
            return Err(crate::meta::type_mismatch(1).into());
        };
        let i = usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| off_bound())?;
        self.row_object(ex, i)
    }

    fn index_set(&self, _: &mut Execution, _: &Value, _: Value) -> CallResult<()> {
        Err(not_writable(&self.type_name()).into())
    }
}

pub(crate) struct ObjTabRow {
    tab: Rc<ObjTab>,
    id: u64,
    hint: Cell<usize>,
}

impl ObjTabRow {
    fn at(&self) -> Result<usize, HostError> {
        let at = self
            .tab
            .position_of(self.id, self.hint.get())
            .ok_or_else(off_bound)?;
        self.hint.set(at);
        Ok(at)
    }

    fn column(&self, ex: &Execution, m: MemberId) -> Result<usize, HostError> {
        let own = if m.0 >= COLUMN_BASE {
            Some(m)
        } else {
            ex.runtime()
                .types()
                .app_type(self.tab.row_ty)
                .and_then(|t| t.template)
                .and_then(|t| crate::meta::member_name(platform_catalog::catalog(), t, m))
                .and_then(|(method, ru, _)| (!method).then(|| self.find_member(ru)).flatten())
        };
        own.and_then(|m| m.0.checked_sub(COLUMN_BASE))
            .map(|i| i as usize)
            .ok_or_else(|| internal("no such column"))
    }
}

impl HostObject for ObjTabRow {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.work(1)?;
        let tab: ObjRef = self.tab.clone();
        visit.value(&Value::Object(tab))?;
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        self.tab.row_ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        self.tab.presentation_of(ex, self.tab.row_ty)
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let col = self.column(ex, m)?;
        let at = self.at()?;
        let rows = self.tab.rows.borrow();
        let value = rows
            .get(at)
            .and_then(|r| r.get(col))
            .ok_or_else(|| internal("no such column"))?;
        budget::reserve(
            ex,
            self.tab
                .bytes
                .get()
                .saturating_add(budget::value_heap(value)),
        )?;
        Ok(value.clone())
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let col = self.column(ex, m)?;
        let def = self.tab.def()?;
        let f = def
            .fields
            .get(col)
            .ok_or_else(|| internal("no such column"))?;
        if f.std == Some(StdAttr::LineNumber) {
            return Err(not_writable(&f.name).into());
        }
        let at = self.at()?;
        let cast = crate::values::typedesc::cast_to_field(&self.tab.app, ex, f, v)?;
        let grow = budget::value_heap(&cast);
        budget::reserve(ex, self.tab.bytes.get().saturating_add(grow))?;
        {
            let mut rows = self.tab.rows.borrow_mut();
            let cell = rows
                .get_mut(at)
                .and_then(|r| r.get_mut(col))
                .ok_or_else(|| internal("no such column"))?;
            let old = budget::value_heap(cell);
            *cell = cast;
            self.tab.bytes.set(
                self.tab
                    .bytes
                    .get()
                    .saturating_sub(old)
                    .saturating_add(grow),
            );
        }
        self.tab.changed();
        Ok(())
    }

    fn call(&self, _: &mut Execution, _: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(internal("a row has no methods").into())
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let def = self.tab.def().ok()?;
        column_index(def, name)
            .and_then(|i| u32::try_from(i).ok())
            .map(|i| MemberId(COLUMN_BASE + i))
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let col = m.0.checked_sub(COLUMN_BASE)? as usize;
        let f = self.tab.def().ok()?.fields.get(col)?;
        Some(MemberKind::Property {
            readable: true,
            writable: f.std != Some(StdAttr::LineNumber),
        })
    }
}
