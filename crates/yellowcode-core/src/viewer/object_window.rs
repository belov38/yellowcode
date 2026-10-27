mod managed;
pub use managed::object_window_managed;

use std::collections::HashSet;

use mdconf::{Child, ChildTag, Configuration, LocalString, MdObject, ModuleKind};
use serde_json::{Value, json};

use super::configurator::{TabItem, WindowTab, has_window, window_tabs};
use super::icons::{NodeRef, icon_key};
use super::names::{
    CHILD_TAGS, child_tag_en, group_name, kind_name, module_name, section_name, sub_kind_en,
};
use super::out::Lang;
use super::path::{self, PathError, Target};
use super::show::{self, PropView};

const MAX_LIST: usize = 2000;

pub fn object_window(cfg: &Configuration, p: &str, lang: Lang) -> Result<Value, PathError> {
    let target = path::resolve(cfg, p)?;
    let (kind, name) = super::identity(&target, lang);
    let props = show::show(cfg, &target, lang);
    let owner = canonical(cfg, &target);
    let holder = Holder::of(&target);
    let ctx = Ctx {
        owner: &owner,
        holder: &holder,
        target: &target,
        lang,
    };
    let tabs = match &target {
        Target::Object(o) if has_window(o.kind) => window_tabs(o.kind)
            .iter()
            .map(|t| {
                (
                    t.title(lang).to_string(),
                    t.title_ru == "Прочее",
                    t.items.clone(),
                )
            })
            .collect(),
        _ => vec![(
            window_tabs(mdconf::MdKind::Configuration)
                .first()
                .map_or_else(String::new, |t: &WindowTab| t.title(lang).to_string()),
            false,
            props
                .iter()
                .map(|v| TabItem::Prop(v.name.clone()))
                .chain(holder.lists())
                .collect(),
        )],
    };
    let windowed = matches!(&target, Target::Object(o) if has_window(o.kind));
    let mut placed: HashSet<&str> = HashSet::new();
    let mut out: Vec<(String, bool, Vec<Value>)> = Vec::new();
    for (title, other, items) in tabs {
        let mut vals = Vec::new();
        for it in &items {
            if let TabItem::Prop(n) = it
                && let Some(v) = props.iter().find(|v| &v.name == n)
            {
                placed.insert(v.name.as_str());
            }
            vals.extend(ctx.item(it, &props));
        }
        out.push((title, other, vals));
    }
    let rest: Vec<&PropView> = props
        .iter()
        .filter(|v| !placed.contains(v.name.as_str()))
        .collect();
    if !rest.is_empty() {
        let i = out
            .iter()
            .position(|t| t.1 && windowed)
            .unwrap_or(out.len().saturating_sub(1));
        if let Some(t) = out.get_mut(i) {
            t.2.extend(rest.iter().map(|v| prop_json(v)));
        }
    }
    Ok(json!({
        "kind": kind,
        "name": name,
        "modules": super::module::modules_of(&target)
            .iter()
            .map(|m| module_name(m.kind, Lang::En))
            .collect::<Vec<_>>(),
        "tabs": out
            .into_iter()
            .map(|(title, _, items)| json!({ "title": title, "items": items }))
            .collect::<Vec<_>>(),
    }))
}

fn prop_json(v: &PropView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

struct Holder<'a> {
    children: &'a [Child],
    object: Option<&'a MdObject>,
}

impl<'a> Holder<'a> {
    fn of(t: &'a Target) -> Holder<'a> {
        match t {
            Target::Object(o) => Holder {
                children: &o.children,
                object: Some(o),
            },
            Target::Child { chain, .. } => Holder {
                children: chain.last().map_or(&[], |c| c.children.as_slice()),
                object: None,
            },
            _ => Holder {
                children: &[],
                object: None,
            },
        }
    }

    fn lists(&self) -> Vec<TabItem> {
        let mut v: Vec<TabItem> = CHILD_TAGS
            .iter()
            .filter(|t| self.children.iter().any(|c| c.tag == **t))
            .map(|t| TabItem::List(child_tag_en(*t).to_string()))
            .collect();
        if let Some(o) = self.object {
            if !o.forms.is_empty() {
                v.push(TabItem::List("Form".into()));
            }
            if !o.templates.is_empty() {
                v.push(TabItem::List("Template".into()));
            }
            if !o.recalculations.is_empty() {
                v.push(TabItem::List("Recalculation".into()));
            }
            if !o.subsystems.is_empty() {
                v.push(TabItem::List("Subsystem".into()));
            }
        }
        v
    }
}

struct Ctx<'a> {
    owner: &'a str,
    holder: &'a Holder<'a>,
    target: &'a Target<'a>,
    lang: Lang,
}

fn synonym(s: &LocalString, lang: Lang) -> Option<String> {
    let code = if lang == Lang::Ru { "ru" } else { "en" };
    s.0.iter()
        .find(|(c, _)| c == code)
        .or_else(|| s.0.first())
        .map(|(_, t)| t.clone())
        .filter(|t| !t.is_empty())
}

