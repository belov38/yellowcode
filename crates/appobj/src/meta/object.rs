use super::{
    MdCtx, MdKey, MdPath, arg_value, fold, member_name, member_of, no_field, not_implemented,
    props, value_of,
};
use crate::model::{FieldDef, FieldRole, ObjId};
use bsl_prim::{Number, Str};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, TypeId, Value,
};
use md_model::{Child, LocalString, MdKind, MdObject, Prop};
use platform_catalog::{TypeIdx, messages::Lang};
use std::any::Any;
use std::sync::Arc;

const ROOT_FULL_NAME: u32 = u32::MAX;

pub(super) fn read_only(name: &str) -> HostError {
    HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
        .with_arg(name)
        .raw()
}

pub(crate) struct Info<'a> {
    pub name: &'a str,
    pub synonym: Option<&'a LocalString>,
    pub comment: &'a str,
    pub props: &'a [Prop],
}

fn md_of(cx: &MdCtx, md: crate::model::Uuid16) -> Option<&MdObject> {
    cx.cfg_index
        .get(&md)
        .and_then(|i| cx.model().config.objects.get(*i))
}

fn child_of(
    md: &MdObject,
    tab: Option<crate::model::Uuid16>,
    id: crate::model::Uuid16,
) -> Option<&Child> {
    match tab {
        None => md.children.iter().find(|c| c.id == id),
        Some(t) => md
            .children
            .iter()
            .find(|c| c.id == t)
            .and_then(|c| c.children.iter().find(|c| c.id == id)),
    }
}

pub(crate) fn field(cx: &MdCtx, obj: ObjId, tab: Option<usize>, i: usize) -> Option<&FieldDef> {
    let def = cx.model().obj(obj);
    match tab {
        None => def.fields.get(i),
        Some(t) => def.tabular.get(t)?.fields.get(i),
    }
}

pub(crate) fn info<'a>(cx: &'a MdCtx, p: MdPath) -> Option<Info<'a>> {
    let model = cx.model();
    let none: &[Prop] = &[];
    match p {
        MdPath::Object(id) => {
            let def = model.obj(id);
            Some(Info {
                name: &def.name,
                synonym: Some(&def.synonym),
                comment: &def.comment,
                props: md_of(cx, def.md).map_or(none, |m| m.props.as_slice()),
            })
        }
        MdPath::Field(o, tab, i) => {
            let def = model.obj(o);
            let fd = field(cx, o, tab, i)?;
            let md = md_of(cx, def.md);
            let tab_id = tab.and_then(|t| def.tabular.get(t)).map(|t| t.md);
            let child = fd
                .md
                .and_then(|id| md.and_then(|m| child_of(m, tab_id, id)));
            Some(Info {
                name: &fd.name,
                synonym: Some(&fd.synonym),
                comment: child.map_or("", |c| c.comment.as_str()),
                props: child.map_or(none, |c| c.props.as_slice()),
            })
        }
        MdPath::Tabular(o, t) => {
            let def = model.obj(o);
            let td = def.tabular.get(t)?;
            let child = md_of(cx, def.md).and_then(|m| child_of(m, None, td.md));
            Some(Info {
                name: &td.name,
                synonym: Some(&td.synonym),
                comment: child.map_or("", |c| c.comment.as_str()),
                props: child.map_or(none, |c| c.props.as_slice()),
            })
        }
        MdPath::EnumValue(o, i) => {
            let def = model.obj(o);
            let ev = def.enum_values.get(i)?;
            let child = md_of(cx, def.md).and_then(|m| child_of(m, None, ev.md));
            Some(Info {
                name: &ev.name,
                synonym: Some(&ev.synonym),
                comment: child.map_or("", |c| c.comment.as_str()),
                props: child.map_or(none, |c| c.props.as_slice()),
            })
        }
        MdPath::Form(o, i) => {
            let sub = md_of(cx, model.obj(o).md)?.forms.get(i)?;
            let syn = sub.props.iter().find_map(|p| match &p.value {
                md_model::Value::LStr(s) if p.name == "Synonym" => Some(s),
                _ => None,
            });
            let comment = sub.props.iter().find_map(|p| match &p.value {
                md_model::Value::Text(t) if p.name == "Comment" => Some(t.as_str()),
                _ => None,
            });
            Some(Info {
                name: &sub.name,
                synonym: syn,
                comment: comment.unwrap_or(""),
                props: &sub.props,
            })
        }
        MdPath::CommonModule(i) => {
            let def = model.common_modules.get(i)?;
            let md = md_of(cx, def.md);
            Some(Info {
                name: &def.name,
                synonym: md.map(|m| &m.synonym),
                comment: md.map_or("", |m| m.comment.as_str()),
                props: md.map_or(none, |m| m.props.as_slice()),
            })
        }
        MdPath::SessionParam(i) => {
            let def = model.session_params.get(i)?;
            let md = md_of(cx, def.md);
            Some(Info {
                name: &def.name,
                synonym: Some(&def.synonym),
                comment: md.map_or("", |m| m.comment.as_str()),
                props: md.map_or(none, |m| m.props.as_slice()),
            })
        }
        MdPath::Subscription(i) => {
            let def = model.subscriptions.get(i)?;
            let md = md_of(cx, def.md);
            Some(Info {
                name: &def.name,
                synonym: md.map(|m| &m.synonym),
                comment: md.map_or("", |m| m.comment.as_str()),
                props: md.map_or(none, |m| m.props.as_slice()),
            })
        }
        MdPath::Other(i) => {
            let md = model.config.objects.get(i)?;
            Some(Info {
                name: &md.name,
                synonym: Some(&md.synonym),
                comment: &md.comment,
                props: &md.props,
            })
        }
        MdPath::Configuration => None,
    }
}

