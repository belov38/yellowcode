use crate::http::{http_types_registered, new_request, response_parts};
use crate::repr::repr_in;
use crate::wrapper::eval_wrapper;
use bsl_compiler::{
    CompileError, CompileOptions, CompiledModule, ExecContext, ModuleKind, compile,
};
use bsl_prim::{CompatMode, Str, fold_name};
use bsl_stdlib::{array_items, new_array, new_structure, structure_get};
use bsl_syntax::{MsgId, SourceText};
use bsl_vm::{
    Arg, CallResult, Category, ErrorInfo, Exception, ExecOptions, Execution, FatalKind, HostError,
    HostObject, MemberId, MemberKind, ModuleContext, ModuleRef, Runtime, SessionEnv, TypeId, Value,
    error_info_of,
};
use serde_json::{Map, Value as Json, json};
use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

pub const EVAL_MODULE: &str = "HTTPСервис.Eval.Модуль";

const CATEGORIES: [&str; 7] = [
    "ИсключениеВызванноеИзВстроенногоЯзыка",
    "ОшибкаВоВремяВыполненияВстроенногоЯзыка",
    "ОшибкаКомпиляцииВстроенногоЯзыка",
    "ОшибкаИспользованияВстроенногоЯзыка",
    "НеподдерживаемыйФормат",
    "ОшибкаКонфигурации",
    "ПрочаяОшибка",
];

#[derive(Debug)]
#[non_exhaustive]
pub enum RunResult {
    Answers(Vec<Json>),
    Fatal(String),
    Budget,
    Http { status: u16, body: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunnerError {
    NoModule(String),
    NoHttpTypes,
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunnerError::NoModule(m) => write!(f, "the host has no module {m}"),
            RunnerError::NoHttpTypes => {
                write!(
                    f,
                    "the runtime lacks the HTTP service types of register_http"
                )
            }
        }
    }
}

impl std::error::Error for RunnerError {}

enum Mode {
    Early(Arc<CompiledModule>),
    Module(ModuleRef),
}

pub struct VmRunner {
    rt: Runtime,
    mode: Mode,
}

impl VmRunner {
    pub fn new(rt: Runtime, eval_module: &str) -> Result<VmRunner, CompileError> {
        Self::new_with_compat(rt, eval_module, CompatMode::DontUse)
    }

    pub fn new_with_compat(
        rt: Runtime,
        eval_module: &str,
        compat: CompatMode,
    ) -> Result<VmRunner, CompileError> {
        let text = eval_module.strip_prefix('\u{FEFF}').unwrap_or(eval_module);
        let wrapped = eval_wrapper(text).map_err(|e| CompileError {
            msg: MsgId("EVAL_WRAPPER"),
            arg: Some(e),
            line: 0,
            col: 0,
        })?;
        let name = crate::kinds::display_name(EVAL_MODULE, rt.app().script_variant());
        let o = CompileOptions {
            module_name: &name,
            kind: ModuleKind::HttpService,
            context: ExecContext::Server,
            compat,
            catalog: platform_catalog::catalog(),
            app: rt.app().names(),
            extension: None,
        };
        let module = compile(&SourceText::from_str(&name, &wrapped), &o)?;
        Ok(VmRunner {
            rt,
            mode: Mode::Early(module),
        })
    }

    pub fn for_module(rt: Runtime, module: &str) -> Result<VmRunner, RunnerError> {
        let r = rt
            .app()
            .module_ref(module)
            .ok_or_else(|| RunnerError::NoModule(module.to_owned()))?;
        if !http_types_registered(&rt) {
            return Err(RunnerError::NoHttpTypes);
        }
        Ok(VmRunner {
            rt,
            mode: Mode::Module(r),
        })
    }

    pub fn runtime(&self) -> &Runtime {
        &self.rt
    }

    pub fn run_body(&self, env: Arc<dyn SessionEnv>, body: &[u8], opts: &ExecOptions) -> RunResult {
        let mut ex = self.rt.execution(env, opts.clone());
        self.run_body_in(&mut ex, body)
    }