impl Ctx<'_> {
    fn item(&self, it: &TabItem, props: &[PropView]) -> Option<Value> {
        match it {
            TabItem::Prop(n) => props.iter().find(|v| &v.name == n).map(prop_json),
            TabItem::List(kind) => Some(self.list(kind)),
            TabItem::Caption { ru, en, control } => {
                let text = if self.lang == Lang::Ru || en.is_empty() {
                    ru
                } else {
                    en
                };
                let mut v = json!({ "caption": text, "control": control });
                if let (Some(m), Some(o)) = (self.open_module(control), v.as_object_mut()) {
                    o.insert("open".into(), json!({ "path": self.owner, "module": m }));
                }
                Some(v)
            }
        }
    }

    fn open_module(&self, control: &str) -> Option<String> {
        let mods = super::module::modules_of(self.target);
        let has = |k: ModuleKind| mods.iter().find(|m| m.kind == k).map(|m| m.kind);
        let kind = match control {
            "BtnOpenManagerModule" | "BtnOpenStorageModule" => has(ModuleKind::ManagerModule),
            "BtnOpenRecordSetModule" => has(ModuleKind::RecordSetModule),
            "BtnOpenModule" => mods
                .iter()
                .find(|m| m.kind != ModuleKind::ManagerModule)
                .map(|m| m.kind),
            _ => None,
        }?;
        Some(module_name(kind, Lang::En))
    }

    fn list(&self, kind: &str) -> Value {
        let mut items: Vec<Value> = Vec::new();
        let mut total = 0usize;
        let mut push = |name: &str, path: String, icon: String, syn: Option<String>| {
            total += 1;
            if items.len() < MAX_LIST {
                let mut it = json!({ "name": name, "path": path, "icon": icon });
                if let (Some(s), Some(o)) = (syn, it.as_object_mut()) {
                    o.insert("synonym".into(), json!(s));
                }
                items.push(it);
            }
        };
        let own = |tag: &str, name: &str| format!("{}.{tag}.{name}", self.owner);
        let title = match kind {
            "Form" => {
                for f in self.holder.object.iter().flat_map(|o| &o.forms) {
                    let n = NodeRef::Sub(f.kind);
                    push(
                        &f.name,
                        own(sub_kind_en(f.kind), &f.name),
                        icon_key(&n),
                        None,
                    );
                }
                group_name("Forms", self.lang)
            }
            "Template" => {
                for f in self.holder.object.iter().flat_map(|o| &o.templates) {
                    let n = NodeRef::Sub(f.kind);
                    push(
                        &f.name,
                        own(sub_kind_en(f.kind), &f.name),
                        icon_key(&n),
                        None,
                    );
                }
                group_name("Templates", self.lang)
            }
            "Recalculation" => {
                for r in self.holder.object.iter().flat_map(|o| &o.recalculations) {
                    push(
                        &r.name,
                        own("Recalculation", &r.name),
                        icon_key(&NodeRef::Recalc),
                        synonym(&r.synonym, self.lang),
                    );
                }
                group_name("Recalculations", self.lang)
            }
            "Subsystem" => {
                for s in self.holder.object.iter().flat_map(|o| &o.subsystems) {
                    push(
                        &s.name,
                        own("Subsystem", &s.name),
                        icon_key(&NodeRef::Object(s.kind)),
                        synonym(&s.synonym, self.lang),
                    );
                }
                section_name(mdconf::MdKind::Subsystem, self.lang)
            }
            tag => {
                let wanted: Option<ChildTag> =
                    CHILD_TAGS.iter().copied().find(|t| child_tag_en(*t) == tag);
                for c in self
                    .holder
                    .children
                    .iter()
                    .filter(|c| Some(c.tag) == wanted)
                {
                    push(
                        &c.name,
                        own(tag, &c.name),
                        icon_key(&NodeRef::Child(c.tag)),
                        synonym(&c.synonym, self.lang),
                    );
                }
                group_name(tag, self.lang)
            }
        };
        let mut list = json!({ "kind": kind, "title": title, "items": items });
        if let (true, Some(o)) = (total > MAX_LIST, list.as_object_mut()) {
            o.insert("truncated".into(), json!(true));
            o.insert("total".into(), json!(total));
        }
        json!({ "list": list })
    }
}

fn canonical(cfg: &Configuration, t: &Target) -> String {
    match t {
        Target::Configuration(_) => "Configuration".to_string(),
        Target::Object(o) => object_path(cfg, o),
        Target::Child { owner, chain } => chain.iter().fold(object_path(cfg, owner), |acc, c| {
            format!("{acc}.{}.{}", child_tag_en(c.tag), c.name)
        }),
        Target::Sub { owner, sub } => format!(
            "{}.{}.{}",
            object_path(cfg, owner),
            sub_kind_en(sub.kind),
            sub.name
        ),
        Target::Recalc { owner, recalc } => {
            format!("{}.Recalculation.{}", object_path(cfg, owner), recalc.name)
        }
    }
}

fn object_path(cfg: &Configuration, o: &MdObject) -> String {
    let mut stack: Vec<(Vec<&MdObject>, &MdObject)> =
        cfg.objects.iter().map(|t| (vec![], t)).collect();
    while let Some((up, x)) = stack.pop() {
        if std::ptr::eq(x, o) {
            let mut p = String::new();
            for a in up.iter().copied().chain(std::iter::once(x)) {
                if !p.is_empty() {
                    p.push('.');
                }
                p.push_str(&format!("{}.{}", kind_name(a.kind, Lang::En), a.name));
            }
            return p;
        }
        for s in &x.subsystems {
            let mut chain = up.clone();
            chain.push(x);
            stack.push((chain, s));
        }
    }
    format!("{}.{}", kind_name(o.kind, Lang::En), o.name)
}

pub(crate) use managed::canonical as canonical_managed;
