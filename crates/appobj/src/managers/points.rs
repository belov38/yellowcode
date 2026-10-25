use crate::Application;
use crate::meta::{member_name, not_implemented};
use crate::model::ObjId;
use crate::types::Category;
use bsl_prim::{Str, fold_name};
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, MemberKind, TypeId, Value,
};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const POINT_BASE: u32 = 1 << 20;

#[derive(Clone, Debug, Default)]
struct PointDef {
    name: String,
    kind: String,
    title: Vec<(String, String)>,
    task: String,
    explanation: String,
    group: bool,
}

fn kind_of_tag(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "Start" => "Start",
        "Completion" | "End" => "End",
        "Activity" => "Activity",
        "Condition" => "Condition",
        "Fork" => "Split",
        "Join" => "Join",
        "Switch" => "Switch",
        "Processing" => "Processing",
        "SubBusinessProcess" => "SubBusinessProcess",
        _ => return None,
    })
}

fn local(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    s.rsplit(':').next().unwrap_or("").to_string()
}

fn parse(xml: &[u8]) -> Vec<PointDef> {
    let xml = xml.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(xml);
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut path: Vec<String> = Vec::new();
    let mut points: Vec<PointDef> = Vec::new();
    let mut text = String::new();
    let mut lang = String::new();
    let mut open = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag = local(e.name().as_ref());
                if path.len() == 2
                    && path.get(1).map(String::as_str) == Some("Items")
                    && let Some(kind) = kind_of_tag(&tag)
                {
                    points.push(PointDef {
                        kind: kind.to_string(),
                        ..PointDef::default()
                    });
                    open = true;
                } else if path.len() == 2 {
                    open = false;
                }
                path.push(tag);
                text.clear();
            }
            Ok(Event::Text(t)) => {
                if let Ok(s) = t.unescape() {
                    text.push_str(&s);
                }
            }
            Ok(Event::End(_)) => {
                let tag = path.pop().unwrap_or_default();
                let depth = path.len();
                let in_props = depth >= 4
                    && path.get(1).map(String::as_str) == Some("Items")
                    && path.get(3).map(String::as_str) == Some("Properties");
                if let (true, true, Some(p)) = (in_props, open, points.last_mut()) {
                    match (depth, tag.as_str()) {
                        (4, "Name") => p.name = text.clone(),
                        (4, "TaskDescription") => p.task = text.clone(),
                        (4, "Explanation") => p.explanation = text.clone(),
                        (4, "Group") => p.group = text == "true",
                        (6, "lang") if path.get(4).map(String::as_str) == Some("Title") => {
                            lang = text.clone();
                        }
                        (6, "content") if path.get(4).map(String::as_str) == Some("Title") => {
                            p.title.push((lang.clone(), text.clone()));
                        }
                        _ => {}
                    }
                }
                text.clear();
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    points
}

fn points_of(app: &Application, obj: ObjId) -> Vec<PointDef> {
    let md = app.model().obj(obj).md;
    app.model()
        .config
        .objects
        .iter()
        .find(|m| m.id == md)
        .and_then(|m| m.flowchart.as_ref())
        .map(|f| parse(&f.xml))
        .unwrap_or_default()
}

pub(crate) fn collection(app: &Arc<Application>, obj: ObjId) -> CallResult<Value> {
    let ty = platform_catalog::catalog()
        .find_type("ТочкиМаршрута")
        .map(TypeId::of_catalog)
        .ok_or_else(|| {
            HostError::text(
                "internal: the catalog has no type ТочкиМаршрута",
                "internal: the catalog has no type ТочкиМаршрута",
            )
        })?;
    Ok(Value::Object(Rc::new(RoutePoints {
        app: app.clone(),
        obj,
        ty,
        points: points_of(app, obj),
    })))
}

struct RoutePoints {
    app: Arc<Application>,
    obj: ObjId,
    ty: TypeId,
    points: Vec<PointDef>,
}

impl RoutePoints {
    fn point(&self, i: usize) -> Option<Value> {
        let ty = self
            .app
            .types()?
            .type_of(self.obj, Category::RoutePointRef)?;
        let def = self.points.get(i)?.clone();
        Some(Value::Object(Rc::new(RoutePoint {
            app: self.app.clone(),
            obj: self.obj,
            i,
            ty,
            def,
        })))
    }

