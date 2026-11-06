use super::{Builder, QTableId, QuerySchema};
use brace::Guid;
use md_model::{Child, ChildTag, MdKind, MdObject, MdRef, Value};

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Facts {
    pub graph: Option<QTableId>,
    pub date: Option<Guid>,
    pub resource: Option<Guid>,
    pub links: Vec<(Guid, Guid)>,
    pub graph_fields: Vec<Guid>,
    pub owner_fields: Vec<Guid>,
    pub base: bool,
    pub error: Option<&'static str>,
}

fn reference<'a>(o: &'a MdObject, key: &str) -> Option<&'a MdRef> {
    match o.prop(key) {
        Some(Value::Ref(r)) if !r.name.is_empty() || r.id.is_some() => Some(r),
        _ => None,
    }
}
fn named(actual: &str, expected: &str) -> bool {
    bsl_prim::fold_name(actual) == bsl_prim::fold_name(expected)
}
fn child<'a>(o: &'a MdObject, r: &MdRef, tag: ChildTag) -> Option<&'a Child> {
    let segment = match tag {
        ChildTag::Dimension => "Dimension",
        ChildTag::Resource => "Resource",
        _ => return None,
    };
    let mut matches = o.children.iter().filter(|c| {
        c.tag == tag
            && r.id.is_none_or(|id| id == c.id)
            && (r.name.is_empty()
                || named(
                    &r.name,
                    &format!("InformationRegister.{}.{segment}.{}", o.name, c.name),
                ))
    });
    let c = matches.next()?;
    if matches.next().is_some() {
        None
    } else {
        Some(c)
    }
}

pub(super) fn build(b: &mut Builder<'_>, owner: QTableId, o: &MdObject) {
    let mut facts = Facts {
        base: matches!(o.prop("BasePeriod"), Some(Value::Bool(true))),
        ..Default::default()
    };
    let r = reference(o, "Schedule");
    let graph = r.and_then(|r| {
        let mut objects = b.cfg.objects.iter().filter(|g| {
            g.kind == MdKind::InformationRegister
                && r.id.is_none_or(|id| id == g.id)
                && (r.name.is_empty() || named(&r.name, &format!("InformationRegister.{}", g.name)))
        });
        let g = objects.next()?;
        if objects.next().is_some() {
            None
        } else {
            Some(g)
        }
    });
    if let Some(g) = graph {
        facts.graph = b.by_object.get(&g.id).copied();
        facts.date = reference(o, "ScheduleDate")
            .and_then(|r| child(g, r, ChildTag::Dimension))
            .map(|c| c.id);
        facts.resource = reference(o, "ScheduleValue")
            .and_then(|r| child(g, r, ChildTag::Resource))
            .map(|c| c.id);
        facts.graph_fields.extend(facts.date);
        facts.graph_fields.extend(facts.resource);
        for c in &o.children {
            let Some(Value::Ref(r)) = c.prop("ScheduleLink") else {
                continue;
            };
            if r.name.is_empty() && r.id.is_none() {
                continue;
            }
            facts.owner_fields.push(c.id);
            if matches!(c.tag, ChildTag::Dimension | ChildTag::Attribute)
                && let Some(target) = child(g, r, ChildTag::Dimension)
            {
                facts.graph_fields.push(target.id);
                facts.links.push((c.id, target.id));
                continue;
            }
            facts.error = Some("Q6c-ScheduleDataLinkShape");
        }
        if !matches!(g.prop("InformationRegisterPeriodicity"),Some(Value::Text(s)) if s=="Nonperiodical")
            || !matches!(g.prop("WriteMode"),Some(Value::Text(s)) if s=="Independent")
        {
            facts.error = Some("Q6c-ScheduleDataScheduleShape");
        }
        let fields = facts.graph.and_then(|t| b.table_ref(t));
        let field = |id| fields.and_then(|t| t.fields.iter().find(|f| f.md == Some(id)));
        if facts.date.and_then(field).is_none_or(|f| !f.ty.only_date())
            || facts
                .resource
                .and_then(field)
                .is_none_or(|f| !f.ty.only_number())
        {
            facts.error = Some("Q6c-ScheduleDataTypes");
        }
        let owner_fields = b.table_ref(owner);
        for (from, to) in &facts.links {
            let a = owner_fields.and_then(|t| t.fields.iter().find(|f| f.md == Some(*from)));
            let z = field(*to);
            if a.zip(z).is_none_or(|(a, z)| {
                a.ty != z.ty
                    || !(a.ty.only_number()
                        || a.ty.only_string()
                        || a.ty.only_date()
                        || a.ty.only_bool()
                        || (!a.ty.refs.is_empty() && a.ty.atom_count() == a.ty.refs.len()))
            }) {
                facts.error = Some("Q6c-ScheduleDataTypes");
            }
        }
        let mut targets = std::collections::HashSet::new();
        for (_, target) in &facts.links {
            if Some(*target) == facts.date || !targets.insert(*target) {
                facts.error = Some("Q6c-ScheduleDataLinkShape");
            }
        }
        if g.children
            .iter()
            .filter(|c| c.tag == ChildTag::Dimension)
            .any(|c| Some(c.id) != facts.date && !targets.contains(&c.id))
        {
            facts.error = Some("Q6c-ScheduleDataLinkShape");
        }
        if facts.graph.is_none() || facts.date.is_none() || facts.resource.is_none() {
            facts.error = Some("Q6c-ScheduleDataMetadata");
        }
    } else {
        facts.error = Some("Q6c-ScheduleDataMetadata");
    }
    if !matches!(o.prop("Periodicity"),Some(Value::Text(s)) if s=="Month") {
        facts.error = Some("Q6c-ScheduleDataPeriodicity");
    }
    b.schedules.insert(owner, facts);
}

impl QuerySchema {
    pub(crate) fn schedule(&self, table: QTableId) -> Option<&Facts> {
        self.table(table)
            .owner
            .and_then(|owner| self.schedules.get(&owner))
    }
    pub(crate) fn schedule_roles(&self, table: QTableId) -> Vec<super::StdRole> {
        use super::StdRole::*;
        let mut roles = vec![
            Recorder,
            LineNumber,
            RegistrationPeriod,
            ActionPeriod,
            BegOfActionPeriod,
            EndOfActionPeriod,
        ];
        if self.schedule(table).is_some_and(|f| f.base) {
            roles.extend([BegOfBasePeriod, EndOfBasePeriod]);
        }
        roles
    }
}
