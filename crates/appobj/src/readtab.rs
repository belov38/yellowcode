use crate::Application;
use crate::model::{FieldDef, ObjId, StdAttr, TabDef, TypeAtom, TypeSet};
use crate::read::read_tabular;
use crate::values::refs::RefValue;
use bsl_prim::{Number, fold_name};
use bsl_vm::{
    Arg, CallError, CallResult, Category, CategorySet, Execution, HostError, HostObject, MemberId,
    MemberKind, TypeId, Value,
};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const METHOD_BASE: u32 = 1 << 20;
const COLUMN_BASE: u32 = 1 << 21;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Count,
    Get,
    IndexOf,
    Write,
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
    m("Добавить", "Add", true, 0, Some(0), Op::Write),
    m("Вставить", "Insert", true, 1, Some(1), Op::Write),
    m("Удалить", "Delete", false, 1, Some(1), Op::Write),
    m("Очистить", "Clear", false, 0, Some(0), Op::Write),
    m("Загрузить", "Load", false, 1, Some(1), Op::Write),
    m("Сдвинуть", "Move", false, 2, Some(2), Op::Write),
    m("Свернуть", "GroupBy", false, 1, Some(2), Op::Write),
    m("Выгрузить", "Unload", true, 0, Some(2), Op::Unload),
    m("ВыгрузитьКолонку", "UnloadColumn", true, 1, None, Op::Later),
    m("Найти", "Find", true, 1, Some(2), Op::Find),
    m("НайтиСтроки", "FindRows", true, 0, None, Op::Later),
    m("Итог", "Total", true, 1, Some(1), Op::Total),
    m("Сортировать", "Sort", false, 0, None, Op::Later),
];

fn read_only() -> HostError {
    let mut e = HostError::text(
        "Объект недоступен для изменения.",
        "Object is not available for modification.",
    );
    e.categories = Some(CategorySet::of(&[
        Category::AllErrors,
        Category::ScriptRuntimeError,
        Category::ScriptUseError,
    ]));
    e
}

fn off_bound() -> HostError {
    HostError::key(bsl_stdlib::bind::msg::RANGE)
}

fn blank(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | ' ')
}

fn is_line_number(name: &str) -> bool {
    fold_name(name) == fold_name(StdAttr::LineNumber.names().0)
}

fn no_column() -> HostError {
    HostError::text(
        "Недопустимое значение параметра (параметр номер '1') (Колонка не принадлежит коллекции)",
        "Invalid parameter value (parameter number '1') (Column does not belong to collection)",
    )
}

fn may_be_number(cell: &Value) -> bool {
    matches!(cell, Value::String(s) if s.to_string_lossy().chars().any(char::is_numeric))
}

pub(crate) fn tabular_value(
    ex: &mut Execution,
    app: &Arc<Application>,
    r: RefValue,
    tab: usize,
) -> CallResult<Value> {
    tabular_value_with_live(ex, app, r, tab, 0)
}

pub(crate) fn tabular_value_with_live(
    ex: &mut Execution,
    app: &Arc<Application>,
    r: RefValue,
    tab: usize,
    live: u64,
) -> CallResult<Value> {
    let wrapper =
        crate::read_budget::rc_size::<ReadTab>() + crate::read_budget::rc_size::<Vec<Vec<Value>>>();
    crate::read_budget::reserve(ex, live.saturating_add(wrapper))?;
    let rows = Rc::new(read_tabular(ex, app, r, tab, live.saturating_add(wrapper))?);
    let bytes = wrapper.saturating_add(crate::read_budget::rows(&rows, rows.capacity()));
    let types = app
        .types()
        .ok_or_else(|| internal("the types are not registered"))?;
    let (Some(ty), Some(row_ty)) = (
        types.tabular_type(r.obj, tab, false),
        types.tabular_type(r.obj, tab, true),
    ) else {
        return Err(internal("no tabular section type").into());
    };
    Ok(Value::Object(Rc::new(ReadTab {
        app: app.clone(),
        obj: r.obj,
        tab,
        ty,
        row_ty,
        rows,
        bytes,
    })))
}

fn internal(text: &str) -> HostError {
    HostError::text(&format!("internal: {text}"), &format!("internal: {text}"))
}