    pub fn run_body_in(&self, ex: &mut Execution, body: &[u8]) -> RunResult {
        let Mode::Module(m) = &self.mode else {
            return RunResult::Fatal(
                "\u{FEFF}VmRunner::run_body needs a module-mode runner".into(),
            );
        };
        let app = self.rt.app().clone();
        let req = new_request(
            "POST",
            &[("Content-Type".into(), "application/json".into())],
            body.to_vec(),
        );
        let returned = app
            .module(ex, *m)
            .and_then(|inst| ex.call(&inst, "AnyPost", &mut [Arg::Val(req)]));
        match returned {
            Ok(v) => match response_parts(&v) {
                Some((200, b)) => match serde_json::from_slice::<Json>(&b) {
                    Ok(Json::Array(a)) => RunResult::Answers(a),
                    _ => RunResult::Http {
                        status: 200,
                        body: String::from_utf8_lossy(&b).into_owned(),
                    },
                },
                Some((status, b)) => RunResult::Http {
                    status,
                    body: String::from_utf8_lossy(&b).into_owned(),
                },
                None => RunResult::Fatal("\u{FEFF}AnyPost returned no HTTP response".into()),
            },
            Err(e) => fatal_result(e),
        }
    }

    pub fn run(
        &self,
        env: Arc<dyn SessionEnv>,
        cases: &[(String, String)],
        opts: &ExecOptions,
    ) -> RunResult {
        let Mode::Early(module) = &self.mode else {
            let list: Vec<Json> = cases
                .iter()
                .map(|(id, code)| json!({"id": id, "code": code}))
                .collect();
            let body = json!({ "cases": list }).to_string();
            return self.run_body(env, body.as_bytes(), opts);
        };
        let _table = TableGuard::install();
        let mut ex = self.rt.execution(env, opts.clone());
        let list = new_array(
            cases
                .iter()
                .map(|(id, code)| {
                    new_structure(vec![
                        ("id", Value::from(id.as_str())),
                        ("code", Value::from(code.as_str())),
                    ])
                })
                .collect(),
        );
        let returned = ex
            .instantiate(module, ModuleContext::None)
            .and_then(|inst| ex.call(&inst, "AnyPost", &mut [Arg::Val(list)]));
        match returned {
            Ok(v) => RunResult::Answers(answers(&v)),
            Err(e) => fatal_result(e),
        }
    }
}

fn fatal_result(e: Exception) -> RunResult {
    match e {
        Exception::Fatal(f) if f.kind == FatalKind::Budget => RunResult::Budget,
        Exception::Fatal(f) => RunResult::Fatal(format!("\u{FEFF}{}", f.text)),
        Exception::Catchable(info) => {
            RunResult::Fatal(format!("\u{FEFF}{}", info.description.to_string_lossy()))
        }
    }
}

fn text_of(v: Option<Value>) -> String {
    match v {
        Some(Value::String(s)) => s.to_string_lossy(),
        _ => String::new(),
    }
}

fn answers(returned: &Value) -> Vec<Json> {
    let mut out = Vec::new();
    for a in array_items(returned).unwrap_or_default() {
        let id = text_of(structure_get(&a, "id"));
        let mut m = Map::new();
        m.insert("id".into(), json!(id));
        let ok = matches!(structure_get(&a, "ok"), Some(Value::Bool(true)));
        m.insert("ok".into(), json!(ok));
        if let Some(fields) =
            TABLE.with(|t| t.borrow_mut().get_mut(&id).and_then(VecDeque::pop_front))
        {
            m.extend(fields);
        }
        if let Some(log) = structure_get(&a, "log").and_then(|l| array_items(&l)) {
            let log: Vec<Json> = log.into_iter().map(|e| json!(text_of(Some(e)))).collect();
            m.insert("log".into(), Json::Array(log));
        }
        m.insert("ms".into(), json!(0));
        out.push(Json::Object(m));
    }
    out
}

type Fields = Map<String, Json>;

thread_local! {
    static TABLE: RefCell<HashMap<String, VecDeque<Fields>>> = RefCell::new(HashMap::new());
}

struct TableGuard;

impl TableGuard {
    fn install() -> TableGuard {
        TABLE.with(|t| t.borrow_mut().clear());
        TableGuard
    }
}

impl Drop for TableGuard {
    fn drop(&mut self) {
        TABLE.with(|t| t.borrow_mut().clear());
    }
}

fn put(id: String, fields: Fields) {
    TABLE.with(|t| t.borrow_mut().entry(id).or_default().push_back(fields));
}

pub fn stand_object() -> Value {
    Value::Object(Rc::new(StandNative))
}

struct StandNative;

const DESCRIBE_VALUE: u32 = 0;
const DESCRIBE_ERROR: u32 = 1;
const PREDST: u32 = 2;