pub(crate) fn synonym_text(s: &LocalString, code: &str) -> String {
    s.0.iter()
        .find(|(l, t)| l == code && !t.is_empty())
        .map_or_else(String::new, |(_, t)| t.clone())
}

pub(crate) fn name_of(cx: &MdCtx, p: MdPath) -> String {
    match p {
        MdPath::Configuration => root_text(cx, "Name"),
        _ => info(cx, p).map_or_else(String::new, |i| i.name.to_string()),
    }
}

fn presentation_of(cx: &MdCtx, p: MdPath) -> String {
    let syn = info(cx, p)
        .and_then(|i| i.synonym.map(|s| synonym_text(s, &cx.model().lang_code)))
        .unwrap_or_default();
    if syn.is_empty() { name_of(cx, p) } else { syn }
}

pub(crate) fn root_text(cx: &MdCtx, name: &str) -> String {
    match cx
        .model()
        .config
        .props
        .iter()
        .find(|p| p.name == name)
        .map(|p| &p.value)
    {
        Some(md_model::Value::Text(t)) => t.clone(),
        _ => String::new(),
    }
}

fn words(cx: &MdCtx, p: MdPath) -> Vec<String> {
    let lang = cx.script_lang();
    let pick = |ru: &str, en: &str| -> String {
        match lang {
            Lang::Ru => ru.to_string(),
            Lang::En => en.to_string(),
        }
    };
    let model = cx.model();
    let role_word = |r: FieldRole| match r {
        FieldRole::Dimension => pick("Измерение", "Dimension"),
        FieldRole::Resource => pick("Ресурс", "Resource"),
        FieldRole::AddressingAttribute => pick("РеквизитАдресации", "AddressingAttribute"),
        _ => pick("Реквизит", "Attribute"),
    };
    match p {
        MdPath::Configuration => vec![pick("Конфигурация", "Configuration"), name_of(cx, p)],
        MdPath::Object(id) => {
            let def = model.obj(id);
            vec![def.kind.name(lang).to_string(), def.name.clone()]
        }
        MdPath::Field(o, tab, i) => {
            let mut w = match tab {
                None => words(cx, MdPath::Object(o)),
                Some(t) => words(cx, MdPath::Tabular(o, t)),
            };
            let (role, name) = field(cx, o, tab, i)
                .map_or((FieldRole::Attribute, String::new()), |f| {
                    (f.role, f.name.clone())
                });
            w.push(role_word(role));
            w.push(name);
            w
        }
        MdPath::Tabular(o, _) => {
            let mut w = words(cx, MdPath::Object(o));
            w.push(pick("ТабличнаяЧасть", "TabularSection"));
            w.push(name_of(cx, p));
            w
        }
        MdPath::EnumValue(o, _) => {
            let mut w = words(cx, MdPath::Object(o));
            w.push(pick("ЗначениеПеречисления", "EnumValue"));
            w.push(name_of(cx, p));
            w
        }
        MdPath::Form(o, _) => {
            let mut w = words(cx, MdPath::Object(o));
            w.push(pick("Форма", "Form"));
            w.push(name_of(cx, p));
            w
        }
        MdPath::CommonModule(_) => vec![pick("ОбщийМодуль", "CommonModule"), name_of(cx, p)],
        MdPath::SessionParam(_) => vec![pick("ПараметрСеанса", "SessionParameter"), name_of(cx, p)],
        MdPath::Subscription(_) => {
            vec![
                pick("ПодпискаНаСобытие", "EventSubscription"),
                name_of(cx, p),
            ]
        }
        MdPath::Other(i) => {
            let kind = model.config.objects.get(i).map(|o| o.kind);
            let ru = kind
                .and_then(|k| {
                    cx.catalog
                        .find_type(&format!("MetadataObject: {}", k.as_str()))
                })
                .and_then(|t| {
                    cx.catalog
                        .type_info(t)
                        .name_ru
                        .strip_prefix("ОбъектМетаданных: ")
                        .map(str::to_string)
                });
            let en = kind.map_or("", MdKind::as_str);
            vec![pick(ru.as_deref().unwrap_or(en), en), name_of(cx, p)]
        }
    }
}