struct ReadTab {
    app: Arc<Application>,
    obj: ObjId,
    tab: usize,
    ty: TypeId,
    row_ty: TypeId,
    rows: Rc<Vec<Vec<Value>>>,
    bytes: u64,
}

impl ReadTab {
    fn def(&self) -> Option<&TabDef> {
        self.app.model().obj(self.obj).tabular.get(self.tab)
    }

    fn row(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.rows.get(i).ok_or_else(off_bound)?;
        crate::read_budget::reserve(
            ex,
            self.bytes
                .saturating_add(crate::read_budget::rc_size::<ReadTabRow>()),
        )?;
        Ok(Value::Object(Rc::new(ReadTabRow {
            app: self.app.clone(),
            obj: self.obj,
            tab: self.tab,
            ty: self.row_ty,
            rows: self.rows.clone(),
            at: i,
            bytes: self.bytes,
        })))
    }

    fn type_name(&self) -> String {
        self.def()
            .map_or_else(String::new, |t| format!("ТабличнаяЧасть.{}", t.name))
    }

    fn view(&self) -> TabView<'_> {
        TabView {
            app: &self.app,
            obj: self.obj,
            tab: self.tab,
            rows: &self.rows,
            bytes: self.bytes,
        }
    }

    fn total(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        self.view().total(ex, args)
    }

    fn find(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        match self.view().find_index(ex, args)? {
            Some(i) => self.row(ex, i),
            None => Ok(Value::Undefined),
        }
    }

    fn unload(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        self.view().unload(ex, args)
    }
}

pub(crate) struct TabView<'a> {
    pub app: &'a Arc<Application>,
    pub obj: ObjId,
    pub tab: usize,
    pub rows: &'a [Vec<Value>],
    pub bytes: u64,
}