fn arg(ex: &mut Execution, args: &[Arg], i: usize) -> CallResult<Value> {
    match args.get(i) {
        None | Some(Arg::Omitted) => Ok(Value::Undefined),
        Some(Arg::Val(v)) => Ok(v.clone()),
        Some(Arg::Place(p)) => Ok(p.get(ex)?),
    }
}

impl HostObject for StandNative {
    fn type_id(&self) -> TypeId {
        TypeId::UNDEFINED
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, _: MemberId) -> CallResult<Value> {
        Err(HostError::text("Нет свойств", "No properties").into())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::text("Нет свойств", "No properties").into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match m.0 {
            DESCRIBE_VALUE => {
                let (v, answer) = (arg(ex, args, 0)?, arg(ex, args, 1)?);
                let fields = describe_value(ex, &v)?;
                put(text_of(structure_get(&answer, "id")), fields);
                Ok(Value::Undefined)
            }
            DESCRIBE_ERROR => {
                let (info, answer) = (arg(ex, args, 0)?, arg(ex, args, 1)?);
                let Some(info) = error_info_of(&info) else {
                    return Err(HostError::text(
                        "Ожидается ИнформацияОбОшибке",
                        "Error info expected",
                    )
                    .into());
                };
                let fields = describe_error(ex, &info)?;
                put(text_of(structure_get(&answer, "id")), fields);
                Ok(Value::Undefined)
            }
            _ => {
                let v = arg(ex, args, 0)?;
                Ok(Value::String(Str::from(repr_in(ex, &v, 0)?.as_str())))
            }
        }
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        match fold_name(name).as_ref() {
            "описатьзначение" => Some(MemberId(DESCRIBE_VALUE)),
            "описатьошибку" => Some(MemberId(DESCRIBE_ERROR)),
            "предст" => Some(MemberId(PREDST)),
            _ => None,
        }
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        Some(MemberKind::Method {
            is_function: m.0 == PREDST,
            min_args: if m.0 == PREDST { 1 } else { 2 },
            max_args: Some(if m.0 == PREDST { 1 } else { 2 }),
        })
    }
}

fn describe_value(ex: &mut Execution, v: &Value) -> Result<Fields, Exception> {
    let mut f = Fields::new();
    let t = ex.present(&Value::Type(v.type_id()))?;
    f.insert("t".into(), json!(t.to_string_lossy()));
    f.insert("r".into(), json!(repr_in(ex, v, 0)?));
    match ex.present(v) {
        Ok(s) => f.insert("s".into(), json!(s.to_string_lossy())),
        Err(Exception::Catchable(info)) => f.insert("s_err".into(), json!(info.brief())),
        Err(e) => return Err(e),
    };
    Ok(f)
}

fn describe_error(ex: &mut Execution, info: &Rc<ErrorInfo>) -> Result<Fields, Exception> {
    let mut f = Fields::new();
    f.insert("err".into(), json!(info.description.to_string_lossy()));
    f.insert("brief".into(), json!(info.brief()));
    let detailed = info.detailed(ex.env().compat(), ex.lang());
    f.insert("detail".into(), json!(detailed));
    f.insert("line".into(), json!(info.line));
    f.insert("module".into(), json!(info.module_name.to_string_lossy()));
    f.insert("src".into(), json!(info.source_line.to_string_lossy()));
    f.insert("code".into(), json!(info.code.to_string_lossy()));
    let cat: Vec<&str> = CATEGORIES
        .iter()
        .copied()
        .filter(|n| Category::from_name(n).is_some_and(|c| info.is_category(c, true)))
        .collect();
    f.insert("cat".into(), json!(cat.join(",")));
    if !matches!(info.additional, Value::Undefined) {
        f.insert("addinfo".into(), json!(repr_in(ex, &info.additional, 0)?));
    }
    let mut causes = Vec::new();
    let mut next = info.cause.clone();
    while let Some(c) = next {
        if causes.len() >= 8 {
            break;
        }
        causes.push(json!({
            "err": c.description.to_string_lossy(),
            "module": c.module_name.to_string_lossy(),
            "line": c.line,
            "src": c.source_line.to_string_lossy(),
        }));
        next = c.cause.clone();
    }
    if !causes.is_empty() {
        f.insert("cause".into(), Json::Array(causes));
    }
    Ok(f)
}
