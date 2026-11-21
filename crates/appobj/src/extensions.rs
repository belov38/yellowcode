use crate::Application;
use crate::meta::{arg_value, member_name, no_field, type_mismatch};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, Value,
};
use ibschema::store::Store;
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

const MANAGER: &str = "МенеджерРасширенийКонфигурации";
const SOURCE: &str = "ИсточникРасширенийКонфигурации";
const EXTENSION: &str = "РасширениеКонфигурации";
const PURPOSE: &str = "НазначениеРасширенияКонфигурации";
const SCOPE: &str = "ОбластьДействияРасширенияКонфигурации";
const PROTECTION: &str = "ОписаниеЗащитыОтОпасныхДействий";

const TABLES: [(&str, &str); 2] = [
    ("ExtensionsInfo", "чтение ExtensionsInfo не удалось"),
    ("ExtensionsInfoNGS", "чтение ExtensionsInfoNGS не удалось"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Presence {
    Absent,
    Present,
    Unknown(&'static str),
}

pub(crate) fn presence(store: &dyn Store) -> Presence {
    let map = store.storage();
    if map.table_by_name(TABLES[0].0).is_none() {
        return Presence::Unknown("ExtensionsInfo не описана в схеме базы");
    }
    let Ok(txn) = store.read() else {
        return Presence::Unknown("чтение базы не удалось");
    };
    for (name, unreadable) in TABLES {
        let Some(table) = map.table_by_name(name) else {
            continue;
        };
        let Ok(mut rows) = txn.scan(table) else {
            return Presence::Unknown(unreadable);
        };
        match rows.next() {
            Some(Ok(_)) => return Presence::Present,
            Some(Err(_)) => return Presence::Unknown(unreadable),
            None => {}
        }
    }
    Presence::Absent
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) {
    let catalog = catalog();
    let (Some(ty), Some(source)) = (catalog.find_type(MANAGER), catalog.find_type(SOURCE)) else {
        return;
    };
    let app = app.clone();
    reg.global_prop(
        "РасширенияКонфигурации",
        "ConfigurationExtensions",
        Arc::new(move |_| {
            Ok(Value::Object(Rc::new(Manager {
                app: app.clone(),
                ty,
                source,
            })))
        }),
    );
}

struct Manager {
    app: Arc<Application>,
    ty: TypeIdx,
    source: TypeIdx,
}

fn refusal(member: &str) -> HostError {
    HostError::not_implemented(MANAGER, member)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Default,
    Database,
    SessionApplied,
    SessionDisabled,
    Other,
}

impl Manager {
    fn check_get(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Source> {
        if !matches!(arg_value(ex, args, 0)?, Value::Undefined) {
            return Err(refusal("Получить: Отбор").into());
        }
        match arg_value(ex, args, 1)? {
            Value::Undefined => Ok(Source::Default),
            v => match bsl_stdlib::enums::as_enum_value(&v) {
                Some(e) if e.type_idx() == self.source => Ok(match e.name_ru() {
                    "БазаДанных" => Source::Database,
                    "СеансАктивные" => Source::SessionApplied,
                    "СеансОтключенные" => Source::SessionDisabled,
                    _ => Source::Other,
                }),
                _ => Err(type_mismatch(2).into()),
            },
        }
    }

    fn none(&self, member: &str) -> CallResult<Value> {
        match self.app.extensions() {
            Presence::Absent => Ok(bsl_stdlib::new_array(Vec::new())),
            Presence::Present => Err(refusal(&format!("{member}: в базе есть расширения")).into()),
            Presence::Unknown(why) => Err(refusal(&format!("{member}: {why}")).into()),
        }
    }

    fn get(&self, member: &str, source: Source) -> CallResult<Value> {
        if *self.app.extensions() != Presence::Present {
            return self.none(member);
        }
        let refuse =
            |why: String| -> CallResult<Value> { Err(refusal(&format!("{member}: {why}")).into()) };
        let all = match installed(&self.app) {
            Ok(all) => all,
            Err(why) => return refuse(why),
        };
        let inactive = all.iter().find_map(|e| match &e.info {
            Ok(i) if i.active => None,
            Ok(_) => Some(format!("расширение «{}» не активно", e.name)),
            Err(why) => Some(format!("расширение «{}»: {why}", e.name)),
        });
        let chosen: Vec<&crate::ExtensionFacts> = match (source, inactive) {
            (Source::Database, _) => all,
            (Source::Other, _) => return refuse("источник не поддержан".into()),
            (_, Some(why)) => return refuse(why),
            (Source::Default | Source::SessionApplied, None) => all,
            (Source::SessionDisabled, None) => Vec::new(),
        };
        let Some(ty) = catalog().find_type(EXTENSION) else {
            return refuse("нет типа РасширениеКонфигурации".into());
        };
        Ok(bsl_stdlib::new_array(
            chosen
                .into_iter()
                .map(|e| {
                    Value::Object(Rc::new(ExtensionObj {
                        ty,
                        facts: e.clone(),
                    }))
                })
                .collect(),
        ))
    }
}

fn installed(app: &Application) -> std::result::Result<Vec<&crate::ExtensionFacts>, String> {
    let facts = &app.options().extensions;
    if facts.is_empty() {
        return Err("в базе есть расширения".into());
    }
    if let Some(why) = app
        .extension_refusals
        .twin
        .get_or_init(|| twin_rows(app.unscoped_store()))
    {
        return Err(why.clone());
    }
    let mut keyed = Vec::with_capacity(facts.len());
    for e in facts {
        match &e.row {
            Ok(row) => keyed.push(((row.order, row.id), e)),
            Err(why) => return Err(format!("строка расширения «{}»: {why}", e.name)),
        }
    }
    keyed.sort_by_key(|(key, _)| *key);
    Ok(keyed.into_iter().map(|(_, e)| e).collect())
}

fn twin_rows(store: &dyn Store) -> Option<String> {
    let map = store.storage();
    let (name, unreadable) = TABLES[1];
    let table = map.table_by_name(name)?;
    let Ok(txn) = store.read() else {
        return Some("чтение базы не удалось".into());
    };
    let Ok(mut rows) = txn.scan(table) else {
        return Some(unreadable.into());
    };
    match rows.next() {
        None => None,
        Some(Ok(_)) => Some("в ExtensionsInfoNGS есть строки".into()),
        Some(Err(_)) => Some(unreadable.into()),
    }
}

impl HostObject for Manager {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(no_field(&m.0.to_string()).into())
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(no_field(&m.0.to_string()).into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member_name(catalog(), self.ty, m) {
            Some((true, ru, "Get")) => {
                let source = self.check_get(ex, args)?;
                Manager::get(self, ru, source)
            }
            Some((true, ru, "GetSessionApplicationIssuesInformation")) => self.none(ru),
            Some((true, ru, _)) => Err(refusal(ru).into()),
            _ => Err(no_field(&m.0.to_string()).into()),
        }
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(0x6578_7400 | i32::from(self.ty.0))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<Manager>()
                .is_some_and(|o| Arc::ptr_eq(&o.app, &self.app)),
        )
    }
}

struct ExtensionObj {
    ty: TypeIdx,
    facts: crate::ExtensionFacts,
}

fn extension_refusal(member: &str) -> HostError {
    HostError::not_implemented(EXTENSION, member)
}

fn enum_of(ty: &str, name: &str) -> Option<Value> {
    bsl_stdlib::enums::enum_value(catalog().find_type(ty)?, name)
}

impl ExtensionObj {
    fn info(&self, member: &str) -> std::result::Result<&crate::ExtensionInfo, HostError> {
        self.facts
            .info
            .as_ref()
            .map_err(|why| extension_refusal(&format!("{member}: {why}")))
    }

    fn row(&self, member: &str) -> std::result::Result<&crate::ExtensionRow, HostError> {
        self.facts
            .row
            .as_ref()
            .map_err(|why| extension_refusal(&format!("{member}: {why}")))
    }

    fn property(&self, ex: &mut Execution, ru: &str) -> CallResult<Value> {
        let unobserved = |what: &str| -> CallResult<Value> {
            Err(extension_refusal(&format!("{ru}: {what}")).into())
        };
        match ru {
            "Имя" => Ok(Value::from(self.facts.name.as_str())),
            "Синоним" => match self.info(ru)?.synonym.as_slice() {
                [(_, text)] => Ok(Value::from(text.as_str())),
                _ => unobserved("синоним не на одном языке"),
            },
            "Версия" => match &self.facts.version {
                Ok(v) => Ok(Value::from(v.as_str())),
                Err(why) => unobserved(why),
            },
            "Назначение" => match self.row(ru)?.purpose_code {
                1 => enum_of(PURPOSE, "Адаптация").map_or_else(|| unobserved("нет значения"), Ok),
                2 => enum_of(PURPOSE, "Дополнение").map_or_else(|| unobserved("нет значения"), Ok),
                n => unobserved(&format!("код назначения {n}")),
            },
            "ОбластьДействия" => match self.row(ru)?.scope_code {
                1 => enum_of(SCOPE, "ИнформационнаяБаза")
                    .map_or_else(|| unobserved("нет значения"), Ok),
                n => unobserved(&format!("код области действия {n}")),
            },
            "ИспользуетсяВРаспределеннойИнформационнойБазе" => {
                Ok(Value::Bool(self.row(ru)?.distributed))
            }
            "УникальныйИдентификатор" => {
                Ok(bsl_stdlib::values::new_uuid(*self.row(ru)?.id.as_bytes()))
            }
            "ХешСумма" => Ok(bsl_stdlib::values::new_binary_data(
                self.info(ru)?.hash.to_vec(),
            )),
            "БезопасныйРежим" => {
                let info = self.info(ru)?;
                if !info.security_profile.is_empty() {
                    return unobserved("профиль безопасности");
                }
                Ok(Value::Bool(info.safe_mode))
            }
            "ЗащитаОтОпасныхДействий" => {
                if !self.info(ru)?.unsafe_action_warnings {
                    return unobserved("без предупреждений об опасных действиях");
                }
                let Some(ty) = catalog().find_type(PROTECTION) else {
                    return unobserved("нет типа");
                };
                Ok(ex.construct(ty, &mut [])?)
            }
            "Активно" => Ok(Value::Bool(self.info(ru)?.active)),
            "ИспользоватьОсновныеРолиДляВсехПользователей" =>
            {
                if self.info(ru)?.main_roles {
                    return unobserved("значение 1 в ExtensionZippedInfo");
                }
                Ok(Value::Bool(false))
            }
            _ => Err(extension_refusal(ru).into()),
        }
    }
}

impl HostObject for ExtensionObj {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match member_name(catalog(), self.ty, m) {
            Some((false, ru, _)) => self.property(ex, ru),
            _ => Err(no_field(&m.0.to_string()).into()),
        }
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        match member_name(catalog(), self.ty, m) {
            Some((_, ru, _)) => Err(extension_refusal(&format!("{ru}: запись")).into()),
            None => Err(no_field(&m.0.to_string()).into()),
        }
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        match member_name(catalog(), self.ty, m) {
            Some((true, ru, "GetData")) => {
                Err(extension_refusal(&format!("{ru}: данные расширения не собираются")).into())
            }
            Some((true, ru, _)) => Err(extension_refusal(ru).into()),
            _ => Err(no_field(&m.0.to_string()).into()),
        }
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        Ok(Some(bsl_prim::Str::from(self.facts.name.as_str())))
    }
    fn boost_hash(&self) -> Option<i32> {
        let id = self
            .facts
            .row
            .as_ref()
            .map(|r| *r.id.as_bytes())
            .unwrap_or_default();
        Some(i32::from_le_bytes([id[0], id[1], id[2], id[3]]))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<ExtensionObj>()
                .is_some_and(|o| o.facts.row.is_ok() && o.facts.row == self.facts.row),
        )
    }
}

pub(crate) fn extension_value(facts: &crate::ExtensionFacts) -> CallResult<Value> {
    let ty = catalog()
        .find_type(EXTENSION)
        .ok_or_else(|| extension_refusal("нет типа"))?;
    Ok(Value::Object(Rc::new(ExtensionObj {
        ty,
        facts: facts.clone(),
    })))
}

pub(crate) fn metadata_facts(
    app: &Application,
) -> std::result::Result<Option<Vec<(&crate::ExtensionFacts, &crate::ExtensionStructure)>>, String>
{
    match app.extensions() {
        Presence::Absent => return Ok(None),
        Presence::Unknown(why) => return Err((*why).to_string()),
        Presence::Present => {}
    }
    let facts = &app.options().extensions;
    if facts.is_empty() {
        return Err("в базе есть расширения, а факты расширений не переданы".into());
    }
    if let Some(why) = app
        .extension_refusals
        .twin
        .get_or_init(|| twin_rows(app.unscoped_store()))
    {
        return Err(why.clone());
    }
    for e in facts {
        match &e.info {
            Ok(i) if i.active => {}
            Ok(_) => return Err(format!("расширение «{}» не активно", e.name)),
            Err(why) => return Err(format!("расширение «{}»: {why}", e.name)),
        }
    }
    facts
        .iter()
        .map(|e| match &e.structure {
            Ok(s) => Ok((e, s)),
            Err(why) => Err(format!("расширение «{}»: {why}", e.name)),
        })
        .collect::<std::result::Result<Vec<_>, String>>()
        .map(Some)
}

pub(crate) const MAX_NODES: usize = 20_000;
pub(crate) const MAX_DEPTH: usize = 64;

const WRITE_EVENTS: [(&str, &str); 8] = [
    ("ПередЗаписью", "BeforeWrite"),
    ("ПриЗаписи", "OnWrite"),
    ("ПриУстановкеНовогоКода", "OnSetNewCode"),
    ("ПриУстановкеНовогоНомера", "OnSetNewNumber"),
    ("ПередУдалением", "BeforeDelete"),
    ("ОбработкаПроверкиЗаполнения", "FillCheckProcessing"),
    ("ОбработкаПроведения", "Posting"),
    ("ОбработкаУдаленияПроведения", "UndoPosting"),
];

#[derive(Default)]
pub(crate) struct Refusals {
    twin: OnceLock<Option<String>>,
    global: OnceLock<Option<String>>,
    index: OnceLock<std::result::Result<Index, String>>,
    owners: std::sync::Mutex<
        std::collections::HashMap<(crate::model::ObjId, &'static str), Option<String>>,
    >,
}

pub(crate) fn whole_refusal(facts: &[crate::ExtensionFacts]) -> Option<String> {
    facts.iter().find_map(|e| {
        if let Some(why) = &e.unreadable {
            Some(format!("расширение «{}» не прочитано: {why}", e.name))
        } else {
            e.subscriptions
                .first()
                .map(|s| format!("расширение «{}» подписано на событие ({s})", e.name))
        }
    })
}

pub(crate) fn blocks_all(app: &Application) -> Option<String> {
    app.extension_refusals
        .global
        .get_or_init(|| whole_refusal(&app.options().extensions))
        .clone()
}

pub(crate) struct RefusingStore {
    pub(crate) inner: Arc<dyn Store>,
    pub(crate) why: String,
}

impl Store for RefusingStore {
    fn storage(&self) -> &ibschema::storage::StorageMap {
        self.inner.storage()
    }
    fn read(&self) -> ibschema::store::StoreResult<Box<dyn ibschema::store::ReadTxn + Send>> {
        self.inner.read()
    }
    fn write(
        &self,
        _wait: std::time::Duration,
    ) -> ibschema::store::StoreResult<Box<dyn ibschema::store::WriteTxn>> {
        Err(ibschema::store::StoreError::Unsupported(self.why.clone()))
    }
}

pub(crate) fn write_refusal(
    app: &Application,
    def: &crate::model::ObjDef,
    category: &'static str,
) -> Option<String> {
    if app.options().extensions.is_empty() {
        return (*app.extensions() == Presence::Present)
            .then(|| "в базе есть расширения, а факты расширений не переданы".to_string());
    }
    if let Some(why) = blocks_all(app) {
        return Some(why);
    }
    if app
        .options()
        .extensions
        .iter()
        .all(|e| e.intercepts.is_empty())
    {
        return None;
    }
    let key = (def.id, category);
    if let Ok(owners) = app.extension_refusals.owners.lock()
        && let Some(known) = owners.get(&key)
    {
        return known.clone();
    }
    let answer = match app
        .extension_refusals
        .index
        .get_or_init(|| Index::build(app))
    {
        Err(why) => Some(why.clone()),
        Ok(index) => index.refusal(app, def, category),
    };
    if let Ok(mut owners) = app.extension_refusals.owners.lock() {
        owners.insert(key, answer.clone());
    }
    answer
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Dot,
    Open,
    Other,
}

fn tokens(text: &str) -> std::result::Result<Vec<Tok>, String> {
    let chars: Vec<char> = text.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let ident = |c: char| c == '_' || c.is_alphanumeric();
    let to_eol = |mut i: usize| {
        while at(i).is_some_and(|c| c != '\n') {
            i += 1;
        }
        i
    };
    let skip_ident = |mut i: usize| {
        while at(i).is_some_and(ident) {
            i += 1;
        }
        i
    };
    let mut out = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    while let Some(c) = at(i) {
        if c == '\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let at_line_start = line_start;
        line_start = false;
        if (c == '#' && at_line_start) || (c == '/' && at(i + 1) == Some('/')) {
            i = to_eol(i);
            continue;
        }
        if c == '"' || c == '\'' {
            let close = c;
            i += 1;
            loop {
                match at(i) {
                    None => return Err("незакрытая строка".into()),
                    Some(x) if x == close => {
                        if close == '"' && at(i + 1) == Some('"') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            out.push(Tok::Other);
            continue;
        }
        if c == '&' || c == '~' {
            i = skip_ident(i + 1);
            continue;
        }
        if ident(c) {
            let start = i;
            i = skip_ident(i);
            let word: String = chars.iter().skip(start).take(i - start).collect();
            out.push(Tok::Ident(bsl_prim::fold_name(&word)));
            continue;
        }
        out.push(match c {
            '.' => Tok::Dot,
            '(' => Tok::Open,
            _ => Tok::Other,
        });
        i += 1;
    }
    Ok(out)
}

struct Method {
    export: bool,
    body: std::ops::Range<usize>,
}

struct Text {
    tokens: Vec<Tok>,
    methods: std::collections::HashMap<String, Method>,
}

fn is(t: Option<&Tok>, words: &[&str]) -> bool {
    matches!(t, Some(Tok::Ident(w)) if words.contains(&w.as_str()))
}

fn split(text: &str) -> std::result::Result<Text, String> {
    let tokens = tokens(text)?;
    let mut methods = std::collections::HashMap::new();
    let mut i = 0;
    while i < tokens.len() {
        if is(
            tokens.get(i),
            &["процедура", "функция", "procedure", "function"],
        ) {
            let Some(Tok::Ident(name)) = tokens.get(i + 1) else {
                return Err("метод без имени".into());
            };
            if tokens.get(i + 2) != Some(&Tok::Open) {
                return Err(format!("метод {name} без параметров"));
            }
            let end = (i + 3..tokens.len())
                .find(|&k| {
                    is(
                        tokens.get(k),
                        &[
                            "конецпроцедуры",
                            "конецфункции",
                            "endprocedure",
                            "endfunction",
                        ],
                    )
                })
                .ok_or(format!("метод {name} не закрыт"))?;
            if (i + 3..end).any(|k| {
                is(
                    tokens.get(k),
                    &["процедура", "функция", "procedure", "function"],
                )
            }) {
                return Err(format!("метод {name} не закрыт"));
            }
            let export = (i + 3..end).any(|k| is(tokens.get(k), &["экспорт", "export"]));
            if methods
                .insert(
                    name.clone(),
                    Method {
                        export,
                        body: i + 3..end,
                    },
                )
                .is_some()
            {
                return Err(format!("метод {name} объявлен дважды"));
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
    Ok(Text { tokens, methods })
}

struct Index {
    modules: Vec<(String, std::result::Result<Text, String>)>,
    by_name: std::collections::HashMap<String, usize>,
    exports: std::collections::HashMap<String, Vec<usize>>,
    targets: std::collections::HashMap<(usize, String), (String, String, String)>,
}

const NOT_A_CALL: &[&str] = &[
    "если",
    "иначеесли",
    "пока",
    "для",
    "возврат",
    "и",
    "или",
    "не",
    "новый",
    "вызватьисключение",
    "тогда",
    "иначе",
    "цикл",
    "каждого",
    "из",
    "по",
    "if",
    "elsif",
    "while",
    "for",
    "return",
    "and",
    "or",
    "not",
    "new",
    "raise",
    "then",
    "else",
    "do",
    "each",
    "in",
    "to",
    "ждать",
    "await",
];
const DYNAMIC: &[&str] = &["выполнить", "вычислить", "execute", "eval"];
const THIS: &[&str] = &["этотобъект", "thisobject"];

impl Index {
    fn build(app: &Application) -> std::result::Result<Index, String> {
        let names = app.names();
        let mut modules = Vec::new();
        let mut by_name = std::collections::HashMap::new();
        let mut exports: std::collections::HashMap<String, Vec<usize>> = Default::default();
        for i in 0..names.module_count() {
            let Some(m) = names.module_at(i) else {
                continue;
            };
            let at = modules.len();
            by_name.insert(bsl_prim::fold_name(&m.name), at);
            let text = std::str::from_utf8(m.text)
                .map_err(|_| "текст модуля не UTF-8".to_string())
                .and_then(split);
            if let Ok(t) = &text
                && matches!(
                    m.kind,
                    bsl_compiler::ModuleKind::Object
                        | bsl_compiler::ModuleKind::Manager
                        | bsl_compiler::ModuleKind::RecordSet
                        | bsl_compiler::ModuleKind::ValueManager
                )
            {
                for (name, method) in &t.methods {
                    if method.export {
                        exports.entry(name.clone()).or_default().push(at);
                    }
                }
            }
            modules.push((m.name, text));
        }
        let mut targets = std::collections::HashMap::new();
        for e in &app.options().extensions {
            for x in &e.intercepts {
                let module = app
                    .modules()
                    .module_ref(&x.module)
                    .and_then(|r| app.modules().name_of(r))
                    .and_then(|n| by_name.get(&bsl_prim::fold_name(n)).copied())
                    .ok_or_else(|| {
                        format!(
                            "расширение «{}» перехватывает метод модуля {}, которого нет в конфигурации",
                            e.name, x.module
                        )
                    })?;
                targets.insert(
                    (module, bsl_prim::fold_name(&x.target)),
                    (e.name.clone(), x.annotation.clone(), x.target.clone()),
                );
            }
        }
        Ok(Index {
            modules,
            by_name,
            exports,
            targets,
        })
    }

    fn module_of(&self, name: &str) -> Option<usize> {
        self.by_name.get(&bsl_prim::fold_name(name)).copied()
    }

    fn text(&self, m: usize) -> std::result::Result<&Text, String> {
        match self.modules.get(m) {
            Some((_, Ok(t))) => Ok(t),
            Some((name, Err(why))) => Err(format!("модуль {name} не разобран: {why}")),
            None => Err("модуль не найден".into()),
        }
    }

    fn label(&self, (m, method): &(usize, String)) -> String {
        format!(
            "{}.{method}",
            self.modules.get(*m).map_or("?", |(n, _)| n.as_str())
        )
    }

    fn calls(
        &self,
        app: &Application,
        (m, method): &(usize, String),
    ) -> std::result::Result<Vec<(usize, String)>, String> {
        use bsl_compiler::{AppGlobal, AppNames, ExecContext};
        let text = self.text(*m)?;
        let module_name = self.modules.get(*m).map_or("", |(n, _)| n.as_str());
        let Some(body) = text.methods.get(method) else {
            return Err(format!("в модуле {module_name} нет метода {method}"));
        };
        let names = app.names();
        let t = &text.tokens;
        let mut out = Vec::new();
        for i in body.body.clone() {
            if let Some(Tok::Ident(name)) = t.get(i)
                && matches!(name.as_str(), "выполнить" | "execute")
                && i.checked_sub(1).and_then(|k| t.get(k)) != Some(&Tok::Dot)
            {
                return Err(format!(
                    "динамический вызов {name} в {module_name}.{method}"
                ));
            }
            let (Some(Tok::Ident(name)), Some(Tok::Open)) = (t.get(i), t.get(i + 1)) else {
                continue;
            };
            let before = i.checked_sub(1).and_then(|k| t.get(k));
            if before == Some(&Tok::Dot) {
                let receiver = match (
                    i.checked_sub(2).and_then(|k| t.get(k)),
                    i.checked_sub(3).and_then(|k| t.get(k)),
                ) {
                    (Some(Tok::Ident(r)), prev) if prev != Some(&Tok::Dot) => Some(r.as_str()),
                    _ => None,
                };
                if let Some(r) = receiver {
                    if THIS.contains(&r) {
                        if text.methods.contains_key(name) {
                            out.push((*m, name.clone()));
                        }
                        continue;
                    }
                    if let Some(AppGlobal::CommonModule { name: common }) =
                        names.global_value(r, ExecContext::Server)
                    {
                        let target = self
                            .module_of(&format!("ОбщийМодуль.{common}.Модуль"))
                            .ok_or_else(|| format!("общий модуль {common} не найден"))?;
                        if !self.text(target)?.methods.contains_key(name) {
                            return Err(format!("в общем модуле {common} нет метода {name}"));
                        }
                        out.push((target, name.clone()));
                        continue;
                    }
                }
                for &target in self.exports.get(name).into_iter().flatten() {
                    out.push((target, name.clone()));
                }
                continue;
            }
            if is(before, &["новый", "new"]) || NOT_A_CALL.contains(&name.as_str()) {
                continue;
            }
            if DYNAMIC.contains(&name.as_str()) {
                return Err(format!(
                    "динамический вызов {name} в {module_name}.{method}"
                ));
            }
            if text.methods.contains_key(name) {
                out.push((*m, name.clone()));
                continue;
            }
            if names.context_method(module_name, name)
                || catalog().find_global_method(name).is_some()
            {
                continue;
            }
            if let Some(sig) = names.global_method(name, ExecContext::Server) {
                let target = self
                    .module_of(&format!("ОбщийМодуль.{}.Модуль", sig.module))
                    .or_else(|| self.module_of(&sig.module))
                    .ok_or_else(|| format!("глобальный модуль {} не найден", sig.module))?;
                out.push((target, bsl_prim::fold_name(&sig.name)));
                continue;
            }
            return Err(format!("неизвестное имя {name} в {module_name}.{method}"));
        }
        Ok(out)
    }

    fn refusal(
        &self,
        app: &Application,
        def: &crate::model::ObjDef,
        category: &'static str,
    ) -> Option<String> {
        let kind = match category {
            "RecordSet" => md_model::ModuleKind::RecordSetModule,
            "ValueManager" => md_model::ModuleKind::ValueManagerModule,
            _ => md_model::ModuleKind::ObjectModule,
        };
        let mut roots = Vec::new();
        if let Some(module) = app
            .modules()
            .of_object(def.id, kind)
            .and_then(|r| app.modules().name_of(r))
            .and_then(|n| self.module_of(n))
        {
            match self.text(module) {
                Err(why) => return Some(why),
                Ok(text) => {
                    for (ru, en) in WRITE_EVENTS {
                        for name in [ru, en] {
                            let name = bsl_prim::fold_name(name);
                            if text.methods.contains_key(&name)
                                || self.targets.contains_key(&(module, name.clone()))
                            {
                                roots.push((module, name));
                            }
                        }
                    }
                }
            }
        }
        for (common, method) in crate::events::subscription_handlers(app, def, category) {
            match self.module_of(&format!("ОбщийМодуль.{common}.Модуль")) {
                Some(module) => roots.push((module, bsl_prim::fold_name(&method))),
                None => return Some(format!("общий модуль подписки {common} не найден")),
            }
        }
        let mut parent: std::collections::HashMap<(usize, String), Option<(usize, String)>> =
            Default::default();
        let mut queue = std::collections::VecDeque::new();
        for root in roots {
            if parent.insert(root.clone(), None).is_none() {
                queue.push_back((root, 0usize));
            }
        }
        let chain =
            |parent: &std::collections::HashMap<(usize, String), Option<(usize, String)>>,
             node: &(usize, String)| {
                let mut chain = vec![self.label(node)];
                let mut at = node.clone();
                while let Some(Some(up)) = parent.get(&at) {
                    chain.push(self.label(up));
                    at = up.clone();
                }
                chain.reverse();
                chain.join(" → ")
            };
        while let Some((node, depth)) = queue.pop_front() {
            if let Some((extension, annotation, target)) = self.targets.get(&node) {
                return Some(format!(
                    "расширение «{extension}» перехватывает {target} ({annotation}): {}",
                    chain(&parent, &node)
                ));
            }
            let calls = match self.calls(app, &node) {
                Ok(calls) => calls,
                Err(why) => return Some(format!("{why}: {}", chain(&parent, &node))),
            };
            for next in calls {
                if parent.contains_key(&next) {
                    continue;
                }
                if depth + 1 > MAX_DEPTH || parent.len() >= MAX_NODES {
                    return Some(format!(
                        "предел обхода вызовов ({MAX_DEPTH} уровней, {MAX_NODES} методов): {}",
                        chain(&parent, &node)
                    ));
                }
                parent.insert(next.clone(), Some(node.clone()));
                queue.push_back((next, depth + 1));
            }
        }
        None
    }
}
