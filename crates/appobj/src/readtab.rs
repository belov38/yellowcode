use crate::Application;
use crate::model::{ObjId, TabDef};
use crate::read::read_tabular;
use crate::values::refs::RefValue;
use bsl_prim::fold_name;
use bsl_vm::{
    Arg, CallResult, Category, CategorySet, Execution, HostError, HostObject, MemberId, MemberKind,
    TypeId, Value,
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
    m("Выгрузить", "Unload", true, 0, None, Op::Later),
    m("ВыгрузитьКолонку", "UnloadColumn", true, 1, None, Op::Later),
    m("Найти", "Find", true, 0, None, Op::Later),
    m("НайтиСтроки", "FindRows", true, 0, None, Op::Later),
    m("Итог", "Total", true, 0, None, Op::Later),
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

pub(crate) fn retained_bytes(value: &Value) -> u64 {
    value
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<ReadTab>())
        .map_or(0, |t| t.bytes)
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
        let n = fold_name(name);
        def.fields
            .iter()
            .position(|f| {
                fold_name(&f.name) == n
                    || f.std.is_some_and(|a| {
                        let (ru, en) = a.names();
                        fold_name(ru) == n || fold_name(en) == n
                    })
            })
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