impl TabView<'_> {
    fn def(&self) -> Option<&TabDef> {
        self.app.model().obj(self.obj).tabular.get(self.tab)
    }

    fn type_name(&self) -> String {
        self.def()
            .map_or_else(String::new, |t| format!("ТабличнаяЧасть.{}", t.name))
    }

    fn refuse(&self, method: &str, form: &str) -> HostError {
        HostError::not_implemented(&self.type_name(), &format!("{method}: {form}"))
    }

    fn atoms(&self, at: usize) -> &[TypeAtom] {
        self.def()
            .and_then(|def| def.fields.get(at))
            .map_or(&[], |f| f.types.atoms.as_slice())
    }

    fn attributes(&self) -> impl Iterator<Item = usize> {
        self.def().into_iter().flat_map(|def| {
            def.fields
                .iter()
                .enumerate()
                .filter(|(_, f)| f.std != Some(StdAttr::LineNumber))
                .map(|(i, _)| i)
        })
    }

    fn column(&self, method: &str, name: &str) -> Result<usize, HostError> {
        let def = self.def().ok_or_else(|| internal("no such section"))?;
        match column_index(def, name) {
            Some(i)
                if def
                    .fields
                    .get(i)
                    .is_some_and(|f| f.std != Some(StdAttr::LineNumber)) =>
            {
                Ok(i)
            }
            Some(_) => Err(self.refuse(method, "колонка НомерСтроки")),
            None => Err(self.refuse(method, "колонка не найдена")),
        }
    }

    pub(crate) fn total(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let at = match crate::meta::arg_value(ex, args, 0)? {
            Value::String(name) => {
                let name = name.to_string_lossy();
                let known = self.def().and_then(|def| column_index(def, &name));
                if !name.is_empty() && (known.is_none() || is_line_number(&name)) {
                    return Err(no_column().into());
                }
                self.column("Итог", &name)?
            }
            Value::Number(n) => usize::try_from(n.trunc_to_i64_saturating())
                .ok()
                .filter(|_| n.is_integer())
                .and_then(|i| self.attributes().nth(i))
                .ok_or_else(|| self.refuse("Итог", "колонка задана не именем"))?,
            _ => return Err(self.refuse("Итог", "колонка задана не именем").into()),
        };
        ex.charge(self.rows.len() as u64)?;
        let cells = self.rows.iter().filter_map(|row| row.get(at));
        let mut sum = Number::from_i64(0);
        match self.atoms(at) {
            [TypeAtom::Ref(_)] => {}
            [TypeAtom::String] if !cells.clone().any(may_be_number) => {}
            [TypeAtom::Number] => {
                for cell in cells {
                    if let Value::Number(n) = cell {
                        sum = sum.add(n);
                    }
                }
            }
            _ => return Err(self.refuse("Итог", "колонка не числового типа").into()),
        }
        Ok(Value::Number(sum))
    }

    fn single(&self, name: &str, needle: &Value) -> Result<usize, HostError> {
        match self.def().and_then(|def| column_index(def, name)) {
            None if !name.is_empty() => Err(HostError::key("core85:IDS_VT_COLUMNCODE_ERROR")),
            Some(at) if is_line_number(name) && matches!(needle, Value::Number(_)) => Ok(at),
            _ => self.column("Найти", name),
        }
    }

    pub(crate) fn find_index(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Option<usize>> {
        let needle = crate::meta::arg_value(ex, args, 0)?;
        let names = match crate::meta::arg_value(ex, args, 1)? {
            Value::Undefined => String::new(),
            Value::String(s) => s.to_string_lossy(),
            _ => return Err(crate::meta::type_mismatch(2).into()),
        };
        let columns: Vec<usize> = if names.is_empty() {
            let written = !matches!(args.get(1), None | Some(Arg::Omitted));
            if written && matches!(needle, Value::Number(_)) {
                return Err(self.refuse("Найти", "число среди всех колонок").into());
            }
            self.attributes().collect()
        } else {
            let names: Vec<&str> = names
                .split(',')
                .map(|name| name.trim_matches(blank))
                .collect();
            match names.as_slice() {
                [name] => vec![self.single(name, &needle)?],
                _ => names
                    .iter()
                    .map(|name| self.column("Найти", name))
                    .collect::<Result<_, _>>()?,
            }
        };
        for (i, row) in self.rows.iter().enumerate() {
            ex.charge(1)?;
            for cell in columns.iter().filter_map(|&c| row.get(c)) {
                bsl_vm::ops::charge_equal(ex, cell, &needle)?;
                if same_value(cell, &needle) {
                    return Ok(Some(i));
                }
            }
        }
        Ok(None)
    }

    fn attribute(&self, name: &str) -> Option<usize> {
        let at = column_index(self.def()?, name)?;
        self.attributes().find(|&i| i == at)
    }

    fn unload_form(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<(Vec<usize>, Lines)> {
        let refused = || self.refuse("Выгрузить", "с параметрами");
        let all = (0..self.def().map_or(0, |def| def.fields.len())).collect();
        let first = crate::meta::arg_value(ex, args, 0)?;
        match args {
            [] => Ok((all, Lines::All)),
            [Arg::Omitted, _] => {
                let Value::String(names) = crate::meta::arg_value(ex, args, 1)? else {
                    return Err(refused().into());
                };
                let mut columns = Vec::new();
                for name in names.to_string_lossy().split(',') {
                    match self.attribute(name.trim_matches(blank)) {
                        Some(at) if !columns.contains(&at) => columns.push(at),
                        _ => return Err(refused().into()),
                    }
                }
                Ok((columns, Lines::All))
            }
            [_] if matches!(first, Value::String(_)) => Err(crate::meta::type_mismatch(1).into()),
            [_] if is_plain(&first, "Массив") => {
                match ex.call_method(&first, "Count", &mut [], true)? {
                    Value::Number(n) if n.is_zero() => Ok((all, Lines::Nothing)),
                    _ => Err(refused().into()),
                }
            }
            [_] if is_plain(&first, "Структура") => {
                match bsl_stdlib::structure_fields(&first).as_deref() {
                    Some([(key, number @ Value::Number(_))]) => {
                        let at = self
                            .attribute(key)
                            .filter(|&at| matches!(self.atoms(at), [TypeAtom::Number]))
                            .ok_or_else(refused)?;
                        Ok((all, Lines::Equal(at, number.clone())))
                    }
                    _ => Err(refused().into()),
                }
            }
            _ => Err(refused().into()),
        }
    }

    pub(crate) fn unload(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let (columns, lines) = self.unload_form(ex, args)?;
        let def = self.def().ok_or_else(|| internal("no such section"))?;
        let picked: Vec<(usize, &FieldDef)> = columns
            .iter()
            .filter_map(|&at| Some((at, def.fields.get(at)?)))
            .collect();
        let line_number = TypeSet::number(0, 0, false);
        let mut types = Vec::with_capacity(picked.len());
        for (_, f) in &picked {
            let set = if f.std == Some(StdAttr::LineNumber) {
                &line_number
            } else {
                &f.types
            };
            match crate::values::typedesc::from_standard(self.app, ex, set) {
                Err(CallError::Host(_)) => {
                    let form = format!("тип колонки {}", f.name);
                    return Err(self.refuse("Выгрузить", &form).into());
                }
                other => types.push(other?),
            }
        }
        let rows: &[Vec<Value>] = if matches!(lines, Lines::Nothing) {
            &[]
        } else {
            self.rows
        };
        let cells = (rows.len() as u64).saturating_mul(picked.len() as u64);
        ex.reserve(
            self.bytes
                .saturating_mul(2)
                .saturating_add(cells.saturating_mul(128)),
        )?;
        let table_type = platform_catalog::catalog()
            .find_type("ValueTable")
            .ok_or_else(|| internal("no value table type"))?;
        let table = ex.construct(table_type, &mut [])?;
        let table_columns = ex.get_member(&table, "Columns")?;
        for ((_, f), ty) in picked.iter().zip(types) {
            let mut add = [Arg::Val(Value::from(f.name.as_str())), Arg::Val(ty)];
            ex.call_method(&table_columns, "Add", &mut add, true)?;
        }
        for row in rows {
            ex.charge(1)?;
            if let Lines::Equal(at, number) = &lines {
                let Some(cell) = row.get(*at) else { continue };
                bsl_vm::ops::charge_equal(ex, cell, number)?;
                if !same_value(cell, number) {
                    continue;
                }
            }
            let line = ex.call_method(&table, "Add", &mut [], true)?;
            for (i, (at, _)) in picked.iter().enumerate() {
                if let Some(cell) = row.get(*at) {
                    let key = Value::from(i64::try_from(i).unwrap_or(i64::MAX));
                    ex.index_set(&line, &key, cell.clone())?;
                }
            }
        }
        Ok(table)
    }
}

enum Lines {
    All,
    Nothing,
    Equal(usize, Value),
}

fn is_plain(v: &Value, name: &str) -> bool {
    platform_catalog::catalog()
        .find_type(name)
        .is_some_and(|idx| v.type_id() == TypeId::of_catalog(idx))
}

fn same_value(a: &Value, b: &Value) -> bool {
    !matches!(
        (a, b),
        (Value::Bool(_), Value::Number(_)) | (Value::Number(_), Value::Bool(_))
    ) && bsl_vm::ops::equal(a, b)
}

pub(crate) fn column_index(def: &TabDef, name: &str) -> Option<usize> {
    let n = fold_name(name);
    def.fields.iter().position(|f| {
        fold_name(&f.name) == n
            || f.std.is_some_and(|a| {
                let (ru, en) = a.names();
                fold_name(ru) == n || fold_name(en) == n
            })
    })
}

fn index_arg(args: &[Arg], ex: &mut Execution) -> CallResult<usize> {
    let v = crate::meta::arg_value(ex, args, 0)?;
    let Value::Number(n) = &v else {
        return Err(crate::meta::type_mismatch(1).into());
    };
    usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| off_bound().into())
}