pub(crate) fn full_name(cx: &MdCtx, p: MdPath) -> String {
    words(cx, p).join(".")
}

fn parent_of(p: MdPath) -> MdPath {
    match p {
        MdPath::Field(o, None, _)
        | MdPath::Tabular(o, _)
        | MdPath::EnumValue(o, _)
        | MdPath::Form(o, _) => MdPath::Object(o),
        MdPath::Field(o, Some(t), _) => MdPath::Tabular(o, t),
        _ => MdPath::Configuration,
    }
}

pub(crate) fn type_idx(cx: &MdCtx, p: MdPath) -> Option<TypeIdx> {
    let en = match p {
        MdPath::Object(id) => cx.model().obj(id).kind.md_kind().as_str(),
        MdPath::Field(o, tab, i) => match field(cx, o, tab, i)?.role {
            FieldRole::Dimension => "Dimension",
            FieldRole::Resource => "Resource",
            FieldRole::AddressingAttribute => "AddressingAttribute",
            _ => "Attribute",
        },
        MdPath::Tabular(..) => "TabularSection",
        MdPath::EnumValue(..) => "EnumValue",
        MdPath::Form(..) => "Form",
        MdPath::CommonModule(_) => "CommonModule",
        MdPath::SessionParam(_) => "SessionParameter",
        MdPath::Subscription(_) => "EventSubscription",
        MdPath::Other(i) => cx.model().config.objects.get(i)?.kind.as_str(),
        MdPath::Configuration => return None,
    };
    cx.catalog.find_type(&format!("MetadataObject: {en}"))
}

pub(crate) struct MdRoot {
    pub cx: Arc<MdCtx>,
}

impl HostObject for MdRoot {
    fn type_id(&self) -> TypeId {
        self.cx.root.id
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        match member_name(self.cx.catalog, self.cx.root.idx, m) {
            Some((false, ru, en)) => props::root_get(&self.cx, ex, ru, en),
            Some((_, ru, _)) => Err(not_implemented(ru).into()),
            None => Err(no_field(&m.0.to_string()).into()),
        }
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name = member_name(self.cx.catalog, self.cx.root.idx, m).map_or("", |n| n.1);
        Err(read_only(name).into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        if m.0 == ROOT_FULL_NAME {
            return Ok(Value::from(
                full_name(&self.cx, MdPath::Configuration).as_str(),
            ));
        }
        match member_name(self.cx.catalog, self.cx.root.idx, m) {
            Some((true, ru, _)) => match fold(ru).as_str() {
                "найтипополномуимени" => {
                    let v = arg_value(ex, args, 0)?;
                    let Value::String(s) = &v else {
                        return Err(props::type_mismatch(1).into());
                    };
                    Ok(match props::resolve_full(&self.cx, &s.to_string_lossy()) {
                        Some(p) => value_of(&self.cx, ex, MdKey::Path(p)),
                        None => Value::Undefined,
                    })
                }
                "найтипотипу" => {
                    let v = arg_value(ex, args, 0)?;
                    let Value::Type(t) = v else {
                        return Err(props::type_mismatch(1).into());
                    };
                    Ok(match props::find_by_type(&self.cx, t) {
                        Some(p) => value_of(&self.cx, ex, MdKey::Path(p)),
                        None => Value::Undefined,
                    })
                }
                "естьизменениярасширениямиконфигурации" => {
                    Ok(Value::Bool(false))
                }
                _ => Err(not_implemented(ru).into()),
            },
            _ => Err(no_field(&format!("{}", m.0)).into()),
        }
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        match fold(name).as_str() {
            "полноеимя" | "fullname" => Some(MemberId(ROOT_FULL_NAME)),
            _ => None,
        }
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        (m.0 == ROOT_FULL_NAME).then_some(MemberKind::Method {
            is_function: true,
            min_args: 0,
            max_args: Some(0),
        })
    }
    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        let info = self.cx.catalog.type_info(self.cx.root.idx);
        Ok(Some(Str::from(match ex.lang() {
            Lang::Ru => info.name_ru.as_str(),
            Lang::En => info.name_en.as_str(),
        })))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(MdKey::Path(MdPath::Configuration).hash32())
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(other.as_any().is::<MdRoot>())
    }
}

