use std::collections::HashMap;
use std::sync::OnceLock;

use mdconf::{
    Child, ChildTag, Configuration, LocalString, MdKind, MdObject, Module, ModuleKind, SubObject,
    Value,
};
use serde::Serialize;

use super::icons::{NodeRef, icon_key};
use super::names::{child_tag_en, group_name, kind_name, module_name, section_name, sub_kind_en};
use super::out::Lang;
use super::types::type_text;

#[derive(Debug, Serialize)]
pub struct TreeNode {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synonym: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    fn new(label: impl Into<String>, node: NodeRef) -> Self {
        TreeNode {
            icon: Some(icon_key(&node)),
            label: label.into(),
            path: None,
            module: None,
            synonym: None,
            type_text: None,
            children: Vec::new(),
        }
    }

    fn with_path(mut self, path: String) -> Self {
        self.path = Some(path);
        self
    }
}

struct Order {
    top: Vec<Option<MdKind>>,
    common: Vec<MdKind>,
    after: HashMap<MdKind, Vec<MdKind>>,
    children: HashMap<String, Vec<String>>,
}

#[doc(hidden)]
pub static ORDER_JSON: &str = include_str!("tree_order.json");

fn kinds_of(v: &serde_json::Value) -> Vec<MdKind> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().and_then(MdKind::from_name))
                .collect()
        })
        .unwrap_or_default()
}

fn order() -> &'static Order {
    static ORDER: OnceLock<Order> = OnceLock::new();
    ORDER.get_or_init(|| {
        let v: serde_json::Value = serde_json::from_str(ORDER_JSON).unwrap_or_default();
        let field = |n: &str| v.get(n).cloned().unwrap_or_default();
        let top = field("root")
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| match x.as_str()? {
                        "Common" => Some(None),
                        n => MdKind::from_name(n).map(Some),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let after = field("groups_inside")
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((MdKind::from_name(k)?, kinds_of(v))))
                    .collect()
            })
            .unwrap_or_default();
        let children = field("children")
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| {
                        let names = v
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        (k.clone(), names)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Order {
            top,
            common: kinds_of(&field("common")),
            after,
            children,
        }
    })
}

fn common_kinds() -> &'static [MdKind] {
    &order().common
}

fn nested_in(owner: MdKind) -> &'static [MdKind] {
    order().after.get(&owner).map_or(&[], Vec::as_slice)
}

fn is_nested(kind: MdKind) -> bool {
    order().after.values().any(|v| v.contains(&kind))
}

fn top_kinds() -> &'static [MdKind] {
    static TOP: OnceLock<Vec<MdKind>> = OnceLock::new();
    TOP.get_or_init(|| {
        let o = order();
        let mut v = Vec::new();
        for k in o.top.iter().flatten() {
            v.push(*k);
            v.extend(o.after.get(k).into_iter().flatten().copied());
        }
        v
    })
}

pub fn kind_order() -> Vec<MdKind> {
    common_kinds()
        .iter()
        .chain(top_kinds())
        .copied()
        .chain(rest())
        .collect()
}

fn rest() -> impl Iterator<Item = MdKind> {
    MdKind::all().iter().copied().filter(|k| {
        *k != MdKind::Configuration && !common_kinds().contains(k) && !top_kinds().contains(k)
    })
}

fn group_rank(owner: MdKind, icon: &str) -> usize {
    let name = icon.strip_prefix("folder:").unwrap_or(icon);
    let name = name.strip_suffix('s').unwrap_or(name);
    order()
        .children
        .get(owner.as_str())
        .and_then(|l| l.iter().position(|x| x == name))
        .unwrap_or(usize::MAX)
}

const CHILD_ORDER: [ChildTag; 14] = [
    ChildTag::Attribute,
    ChildTag::TabularSection,
    ChildTag::Dimension,
    ChildTag::Resource,
    ChildTag::EnumValue,
    ChildTag::AddressingAttribute,
    ChildTag::AccountingFlag,
    ChildTag::ExtDimensionAccountingFlag,
    ChildTag::Column,
    ChildTag::URLTemplate,
    ChildTag::Method,
    ChildTag::Operation,
    ChildTag::Parameter,
    ChildTag::IntegrationServiceChannel,
];

fn module_rank(k: ModuleKind) -> u8 {
    match k {
        ModuleKind::Module => 0,
        ModuleKind::ObjectModule => 1,
        ModuleKind::ManagerModule => 2,
        ModuleKind::ValueManagerModule => 3,
        ModuleKind::RecordSetModule => 4,
        ModuleKind::CommandModule => 5,
        ModuleKind::FormModule => 6,
        ModuleKind::ManagedApplicationModule => 7,
        ModuleKind::OrdinaryApplicationModule => 8,
        ModuleKind::SessionModule => 9,
        ModuleKind::ExternalConnectionModule => 10,
    }
}