impl HostObject for ReadTab {
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        crate::read_budget::visit_rows(&self.rows, visit)?;
        Ok(true)
    }
    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        if self
            .app
            .model()
            .obj(self.obj)
            .tabular
            .get(self.tab)
            .is_some_and(|t| t.std_list.is_some())
        {
            return Ok(Some(bsl_prim::Str::from(
                ex.runtime().types().name(self.ty, ex.script_variant()),
            )));
        }
        Ok(None)
    }

    fn get(&self, _: &mut Execution, _: MemberId) -> CallResult<Value> {
        Err(internal("a tabular section has no properties").into())
    }

    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(read_only().into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let method = if let Some(i) = m.0.checked_sub(METHOD_BASE) {
            METHODS.get(i as usize)
        } else {
            let template = ex
                .runtime()
                .types()
                .app_type(self.ty)
                .and_then(|t| t.template);
            let name =
                template.and_then(|t| crate::meta::member_name(platform_catalog::catalog(), t, m));
            match name {
                Some((true, ru, _)) => {
                    let method = METHODS.iter().find(|method| method.ru == ru);
                    if method.is_none() {
                        return Err(HostError::not_implemented(&self.type_name(), ru).into());
                    }
                    method
                }
                _ => None,
            }
        };
        let Some(method) = method else {
            return Err(internal("unknown method of a tabular section").into());
        };
        match method.op {
            Op::Count => Ok(Value::from(
                i64::try_from(self.rows.len()).unwrap_or(i64::MAX),
            )),
            Op::Get => {
                let i = index_arg(args, ex)?;
                self.row(ex, i)
            }
            Op::IndexOf => {
                let v = crate::meta::arg_value(ex, args, 0)?;
                let at = v
                    .as_object()
                    .and_then(|o| o.as_any().downcast_ref::<ReadTabRow>())
                    .filter(|r| Rc::ptr_eq(&r.rows, &self.rows))
                    .map_or(-1, |r| i64::try_from(r.at).unwrap_or(-1));
                Ok(Value::from(at))
            }
            Op::Write => Err(read_only().into()),
            Op::Total => self.total(ex, args),
            Op::Find => self.find(ex, args),
            Op::Unload => self.unload(ex, args),
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
        Some(self.rows.len())
    }

    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        self.row(ex, i)
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let Value::Number(n) = key else {
            return Err(crate::meta::type_mismatch(1).into());
        };
        let i = usize::try_from(n.trunc_to_i64_saturating()).map_err(|_| off_bound())?;
        self.row(ex, i)
    }

    fn index_set(&self, _: &mut Execution, _: &Value, _: Value) -> CallResult<()> {
        Err(read_only().into())
    }
}