    fn catalog_name(&self, m: MemberId) -> Option<&'static str> {
        let c = platform_catalog::catalog();
        let t = c.find_type("ТочкиМаршрута")?;
        member_name(c, t, m).map(|(_, ru, _)| ru)
    }
}

impl HostObject for RoutePoints {
    fn type_id(&self) -> TypeId {
        self.ty
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        m.0.checked_sub(POINT_BASE)
            .and_then(|i| self.point(i as usize))
            .ok_or_else(|| not_implemented(&m.0.to_string()).into())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(not_implemented("ТочкиМаршрута").into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match self.catalog_name(m) {
            Some("Количество") => Ok(Value::from(
                i64::try_from(self.points.len()).unwrap_or(i64::MAX),
            )),
            Some("Получить") => {
                let Some(Arg::Val(Value::Number(n))) = args.first() else {
                    return Err(crate::meta::type_mismatch(1).into());
                };
                usize::try_from(n.trunc_to_i64_saturating())
                    .ok()
                    .and_then(|i| self.point(i))
                    .ok_or_else(|| HostError::key(bsl_stdlib::bind::msg::RANGE).into())
            }
            Some("Индекс") => {
                let Some(Arg::Val(Value::Object(o))) = args.first() else {
                    return Err(crate::meta::type_mismatch(1).into());
                };
                let i = o
                    .as_any()
                    .downcast_ref::<RoutePoint>()
                    .filter(|p| p.obj == self.obj)
                    .map_or(-1, |p| i64::try_from(p.i).unwrap_or(-1));
                Ok(Value::from(i))
            }
            _ => Err(not_implemented("ТочкиМаршрута").into()),
        }
    }
    fn find_member(&self, name: &str) -> Option<MemberId> {
        let f = fold_name(name);
        self.points
            .iter()
            .position(|p| fold_name(&p.name) == f)
            .and_then(|i| u32::try_from(i).ok())
            .map(|i| MemberId(POINT_BASE + i))
    }
    fn member_kind(&self, m: MemberId) -> Option<MemberKind> {
        (m.0 >= POINT_BASE).then_some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.points.len())
    }
    fn iter_get(&self, _: &mut Execution, i: usize) -> CallResult<Value> {
        self.point(i)
            .ok_or_else(|| HostError::key(bsl_stdlib::bind::msg::RANGE).into())
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(Str::new()))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<RoutePoints>()
                .is_some_and(|o| o.obj == self.obj),
        )
    }
}

struct RoutePoint {
    app: Arc<Application>,
    obj: ObjId,
    i: usize,
    ty: TypeId,
    def: PointDef,
}

impl HostObject for RoutePoint {
    fn type_id(&self) -> TypeId {
        self.ty
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let name = ex
            .runtime()
            .types()
            .app_type(self.ty)
            .and_then(|a| a.template)
            .and_then(|t| member_name(platform_catalog::catalog(), t, m))
            .map_or("", |n| n.1);
        let tname = format!(
            "ТочкаМаршрутаБизнесПроцессаСсылка.{}",
            self.app.model().obj(self.obj).name
        );
        Ok(match name {
            "Имя" => Value::from(self.def.name.as_str()),
            "НаименованиеЗадачи" => Value::from(self.def.task.as_str()),
            "Пояснение" => Value::from(self.def.explanation.as_str()),
            "Групповая" => Value::Bool(self.def.group),
            "Вид" => {
                let c = platform_catalog::catalog();
                c.find_type("ВидТочкиМаршрутаБизнесПроцесса")
                    .and_then(|t| bsl_stdlib::enums::enum_value(t, &self.def.kind))
                    .unwrap_or(Value::Undefined)
            }
            other => return Err(HostError::not_implemented(&tname, other).into()),
        })
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(not_implemented("ТочкаМаршрута").into())
    }
    fn call(&self, _: &mut Execution, _: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented("ТочкаМаршрута").into())
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        let code = &self.app.model().lang_code;
        let title = self
            .def
            .title
            .iter()
            .find(|(l, t)| l == code && !t.is_empty())
            .map_or("", |(_, t)| t.as_str());
        Ok(Some(Str::from(title)))
    }
    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        Some(
            other
                .as_any()
                .downcast_ref::<RoutePoint>()
                .is_some_and(|o| o.obj == self.obj && o.i == self.i),
        )
    }
}