pub(crate) struct MdObj {
    pub cx: Arc<MdCtx>,
    pub path: MdPath,
}

impl MdObj {
    fn tidx(&self) -> Option<TypeIdx> {
        type_idx(&self.cx, self.path)
    }
}

impl HostObject for MdObj {
    fn type_id(&self) -> TypeId {
        self.cx.t_obj
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = self.tidx().ok_or_else(|| no_field(""))?;
        match member_name(self.cx.catalog, t, m) {
            Some((false, ru, en)) => props::obj_get(&self.cx, ex, self.path, ru, en),
            _ => Err(no_field(&m.0.to_string()).into()),
        }
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name = self
            .tidx()
            .and_then(|t| member_name(self.cx.catalog, t, m))
            .map_or("", |n| n.1);
        Err(read_only(name).into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        let t = self.tidx().ok_or_else(|| no_field(""))?;
        let Some((true, ru, _)) = member_name(self.cx.catalog, t, m) else {
            return Err(no_field(&m.0.to_string()).into());
        };
        match fold(ru).as_str() {
            "полноеимя" => Ok(Value::from(full_name(&self.cx, self.path).as_str())),
            "представление" => {
                Ok(Value::from(presentation_of(&self.cx, self.path).as_str()))
            }
            "родитель" => Ok(value_of(&self.cx, ex, MdKey::Path(parent_of(self.path)))),
            "получитьименапредопределенных" => {
                props::predefined_names(&self.cx, ex, self.path)
            }
            "расширениеконфигурации" => Ok(Value::Undefined),
            "естьизменениярасширениямиконфигурации" => {
                Ok(Value::Bool(false))
            }
            _ => Err(not_implemented(ru).into()),
        }
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        let t = self.tidx()?;
        member_of(self.cx.catalog, t, name).map(|(id, _)| id)
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        let t = self.tidx()?;
        let (_, ru, _) = member_name(self.cx.catalog, t, m)?;
        member_of(self.cx.catalog, t, ru).map(|k| k.1)
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::from(
            presentation_of(&self.cx, self.path).as_str(),
        )))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(MdKey::Path(self.path).hash32())
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<MdObj>()
                .is_some_and(|o| o.path == self.path),
        )
    }
}

pub(crate) struct MdStd {
    pub cx: Arc<MdCtx>,
    pub obj: ObjId,
    pub tab: Option<usize>,
    pub idx: usize,
}

impl MdStd {
    fn def(&self) -> Option<&FieldDef> {
        field(&self.cx, self.obj, self.tab, self.idx)
    }
}

impl HostObject for MdStd {
    fn type_id(&self) -> TypeId {
        self.cx.std.id
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let fd = self.def().ok_or_else(|| no_field(""))?;
        match member_name(self.cx.catalog, self.cx.std.idx, m) {
            Some((false, ru, en)) => props::std_get(&self.cx, ex, self.obj, fd, ru, en),
            _ => Err(no_field(&m.0.to_string()).into()),
        }
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let name = member_name(self.cx.catalog, self.cx.std.idx, m).map_or("", |n| n.1);
        Err(read_only(name).into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        let fd = self.def().ok_or_else(|| no_field(""))?;
        match member_name(self.cx.catalog, self.cx.std.idx, m) {
            Some((true, ru, _)) if fold(ru) == "представление" => {
                let syn = synonym_text(&fd.synonym, &self.cx.model().lang_code);
                Ok(Value::from(if syn.is_empty() {
                    fd.name.as_str()
                } else {
                    syn.as_str()
                }))
            }
            Some((true, ru, _)) => Err(not_implemented(ru).into()),
            _ => Err(no_field(&m.0.to_string()).into()),
        }
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        let Some(fd) = self.def() else {
            return Ok(None);
        };
        let syn = synonym_text(&fd.synonym, &self.cx.model().lang_code);
        Ok(Some(Str::from(if syn.is_empty() {
            fd.name.as_str()
        } else {
            syn.as_str()
        })))
    }
    fn boost_hash(&self) -> Option<i32> {
        Some(MdKey::Std(self.obj, self.tab, self.idx).hash32())
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<MdStd>()
                .is_some_and(|o| (o.obj, o.tab, o.idx) == (self.obj, self.tab, self.idx)),
        )
    }
}

pub(crate) fn number(n: usize) -> Value {
    Value::Number(Number::from_i64(i64::try_from(n).unwrap_or(i64::MAX)))
}
