mod coll;
mod object;
mod props;
mod stdtab;

use crate::errors::{self, keys};
use crate::model::{AppModel, ObjId, ObjKind, Uuid16};
use crate::session::call_ctx;
use crate::{AppError, Application, Result};
use bsl_prim::fold_name;
use bsl_vm::{
    AppTypeInfo, Arg, CallResult, Execution, HostError, MemberId, MemberKind, ObjRef, Registry,
    TypeId, Value,
};
use md_model::MdKind;
use platform_catalog::{Catalog, Member, TypeIdx, messages::Lang};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) use crate::model::MdPath;
pub(crate) use props::type_mismatch;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MdKey {
    Path(MdPath),
    Std(ObjId, Option<usize>, usize),
}

impl MdKey {
    fn hash32(self) -> i32 {
        let mut h: u32 = 0x811c_9dc5;
        let mut put = |v: u32| {
            for b in v.to_le_bytes() {
                h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
            }
        };
        let idx = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
        match self {
            MdKey::Path(p) => match p {
                MdPath::Object(o) => {
                    put(1);
                    put(o.0);
                }
                MdPath::Field(o, t, i) => {
                    put(2);
                    put(o.0);
                    put(t.map_or(u32::MAX, idx));
                    put(idx(i));
                }
                MdPath::Tabular(o, t) => {
                    put(3);
                    put(o.0);
                    put(idx(t));
                }
                MdPath::EnumValue(o, i) => {
                    put(4);
                    put(o.0);
                    put(idx(i));
                }
                MdPath::CommonModule(i) => {
                    put(5);
                    put(idx(i));
                }
                MdPath::SessionParam(i) => {
                    put(6);
                    put(idx(i));
                }
                MdPath::Subscription(i) => {
                    put(7);
                    put(idx(i));
                }
                MdPath::Other(i) => {
                    put(8);
                    put(idx(i));
                }
                MdPath::Form(o, i) => {
                    put(11);
                    put(o.0);
                    put(idx(i));
                }
                MdPath::Configuration => put(9),
            },
            MdKey::Std(o, t, i) => {
                put(10);
                put(o.0);
                put(t.map_or(u32::MAX, idx));
                put(idx(i));
            }
        }
        h as i32
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Ty {
    pub idx: TypeIdx,
    pub id: TypeId,
}

pub(crate) struct MdCtx {
    pub app: Arc<Application>,
    pub catalog: &'static Catalog,
    pub t_obj: TypeId,
    pub root: Ty,
    pub std: Ty,
    pub coll: Ty,
    pub vals: Ty,
    pub stds: Ty,
    pub cfg_index: HashMap<Uuid16, usize>,
}

impl MdCtx {
    pub fn model(&self) -> &AppModel {
        self.app.model()
    }

    pub fn script_lang(&self) -> Lang {
        match self.model().script {
            md_model::ScriptVariant::Russian => Lang::Ru,
            md_model::ScriptVariant::English => Lang::En,
        }
    }
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) -> Result<()> {
    let catalog = platform_catalog::catalog();
    let find = |name: &str| -> Result<Ty> {
        catalog
            .find_type(name)
            .map(|idx| Ty {
                idx,
                id: TypeId::of_catalog(idx),
            })
            .ok_or_else(|| AppError::Unsupported(format!("the catalog has no type {name}")))
    };
    let m = errors::msg(keys::MD_OBJECT_TYPE).ok_or_else(|| {
        AppError::Unsupported(format!("internal: no message {}", keys::MD_OBJECT_TYPE))
    })?;
    let (ru, en) = (m.ru, m.en);
    let guid: [u8; 16] = {
        let d = Sha256::digest(b"MetadataObject");
        let mut g = [0u8; 16];
        g.copy_from_slice(d.get(..16).unwrap_or(&[0; 16]));
        g
    };
    let t_obj = reg.register_app_type(AppTypeInfo {
        name_ru: "ОбъектМетаданных".into(),
        name_en: "MetadataObject".into(),
        presentation_ru: ru.into(),
        presentation_en: en.into(),
        guid,
        xdto: None,
        template: None,
        default_value: None,
    });
    let cx = Arc::new(MdCtx {
        app: app.clone(),
        catalog,
        t_obj,
        root: find("ОбъектМетаданныхКонфигурация")?,
        std: find("ОписаниеСтандартногоРеквизита")?,
        coll: find("КоллекцияОбъектовМетаданных")?,
        vals: find("КоллекцияЗначенийСвойстваОбъектаМетаданных")?,
        stds: find("ОписанияСтандартныхРеквизитов")?,
        cfg_index: app
            .model()
            .config
            .objects
            .iter()
            .enumerate()
            .map(|(i, o)| (o.id, i))
            .collect(),
    });
    app.set_md_ctx(&cx);
    reg.global_prop(
        "Метаданные",
        "Metadata",
        Arc::new(move |ex| Ok(value_of(&cx, ex, MdKey::Path(MdPath::Configuration)))),
    );
    Ok(())
}

pub(crate) fn value_of(cx: &Arc<MdCtx>, ex: &mut Execution, key: MdKey) -> Value {
    if let Some(o) = call_ctx(ex).and_then(|c| c.md_cache.get(&key).cloned()) {
        return Value::Object(o);
    }
    let o: ObjRef = match key {
        MdKey::Path(MdPath::Configuration) => Rc::new(object::MdRoot { cx: cx.clone() }),
        MdKey::Path(path) => Rc::new(object::MdObj {
            cx: cx.clone(),
            path,
        }),
        MdKey::Std(obj, tab, idx) => Rc::new(object::MdStd {
            cx: cx.clone(),
            obj,
            tab,
            idx,
        }),
    };
    if let Some(c) = call_ctx(ex) {
        c.md_cache.insert(key, o.clone());
    }
    Value::Object(o)
}

pub(crate) fn key_of(v: &Value) -> Option<MdKey> {
    let any = v.as_object()?.as_any();
    if any.is::<object::MdRoot>() {
        return Some(MdKey::Path(MdPath::Configuration));
    }
    if let Some(o) = any.downcast_ref::<object::MdObj>() {
        return Some(MdKey::Path(o.path));
    }
    any.downcast_ref::<object::MdStd>()
        .map(|s| MdKey::Std(s.obj, s.tab, s.idx))
}

pub(crate) fn no_field(name: &str) -> HostError {
    HostError::key(bsl_stdlib::bind::msg::PROP_NOT_FOUND)
        .with_arg(name)
        .raw()
}

pub(crate) fn not_implemented(member: &str) -> HostError {
    HostError::not_implemented("ОбъектМетаданных", member)
}

pub(crate) fn arg_value(ex: &mut Execution, args: &[Arg], i: usize) -> CallResult<Value> {
    Ok(match args.get(i) {
        None | Some(Arg::Omitted) => Value::Undefined,
        Some(Arg::Val(v)) => v.clone(),
        Some(Arg::Place(p)) => p.get(ex)?,
    })
}

pub(crate) fn member_of(
    catalog: &Catalog,
    t: TypeIdx,
    name: &str,
) -> Option<(MemberId, MemberKind)> {
    let info = catalog.type_info(t);
    match catalog.find_member(t, name)? {
        Member::Property(p) => {
            let i = info.properties.iter().position(|q| std::ptr::eq(q, p))?;
            Some((
                MemberId(u32::try_from(i).ok()?),
                MemberKind::Property {
                    readable: true,
                    writable: false,
                },
            ))
        }
        Member::Method(m) => {
            let j = info.methods.iter().position(|q| std::ptr::eq(q, m))?;
            let id = u32::try_from(info.properties.len() + j).ok()?;
            let sigs = &m.variants;
            let min = sigs
                .iter()
                .map(|s| {
                    s.params
                        .iter()
                        .filter(|p| !p.optional && !p.variadic)
                        .count()
                })
                .min()
                .unwrap_or(0);
            let max = if sigs.iter().any(|s| s.params.iter().any(|p| p.variadic)) {
                None
            } else {
                Some(sigs.iter().map(|s| s.params.len()).max().unwrap_or(0))
            };
            Some((
                MemberId(id),
                MemberKind::Method {
                    is_function: m.returns_value || m.name_ru == "Родитель",
                    min_args: u16::try_from(min).unwrap_or(u16::MAX),
                    max_args: max.map(|m| u16::try_from(m).unwrap_or(u16::MAX)),
                },
            ))
        }
    }
}

pub(crate) fn member_name(
    catalog: &Catalog,
    t: TypeIdx,
    m: MemberId,
) -> Option<(bool, &str, &str)> {
    let info = catalog.type_info(t);
    let i = m.0 as usize;
    match info.properties.get(i) {
        Some(p) => Some((false, p.name_ru.as_str(), p.name_en.as_str())),
        None => info
            .methods
            .get(i - info.properties.len())
            .map(|m| (true, m.name_ru.as_str(), m.name_en.as_str())),
    }
}

pub(crate) fn fold(s: &str) -> String {
    fold_name(s)
}

pub(crate) fn root_kind(en: &str) -> Option<MdKind> {
    let f = fold_name(en);
    MdKind::all()
        .iter()
        .copied()
        .find(|k| k.folder().is_some_and(|d| fold_name(d) == f))
}

pub(crate) fn is_model_kind(k: MdKind) -> Option<ObjKind> {
    ObjKind::of_md(k)
}