fn synonym(s: &LocalString, lang: Lang) -> Option<String> {
    let code = if lang == Lang::Ru { "ru" } else { "en" };
    s.0.iter()
        .find(|(c, _)| c == code)
        .or_else(|| s.0.first())
        .map(|(_, t)| t.clone())
        .filter(|t| !t.is_empty())
}

struct Builder<'a> {
    cfg: &'a Configuration,
    lang: Lang,
}

impl Builder<'_> {
    fn group(&self, key: &str) -> TreeNode {
        TreeNode::new(group_name(key, self.lang), NodeRef::Folder(key))
    }

    fn module_nodes(&self, mods: &[&Module], path: &str) -> Option<TreeNode> {
        if mods.is_empty() {
            return None;
        }
        let mut sorted: Vec<&&Module> = mods.iter().collect();
        sorted.sort_by_key(|m| module_rank(m.kind));
        let mut g = self.group("Modules");
        for m in sorted {
            let en = module_name(m.kind, Lang::En);
            let mut n = TreeNode::new(module_name(m.kind, self.lang), NodeRef::Module(m.kind))
                .with_path(path.to_string());
            n.module = Some(en);
            g.children.push(n);
        }
        Some(g)
    }

    fn below(budget: Option<usize>) -> Option<usize> {
        budget.map(|b| b.saturating_sub(1))
    }

    fn child(&self, c: &Child, path: &str, budget: Option<usize>) -> TreeNode {
        let path = format!("{path}.{}.{}", child_tag_en(c.tag), c.name);
        let mut n = TreeNode::new(&c.name, NodeRef::Child(c.tag)).with_path(path.clone());
        n.synonym = synonym(&c.synonym, self.lang);
        n.type_text = c.type_desc().map(|t| type_text(t, self.lang));
        if budget != Some(0) {
            for c2 in &c.children {
                n.children.push(self.child(c2, &path, Self::below(budget)));
            }
        }
        n
    }

    fn sub(&self, owner_path: &str, s: &SubObject) -> TreeNode {
        let path = format!("{owner_path}.{}.{}", sub_kind_en(s.kind), s.name);
        let mut n = TreeNode::new(&s.name, NodeRef::Sub(s.kind)).with_path(path);
        n.synonym = s
            .props
            .iter()
            .find(|p| p.name == "Synonym")
            .and_then(|p| match &p.value {
                Value::LStr(l) => synonym(l, self.lang),
                _ => None,
            });
        n
    }

    fn recalc(
        &self,
        owner_path: &str,
        r: &mdconf::Recalculation,
        budget: Option<usize>,
    ) -> TreeNode {
        let path = format!("{owner_path}.Recalculation.{}", r.name);
        let mut n = TreeNode::new(&r.name, NodeRef::Recalc).with_path(path.clone());
        n.synonym = synonym(&r.synonym, self.lang);
        if let (Some(m), true) = (&r.module, budget != Some(0)) {
            n.children
                .extend(self.module_nodes(&[m], &path).map(|mut g| {
                    if Self::below(budget) == Some(0) {
                        g.children.clear();
                    }
                    g
                }));
        }
        n
    }

    fn group_node(
        &self,
        key: &str,
        budget: Option<usize>,
        items: impl FnOnce() -> Vec<TreeNode>,
    ) -> TreeNode {
        let mut g = self.group(key);
        if budget != Some(0) {
            g.children = items();
        }
        g
    }

    fn object(&self, o: &MdObject, path: String, budget: Option<usize>) -> TreeNode {
        let mut n = TreeNode::new(&o.name, NodeRef::Object(o.kind)).with_path(path.clone());
        n.synonym = synonym(&o.synonym, self.lang);
        n.type_text = match o.prop("Type") {
            Some(Value::Type(t)) => Some(type_text(t, self.lang)),
            _ => None,
        };
        if budget == Some(0) {
            return n;
        }
        let gb = Self::below(budget);
        let ib = Self::below(gb);
        for s in &o.subsystems {
            let p = format!("{path}.Subsystem.{}", s.name);
            n.children.push(self.object(s, p, gb));
        }
        let mut groups: Vec<TreeNode> = Vec::new();
        for tag in CHILD_ORDER {
            let items: Vec<&Child> = o.children.iter().filter(|c| c.tag == tag).collect();
            if items.is_empty() {
                continue;
            }
            groups.push(self.group_node(child_tag_en(tag), gb, || {
                items.iter().map(|c| self.child(c, &path, ib)).collect()
            }));
        }
        if !o.forms.is_empty() {
            groups.push(self.group_node("Forms", gb, || {
                o.forms.iter().map(|f| self.sub(&path, f)).collect()
            }));
        }
        let cmds: Vec<&Child> = o
            .children
            .iter()
            .filter(|c| c.tag == ChildTag::Command)
            .collect();
        if !cmds.is_empty() {
            groups.push(self.group_node("Command", gb, || {
                cmds.iter().map(|c| self.child(c, &path, ib)).collect()
            }));
        }
        let funcs: Vec<&Child> = o
            .children
            .iter()
            .filter(|c| c.tag == ChildTag::Function)
            .collect();
        if !funcs.is_empty() {
            groups.push(self.group_node("Function", gb, || {
                funcs.iter().map(|c| self.child(c, &path, ib)).collect()
            }));
        }
        if !o.templates.is_empty() {
            groups.push(self.group_node("Templates", gb, || {
                o.templates.iter().map(|f| self.sub(&path, f)).collect()
            }));
        }
        if !o.recalculations.is_empty() {
            groups.push(self.group_node("Recalculations", gb, || {
                o.recalculations
                    .iter()
                    .map(|r| self.recalc(&path, r, ib))
                    .collect()
            }));
        }
        groups.sort_by_key(|g| group_rank(o.kind, g.icon.as_deref().unwrap_or_default()));
        n.children.extend(groups);
        let mods: Vec<&Module> = o.modules.iter().collect();
        if let Some(mut g) = self.module_nodes(&mods, &path) {
            if gb == Some(0) {
                g.children.clear();
            }
            n.children.push(g);
        }
        n
    }

    fn section(
        &self,
        kind: MdKind,
        budget: Option<usize>,
        want: &dyn Fn(MdKind) -> bool,
    ) -> Option<TreeNode> {
        let of = |k: MdKind| -> Vec<&MdObject> {
            if want(k) {
                self.cfg.objects.iter().filter(|o| o.kind == k).collect()
            } else {
                Vec::new()
            }
        };
        let objs = of(kind);
        let nested: Vec<(MdKind, Vec<&MdObject>)> = nested_in(kind)
            .iter()
            .map(|k| (*k, of(*k)))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        if objs.is_empty() && nested.is_empty() {
            return None;
        }
        let mut s = TreeNode::new(section_name(kind, self.lang), NodeRef::Section(kind));
        if budget != Some(0) {
            for (k, list) in nested {
                let mut g = TreeNode::new(section_name(k, self.lang), NodeRef::Section(k));
                for o in list {
                    let p = format!("{}.{}", kind_name(k, Lang::En), o.name);
                    g.children.push(self.object(o, p, Self::below(budget)));
                }
                s.children.push(g);
            }
            for o in objs {
                let p = format!("{}.{}", kind_name(kind, Lang::En), o.name);
                s.children.push(self.object(o, p, Self::below(budget)));
            }
        }
        Some(s)
    }
}

