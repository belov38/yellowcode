use super::*;
use bsl_syntax::ast::{Expr, ExprKind, Literal};
use mdconf::{ChildTag, MdKind};
use platform_catalog::{Context, MethodInfo, ResultType, TypeInfo, catalog};
#[derive(Clone, PartialEq, Eq)]
pub(super) enum Atom {
    Platform(String, Option<usize>),
    Path(Vec<String>),
    Nullable,
}
pub(super) type Ty = Option<Vec<Atom>>;
pub(super) struct Resolver<'a> {
    cfg: &'a Configuration,
    pub started: Instant,
    pub limited: bool,
    visits: usize,
}
impl<'a> Resolver<'a> {
    pub fn new(cfg: &'a Configuration) -> Self {
        let _ = catalog();
        Self {
            cfg,
            started: Instant::now(),
            limited: false,
            visits: 0,
        }
    }
    pub fn step(&mut self) -> bool {
        self.visits += 1;
        if self.visits > 10000 || self.started.elapsed() > Duration::from_millis(50) {
            self.limited = true;
        }
        !self.limited
    }
    fn platform(&self, name: &str, binding: Option<usize>) -> Ty {
        let t = catalog()
            .types()
            .iter()
            .filter(|t| t.avail.has(Context::Server))
            .find(|t| {
                super::super::same(&t.id, name)
                    || super::super::same(&t.name_ru, name)
                    || super::super::same(&t.name_en, name)
            })?;
        Some(vec![Atom::Platform(t.id.clone(), binding)])
    }
    fn info(id: &str) -> Option<&'static TypeInfo> {
        catalog().types().iter().find(|t| t.id == id)
    }
    fn template(&self, stem: &str, binding: usize) -> Ty {
        let t = catalog().types().iter().find(|t| {
            t.avail.has(Context::Server)
                && t.metadata_template
                && t.id.split_once(".<").is_some_and(|(s, _)| s == stem)
        })?;
        Some(vec![Atom::Platform(t.id.clone(), Some(binding))])
    }
    fn results(&mut self, rs: &[ResultType], owner: &Atom) -> Ty {
        if rs.len() > 8 {
            self.limited = true;
            return None;
        }
        if rs.is_empty() {
            return None;
        }
        let mut out = vec![];
        for r in rs {
            let id = r.id.as_deref()?;
            if matches!(
                id,
                "Undefined" | "Null" | "lang:def_Undefined" | "lang:def_Null"
            ) {
                out.push(Atom::Nullable);
                continue;
            }
            let binding = if id.contains(".<") {
                match owner {
                    Atom::Platform(old, b)
                        if old.starts_with("Catalog") && id.starts_with("Catalog")
                            || old.starts_with("Document") && id.starts_with("Document") =>
                    {
                        *b
                    }
                    _ => None,
                }
            } else {
                None
            };
            let mapped = id.strip_prefix("lang:def_").unwrap_or(id);
            let mut v = self.platform(mapped, binding)?;
            out.append(&mut v);
        }
        Some(out)
    }
    pub fn has_call(&self, e: &Expr) -> bool {
        let mut todo = vec![e];
        while let Some(e) = todo.pop() {
            match &e.kind {
                ExprKind::Call { .. } | ExprKind::NewDynamic { .. } => return true,
                ExprKind::New {
                    args: Some(args), ..
                } => {
                    for arg in args {
                        if let Some(value) = &arg.value {
                            todo.push(value);
                        }
                    }
                }
                ExprKind::Member { object, .. }
                | ExprKind::Paren(object)
                | ExprKind::Unary {
                    operand: object, ..
                }
                | ExprKind::Await(object) => todo.push(object),
                ExprKind::Binary { lhs, rhs, .. } => {
                    todo.push(lhs);
                    todo.push(rhs)
                }
                ExprKind::Index { object, index } => {
                    todo.push(object);
                    todo.push(index)
                }
                ExprKind::Ternary {
                    cond,
                    then,
                    otherwise,
                } => {
                    todo.push(cond);
                    todo.push(then);
                    todo.push(otherwise)
                }
                _ => {}
            }
        }
        false
    }
    pub fn expr(&mut self, e: &Expr, env: &HashMap<String, Ty>, depth: usize) -> Ty {
        if depth > 64 {
            self.limited = true;
            return None;
        }
        if !self.step() {
            return None;
        }
        match &e.kind {
            ExprKind::New { type_name, .. } => self.platform(&type_name.name, None),
            ExprKind::Name(n) => {
                let key = bsl_prim::fold_name(&n.name);
                if let Some(v) = env.get(&key) {
                    return v.clone();
                }
                if [
                    "Справочники",
                    "Catalogs",
                    "Документы",
                    "Documents",
                    "Метаданные",
                    "Metadata",
                ]
                .iter()
                .any(|x| super::super::same(x, &n.name))
                {
                    Some(vec![Atom::Path(vec![n.name.clone()])])
                } else if let Some(p) = catalog().global_properties().iter().find(|p| {
                    p.avail.has(Context::Server)
                        && (super::super::same(&p.name_ru, &n.name)
                            || super::super::same(&p.name_en, &n.name))
                }) {
                    self.results(&p.types, &Atom::Platform(String::new(), None))
                } else if catalog().types().iter().any(|t| {
                    t.enum_global
                        && t.avail.has(Context::Server)
                        && (super::super::same(&t.name_ru, &n.name)
                            || super::super::same(&t.name_en, &n.name))
                }) {
                    Some(vec![Atom::Path(vec![n.name.clone()])])
                } else {
                    None
                }
            }
            ExprKind::Paren(e) => self.expr(e, env, depth + 1),
            ExprKind::Lit(l) => match l {
                Literal::Null | Literal::Undefined => Some(vec![Atom::Nullable]),
                Literal::Number(_) => self.platform("Number", None),
                Literal::Str(_) => self.platform("String", None),
                Literal::Bool(_) => self.platform("Boolean", None),
                Literal::Date(_) => self.platform("Date", None),
            },
            ExprKind::Member { object, name } => {
                let t = self.expr(object, env, depth + 1)?;
                self.property(&t, &name.name)
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Name(n) = &callee.kind {
                    let m = &catalog()
                        .global_methods()
                        .iter()
                        .find(|m| {
                            m.info.avail.has(Context::Server)
                                && (super::super::same(&m.info.name_ru, &n.name)
                                    || super::super::same(&m.info.name_en, &n.name))
                        })?
                        .info;
                    return self.method_results(
                        m,
                        args.len(),
                        &Atom::Platform(String::new(), None),
                    );
                }
                let ExprKind::Member { object, name } = &callee.kind else {
                    return None;
                };
                let recv = self.expr(object, env, depth + 1)?;
                let mut all = vec![];
                for atom in recv {
                    if atom == Atom::Nullable {
                        continue;
                    }
                    let Atom::Platform(ref id, _) = atom else {
                        return None;
                    };
                    let t = Self::info(id)?;
                    if !t.avail.has(Context::Server) {
                        return None;
                    }
                    let m = t.methods.iter().find(|m| {
                        m.avail.has(Context::Server)
                            && (super::super::same(&m.name_ru, &name.name)
                                || super::super::same(&m.name_en, &name.name))
                    })?;
                    all.extend(self.method_results(m, args.len(), &atom)?);
                    if all.len() > 8 {
                        self.limited = true;
                        return None;
                    }
                }
                if all.is_empty() { None } else { Some(all) }
            }
            _ => None,
        }
    }
    fn method_results(&mut self, m: &MethodInfo, count: usize, owner: &Atom) -> Ty {
        let mut all = vec![];
        let mut matched = false;
        for s in &m.variants {
            let min = s
                .params
                .iter()
                .filter(|p| !p.optional && !p.variadic)
                .count();
            if count < min || count > s.params.len() && !s.params.last().is_some_and(|p| p.variadic)
            {
                continue;
            }
            matched = true;
            all.extend(self.results(s.return_types.as_deref()?, owner)?);
            if all.len() > 8 {
                self.limited = true;
                return None;
            }
        }
        (matched && !all.is_empty()).then_some(all)
    }
    fn find_object(
        &mut self,
        mut predicate: impl FnMut(&mdconf::MdObject) -> bool,
    ) -> Option<usize> {
        let cfg = self.cfg;
        for (i, object) in cfg.objects.iter().enumerate() {
            if !self.step() {
                return None;
            }
            if predicate(object) {
                return Some(i);
            }
        }
        None
    }
    fn description(&mut self, description: &mdconf::TypeDescription) -> Ty {
        let mut all = vec![];
        let cfg = self.cfg;
        if description.types.len() > 8 {
            self.limited = true;
            return None;
        }
        let mut pending: Vec<(&mdconf::TypeRef, Vec<String>)> =
            description.types.iter().map(|r| (r, vec![])).collect();
        let mut expanded = vec![];
        while let Some((reference, ancestors)) = pending.pop() {
            if !self.step() {
                return None;
            }
            if !reference.set {
                expanded.push(reference);
                if expanded.len() > 8 {
                    self.limited = true;
                    return None;
                }
                continue;
            }
            let local = reference.name.fixed_local(mdconf::FixedNamespace::Cfg)?;
            let (kind, name) = if let Some(name) = local.strip_prefix("DefinedType.") {
                (MdKind::DefinedType, name)
            } else {
                (
                    MdKind::ChartOfCharacteristicTypes,
                    local.strip_prefix("Characteristic.")?,
                )
            };
            let key = reference.name.to_string();
            if ancestors.contains(&key) {
                return None;
            }
            if ancestors.len() >= 16 {
                self.limited = true;
                return None;
            }
            let by_id = reference
                .type_id
                .and_then(|id| cfg.generated_type(id))
                .filter(|g| g.object.kind == kind)
                .map(|g| g.object);
            let object = if let Some(object) = by_id {
                object
            } else {
                let i =
                    self.find_object(|o| o.kind == kind && super::super::same(&o.name, name))?;
                cfg.objects.get(i)?
            };
            let Some(mdconf::Value::Type(inner)) = object.prop("Type") else {
                return None;
            };
            if inner.types.is_empty() {
                return None;
            }
            if inner.types.len() > 8 {
                self.limited = true;
                return None;
            }
            let mut ancestors = ancestors;
            ancestors.push(key);
            for r in &inner.types {
                pending.push((r, ancestors.clone()));
            }
            if pending.len() + expanded.len() > 8 {
                self.limited = true;
                return None;
            }
        }
        for tr in expanded {
            if tr.set {
                return None;
            }
            let local = &tr.name.local;
            if tr.name.fixed_local(mdconf::FixedNamespace::Cfg).is_some() {
                let (role, n) = local.split_once('.')?;
                let k = if role.starts_with("Catalog") {
                    MdKind::Catalog
                } else if role.starts_with("Document") {
                    MdKind::Document
                } else {
                    return None;
                };
                let generated = tr.type_id.and_then(|id| self.cfg.generated_type(id));
                let j = if let Some(g) = generated {
                    if g.child.is_some() || g.recalculation.is_some() {
                        return None;
                    }
                    self.find_object(|o| o.id == g.object.id && o.kind == k)?
                } else {
                    self.find_object(|o| o.kind == k && super::super::same(&o.name, n))?
                };
                all.extend(self.template(role, j)?);
            } else if tr.name.fixed_local(mdconf::FixedNamespace::Xs).is_some()
                || tr.name.fixed_local(mdconf::FixedNamespace::V8).is_some()
            {
                let p = match local.as_str() {
                    "string" => "String",
                    "decimal" => "Number",
                    "boolean" => "Boolean",
                    "dateTime" => "Date",
                    s => s,
                };
                all.extend(self.platform(p, None)?);
            } else {
                return None;
            }
        }
        if all.is_empty() { None } else { Some(all) }
    }
    fn common_attribute(&mut self, name: &str) -> Option<&'a mdconf::MdObject> {
        let i = self.find_object(|o| {
            o.kind == MdKind::CommonAttribute
                && super::super::same(&o.name, name)
                && Self::automatic(o)
        })?;
        self.cfg.objects.get(i)
    }
    fn automatic(o: &mdconf::MdObject) -> bool {
        matches!(o.prop("AutoUse"),Some(mdconf::Value::Text(v)) if v=="Use")
            && matches!(o.prop("Content"),Some(mdconf::Value::Xml(v)) if v.children.is_empty()&&v.text.as_ref().is_none_or(|s|s.trim().is_empty()))
    }
    fn property(&mut self, types: &[Atom], name: &str) -> Ty {
        let cfg = self.cfg;
        let mut all = vec![];
        for atom in types {
            match atom {
                Atom::Nullable => continue,
                Atom::Path(path) => {
                    let mut path = path.clone();
                    path.push(name.to_owned());
                    if path.len() == 2 {
                        let k = if super::super::same(path.first()?.as_str(), "Catalogs")
                            || super::super::same(path.first()?.as_str(), "Справочники")
                        {
                            Some(MdKind::Catalog)
                        } else if super::super::same(path.first()?.as_str(), "Documents")
                            || super::super::same(path.first()?.as_str(), "Документы")
                        {
                            Some(MdKind::Document)
                        } else {
                            None
                        };
                        if let Some(k) = k {
                            let i = self.find_object(|o| {
                                o.kind == k && super::super::same(&o.name, name)
                            })?;
                            all.extend(self.template(&format!("{}Manager", k.as_str()), i)?);
                            continue;
                        }
                    }
                    if path.len() > 16 {
                        self.limited = true;
                        return None;
                    }
                    all.push(Atom::Path(path));
                }
                Atom::Platform(id, binding) => {
                    let t = Self::info(id)?;
                    if !t.avail.has(Context::Server) {
                        return None;
                    }
                    if let Some(i) = binding.filter(|_| !id.contains("Manager")) {
                        let obj = cfg.objects.get(i)?;
                        if let Some(c) = obj
                            .children
                            .iter()
                            .find(|c| super::super::same(&c.name, name))
                        {
                            if c.tag != ChildTag::Attribute {
                                return None;
                            }
                            all.extend(self.description(c.type_desc()?)?);
                            continue;
                        }
                        if let Some(common) = self.common_attribute(name) {
                            let Some(mdconf::Value::Type(td)) = common.prop("Type") else {
                                return None;
                            };
                            all.extend(self.description(td)?);
                            continue;
                        }
                    }
                    let p = t.properties.iter().find(|p| {
                        p.avail.has(Context::Server)
                            && (super::super::same(&p.name_ru, name)
                                || super::super::same(&p.name_en, name))
                    })?;
                    if super::super::same(name, "Parent") || super::super::same(name, "Родитель")
                    {
                        let i = (*binding)?;
                        if self.cfg.objects.get(i)?.prop("Hierarchical")
                            != Some(&mdconf::Value::Bool(true))
                        {
                            return None;
                        }
                    }
                    if super::super::same(name, "Owner") || super::super::same(name, "Владелец")
                    {
                        let i = (*binding)?;
                        let Some(mdconf::Value::RefList(owners)) =
                            self.cfg.objects.get(i)?.prop("Owners")
                        else {
                            return None;
                        };
                        if owners.len() > 8 {
                            self.limited = true;
                            return None;
                        }
                        if owners.is_empty() {
                            return None;
                        }
                        for owner in owners {
                            let j = self.find_object(|o| {
                                owner.id == Some(o.id)
                                    || owner.name == format!("{}.{}", o.kind.as_str(), o.name)
                            })?;
                            let object = self.cfg.objects.get(j)?;
                            if !matches!(object.kind, MdKind::Catalog | MdKind::Document) {
                                return None;
                            }
                            all.extend(self.template(&format!("{}Ref", object.kind.as_str()), j)?);
                        }
                        continue;
                    }
                    all.extend(self.results(&p.types, atom)?);
                }
            }
            if all.len() > 8 {
                self.limited = true;
                return None;
            }
        }
        if all.is_empty() { None } else { Some(all) }
    }
    fn standard_applicable(object: &mdconf::MdObject, other: &str, name: &str) -> bool {
        if object.kind != MdKind::Catalog
            || object
                .children
                .iter()
                .any(|c| super::super::same(&c.name, name) || super::super::same(&c.name, other))
        {
            return true;
        }
        if ["Parent", "IsFolder", "Родитель", "ЭтоГруппа"]
            .iter()
            .any(|n| super::super::same(n, name) || super::super::same(n, other))
        {
            return object.prop("Hierarchical") == Some(&mdconf::Value::Bool(true));
        }
        if ["Owner", "Владелец"]
            .iter()
            .any(|n| super::super::same(n, name) || super::super::same(n, other))
        {
            return matches!(object.prop("Owners"),Some(mdconf::Value::RefList(v)) if !v.is_empty());
        }
        true
    }
    pub fn members(&mut self, ty: &Ty, out: &mut Builder) {
        let Some(atoms) = ty else { return };
        let nullable = atoms.contains(&Atom::Nullable);
        let mut intersection: Option<Vec<CompletionItem>> = None;
        for atom in atoms {
            if !self.step() {
                return;
            }
            if *atom == Atom::Nullable {
                continue;
            }
            let mut b = list("", out.lang);
            b.limit = 10000;
            match atom {
                Atom::Platform(id, binding) => {
                    let Some(t) = Self::info(id) else { return };
                    b.members(t);
                    if let Some(i) = binding.filter(|_| !id.contains("Manager")) {
                        let Some(object) = self.cfg.objects.get(i) else {
                            return;
                        };
                        let mut removed = vec![];
                        b.result.items.retain(|item| {
                            let applicable =
                                Self::standard_applicable(object, &item.name_other, &item.name);
                            if !applicable {
                                removed.push(bsl_prim::fold_name(&item.name));
                            }
                            applicable
                        });
                        for key in removed {
                            b.seen.remove(&key);
                        }
                        for c in &object.children {
                            if !self.step() {
                                return;
                            }
                            if matches!(c.tag, ChildTag::Attribute | ChildTag::TabularSection) {
                                b.add(CompletionItem::named(
                                    &c.name, &c.name, "property", out.lang,
                                ));
                            }
                        }
                        for common in self.cfg.objects.iter() {
                            if !self.step() {
                                return;
                            }
                            if common.kind != MdKind::CommonAttribute || !Self::automatic(common) {
                                continue;
                            }
                            b.add(CompletionItem::named(
                                &common.name,
                                &common.name,
                                "property",
                                out.lang,
                            ));
                        }
                    }
                }
                Atom::Path(path) => {
                    super::super::metadata::members(self.cfg, path, &mut b);
                }
                Atom::Nullable => {}
            }
            match &mut intersection {
                None => intersection = Some(b.result.items),
                Some(items) => {
                    items.retain(|a| {
                        b.result
                            .items
                            .iter()
                            .any(|b| a.kind == b.kind && super::super::same(&a.name, &b.name))
                    });
                    for other in b.result.items {
                        if let Some(item) = items.iter_mut().find(|a| {
                            a.kind == other.kind && super::super::same(&a.name, &other.name)
                        }) {
                            for sig in other.signatures {
                                if !item.signatures.iter().any(|s| {
                                    s.parameters == sig.parameters && s.variadic == sig.variadic
                                }) {
                                    item.signatures.push(sig);
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(items) = intersection {
            for mut item in items {
                if nullable {
                    item.note = Some(
                        if out.lang == Lang::Ru {
                            "Возможны Null/Неопределено"
                        } else {
                            "May be Null/Undefined"
                        }
                        .into(),
                    );
                }
                out.add(item)
            }
        }
    }
}