struct ReadTabRow {
    app: Arc<Application>,
    obj: ObjId,
    tab: usize,
    ty: TypeId,
    rows: Rc<Vec<Vec<Value>>>,
    at: usize,
    bytes: u64,
}

impl HostObject for ReadTabRow {
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> Result<bool, bsl_vm::Exception> {
        crate::read_budget::visit_rows(&self.rows, visit)?;
        Ok(true)
    }
    fn type_id(&self) -> TypeId {
        self.ty
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        if self
            .app
            .model()
            .obj(self.obj)
            .tabular
            .get(self.tab)
            .is_some_and(|t| t.std_list.is_some())
        {
            return Ok(Some(bsl_prim::Str::from(
                ex.runtime().types().name(self.ty, ex.script_variant()),
            )));
        }
        Ok(None)
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let own = if m.0 >= COLUMN_BASE {
            Some(m)
        } else {
            ex.runtime()
                .types()
                .app_type(self.ty)
                .and_then(|t| t.template)
                .and_then(|t| crate::meta::member_name(platform_catalog::catalog(), t, m))
                .and_then(|(method, ru, _)| (!method).then(|| self.find_member(ru)).flatten())
        };
        let Some(i) = own
            .and_then(|m| m.0.checked_sub(COLUMN_BASE))
            .map(|i| i as usize)
        else {
            return Err(internal("no such column").into());
        };
        let value = self
            .rows
            .get(self.at)
            .and_then(|r| r.get(i))
            .ok_or_else(|| internal("no such column"))?;
        crate::read_budget::reserve(
            ex,
            self.bytes
                .saturating_add(crate::read_budget::value_heap(value)),
        )?;
        Ok(value.clone())
    }

    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(read_only().into())
    }

    fn call(&self, _: &mut Execution, _: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(internal("a row has no methods").into())
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        let def = self.app.model().obj(self.obj).tabular.get(self.tab)?;
        column_index(def, name)
            .and_then(|i| u32::try_from(i).ok())
            .map(|i| MemberId(COLUMN_BASE + i))
    }

    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        (m.0 >= COLUMN_BASE).then_some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
}