pub fn config_name(cfg: &Configuration) -> String {
    cfg.props
        .iter()
        .find(|p| p.name == "Name")
        .and_then(|p| match &p.value {
            Value::Text(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub fn tree(cfg: &Configuration, depth: Option<usize>, kinds: &[MdKind], lang: Lang) -> TreeNode {
    let mut root =
        TreeNode::new(config_name(cfg), NodeRef::Root).with_path("Configuration".to_string());
    if depth == Some(0) {
        return root;
    }
    let b = Builder { cfg, lang };
    let want = |k: MdKind| kinds.is_empty() || kinds.contains(&k);
    let sb = Builder::below(depth);
    let present = |k: &MdKind| want(*k) && cfg.objects.iter().any(|o| o.kind == *k);
    if common_kinds().iter().any(present) {
        let mut common = TreeNode::new(group_name("Common", lang), NodeRef::Group("Common"));
        common.children = common_kinds()
            .iter()
            .copied()
            .filter(|k| want(*k))
            .filter_map(|k| b.section(k, sb, &want))
            .collect();
        root.children.push(common);
    }
    for k in top_kinds()
        .iter()
        .copied()
        .chain(rest())
        .filter(|k| !is_nested(*k))
    {
        root.children.extend(b.section(k, sb, &want));
    }
    root
}

pub fn render_text(root: &TreeNode, out: &mut dyn std::io::Write) -> crate::Result<()> {
    let mut stack: Vec<(&TreeNode, usize)> = vec![(root, 0)];
    while let Some((n, level)) = stack.pop() {
        write!(out, "{}{}", "  ".repeat(level), n.label)?;
        if let Some(s) = &n.synonym {
            write!(out, " ({s})")?;
        }
        if let Some(t) = n.type_text.as_deref().filter(|t| !t.is_empty()) {
            write!(out, ": {t}")?;
        }
        writeln!(out)?;
        stack.extend(n.children.iter().rev().map(|c| (c, level + 1)));
    }
    Ok(())
}
