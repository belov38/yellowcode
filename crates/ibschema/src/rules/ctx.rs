use std::collections::HashMap;

use brace::Guid;
use md_model::{Child, ChildTag, Configuration, MdKind, MdObject, MdRef, Value};

use super::types::Class;
use crate::error::{Error, Result};

pub(crate) trait Props {
    fn value(&self, name: &str) -> Option<&Value>;

    fn bool_or(&self, name: &str, default: bool) -> bool {
        match self.value(name) {
            Some(Value::Bool(b)) => *b,
            Some(Value::Text(t)) if t == "true" => true,
            Some(Value::Text(t)) if t == "false" => false,
            _ => default,
        }
    }

    fn flag(&self, name: &str) -> bool {
        self.bool_or(name, false)
    }

    fn text(&self, name: &str) -> Option<&str> {
        match self.value(name) {
            Some(Value::Text(t)) => Some(t),
            _ => None,
        }
    }

    fn num(&self, name: &str, default: u32) -> u32 {
        self.text(name)
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(default)
    }

    fn refs(&self, name: &str) -> &[MdRef] {
        match self.value(name) {
            Some(Value::RefList(v)) => v,
            Some(Value::Ref(r)) if r.id.is_some() || !r.name.is_empty() => std::slice::from_ref(r),
            _ => &[],
        }
    }
}

impl Props for MdObject {
    fn value(&self, name: &str) -> Option<&Value> {
        self.prop(name)
    }
}

impl Props for Child {
    fn value(&self, name: &str) -> Option<&Value> {
        self.prop(name)
    }
}

pub(crate) fn full_name(o: &MdObject) -> String {
    format!("{}.{}", o.kind.as_str(), o.name)
}

pub(crate) fn tag_name(t: ChildTag) -> &'static str {
    match t {
        ChildTag::Attribute => "Attribute",
        ChildTag::TabularSection => "TabularSection",
        ChildTag::Dimension => "Dimension",
        ChildTag::Resource => "Resource",
        ChildTag::EnumValue => "EnumValue",
        ChildTag::Command => "Command",
        ChildTag::AddressingAttribute => "AddressingAttribute",
        ChildTag::AccountingFlag => "AccountingFlag",
        ChildTag::ExtDimensionAccountingFlag => "ExtDimensionAccountingFlag",
        ChildTag::Column => "Column",
        ChildTag::URLTemplate => "URLTemplate",
        ChildTag::Method => "Method",
        ChildTag::Operation => "Operation",
        ChildTag::Parameter => "Parameter",
        ChildTag::IntegrationServiceChannel => "IntegrationServiceChannel",
        ChildTag::Function => "Function",
    }
}

pub(crate) fn md_name(owner: &str, c: &Child) -> String {
    format!("{owner}.{}.{}", tag_name(c.tag), c.name)
}

pub(crate) fn refers_to(r: &MdRef, o: &MdObject) -> bool {
    match r.id {
        Some(id) => id == o.id,
        None => r.name == full_name(o),
    }
}

pub(crate) struct Lookup<'a> {
    pub cfg: &'a Configuration,
    by_id: HashMap<Guid, &'a MdObject>,
    by_name: HashMap<(MdKind, &'a str), &'a MdObject>,
    by_kind: HashMap<MdKind, Vec<&'a MdObject>>,
    pub common: super::common::CommonAttrs<'a>,
}

impl<'a> Lookup<'a> {
    pub fn new(cfg: &'a Configuration) -> Lookup<'a> {
        let mut l = Lookup {
            cfg,
            by_id: HashMap::new(),
            by_name: HashMap::new(),
            by_kind: HashMap::new(),
            common: Default::default(),
        };
        for o in &cfg.objects {
            l.by_id.entry(o.id).or_insert(o);
            l.by_name.entry((o.kind, o.name.as_str())).or_insert(o);
            l.by_kind.entry(o.kind).or_default().push(o);
        }
        l.common = super::common::CommonAttrs::new(l.of_kind(MdKind::CommonAttribute));
        l
    }

    pub fn object(&self, id: Guid) -> Option<&'a MdObject> {
        self.by_id.get(&id).copied()
    }

    pub fn find(&self, kind: MdKind, name: &str) -> Option<&'a MdObject> {
        self.by_name.get(&(kind, name)).copied()
    }

    pub fn of_kind(&self, kind: MdKind) -> &[&'a MdObject] {
        self.by_kind.get(&kind).map_or(&[], Vec::as_slice)
    }

    pub fn resolve(&self, r: &MdRef) -> Option<&'a MdObject> {
        if let Some(id) = r.id {
            return self.object(id);
        }
        let (kind, name) = r.name.split_once('.')?;
        self.find(MdKind::from_name(kind)?, name)
    }

    pub fn resolve_all(&self, list: &[MdRef], at: &str) -> Result<Vec<&'a MdObject>> {
        list.iter()
            .map(|r| self.resolve(r).ok_or_else(|| unknown(at, r)))
            .collect()
    }
}

fn unknown(at: &str, r: &MdRef) -> Error {
    let what = match (&r.id, r.name.is_empty()) {
        (_, false) => r.name.clone(),
        (Some(id), true) => id.to_string(),
        (None, true) => "<empty>".into(),
    };
    Error::Parse {
        what: "configuration",
        at: at.to_string(),
        why: format!("unknown object {what}"),
    }
}

pub(crate) fn is_balance(obj: &MdObject) -> bool {
    obj.text("RegisterType").unwrap_or("Balance") == "Balance"
}

pub(crate) struct Ctx<'a> {
    pub cfg: &'a Configuration,
    pub md: &'a Lookup<'a>,
    pub obj: &'a MdObject,
    pub kind: MdKind,
    pub full_name: String,
    pub owners: Vec<&'a MdObject>,
    pub hier: bool,
    pub folders: bool,
    pub code_len: u32,
    pub descr_len: u32,
    pub order_len: u32,
    pub code: Class,
    pub num_len: u32,
    pub periodic: bool,
    pub number: Class,
    pub recpos: bool,
    pub sub: bool,
    pub slice_first: bool,
    pub slice_last: bool,
    pub balance: bool,
    pub split: bool,
    pub corr: bool,
    pub action: bool,
    pub base: bool,
    pub apu: bool,
    pub dep: bool,
    pub base_charts: Vec<&'a MdObject>,
    pub recorders: Vec<&'a MdObject>,
    pub dims: bool,
    pub base_dims: bool,
    pub docs: Vec<&'a MdObject>,
    pub bps: Vec<&'a MdObject>,
    pub addressing: bool,
    pub ed: u32,
    pub coa: Option<&'a MdObject>,
    pub predefined: bool,
    pub in_exchange_plan: bool,
}

fn code_class(p: &dyn Props, len: u32, type_prop: &str, allowed_prop: &str) -> Class {
    if p.text(type_prop) == Some("Number") {
        Class::N {
            digits: len,
            fraction: 0,
        }
    } else {
        Class::S {
            len,
            fixed: p.text(allowed_prop) == Some("Fixed"),
        }
    }
}

impl<'a> Ctx<'a> {
    pub fn new(md: &'a Lookup<'a>, obj: &'a MdObject) -> Result<Ctx<'a>> {
        let full = full_name(obj);
        let at = |prop: &str| format!("{full}.{prop}");
        let kind = obj.kind;
        let owners = md.resolve_all(obj.refs("Owners"), &at("Owners"))?;
        let hier = obj.flag("Hierarchical");
        let code_len = obj.num("CodeLength", 9);
        let descr_len = obj.num("DescriptionLength", 25);
        let order_len = obj.num("OrderLength", 0);
        for (prop, len) in [("DescriptionLength", descr_len), ("OrderLength", order_len)] {
            if len >= crate::logical::VAR {
                return Err(Error::Unsupported(format!(
                    "{}: {len} is out of range",
                    at(prop)
                )));
            }
        }
        let numerator = match obj.refs("Numerator").first() {
            Some(r) => Some(md.resolve(r).ok_or_else(|| unknown(&at("Numerator"), r))?),
            None => None,
        };
        let q: &dyn Props = match numerator {
            Some(n) => n,
            None => obj,
        };
        let num_len = q.num("NumberLength", 9);
        let mut periodic =
            q.text("NumberPeriodicity").unwrap_or("Nonperiodical") != "Nonperiodical";
        let (mut recpos, mut sub) = (false, false);
        if kind == MdKind::InformationRegister {
            let per = obj
                .text("InformationRegisterPeriodicity")
                .unwrap_or("Nonperiodical");
            periodic = per != "Nonperiodical";
            recpos = per == "RecorderPosition";
            sub = obj.text("WriteMode") == Some("RecorderSubordinate");
        }
        let recorders = match kind {
            MdKind::InformationRegister
            | MdKind::AccumulationRegister
            | MdKind::AccountingRegister
            | MdKind::CalculationRegister => md
                .of_kind(MdKind::Document)
                .iter()
                .copied()
                .filter(|d| d.refs("RegisterRecords").iter().any(|r| refers_to(r, obj)))
                .collect(),
            _ => Vec::new(),
        };
        let bps = if kind == MdKind::Task {
            md.of_kind(MdKind::BusinessProcess)
                .iter()
                .copied()
                .filter(|b| b.refs("Task").iter().any(|r| refers_to(r, obj)))
                .collect()
        } else {
            Vec::new()
        };
        let docs = match obj.refs("Documents") {
            [] => md.resolve_all(obj.refs("RegisteredDocuments"), &at("RegisteredDocuments"))?,
            list => md.resolve_all(list, &at("Documents"))?,
        };
        let coa = match (kind, obj.refs("ChartOfAccounts").first()) {
            (MdKind::AccountingRegister, Some(r)) => Some(
                md.resolve(r)
                    .ok_or_else(|| unknown(&at("ChartOfAccounts"), r))?,
            ),
            _ => None,
        };
        let ed = match kind {
            MdKind::ChartOfAccounts => obj.num("MaxExtDimensionCount", 0),
            MdKind::AccountingRegister => coa.map_or(0, |c| c.num("MaxExtDimensionCount", 0)),
            _ => 0,
        };
        let mut cx = Ctx {
            cfg: md.cfg,
            md,
            obj,
            kind,
            owners,
            hier,
            folders: hier && obj.text("HierarchyType") != Some("HierarchyOfItems"),
            code_len,
            descr_len,
            order_len,
            code: code_class(obj, code_len, "CodeType", "CodeAllowedLength"),
            num_len,
            periodic,
            number: code_class(q, num_len, "NumberType", "NumberAllowedLength"),
            recpos,
            sub,
            slice_first: obj.flag("EnableTotalsSliceFirst"),
            slice_last: obj.flag("EnableTotalsSliceLast"),
            balance: is_balance(obj),
            split: obj.flag("EnableTotalsSplitting"),
            corr: obj.flag("Correspondence"),
            action: obj.flag("ActionPeriod"),
            base: obj.flag("BasePeriod"),
            apu: obj.flag("ActionPeriodUse"),
            dep: obj
                .text("DependenceOnCalculationTypes")
                .unwrap_or("DontUse")
                != "DontUse",
            base_charts: md.resolve_all(
                obj.refs("BaseCalculationTypes"),
                &at("BaseCalculationTypes"),
            )?,
            recorders,
            dims: obj.dimensions().next().is_some(),
            base_dims: obj.dimensions().any(|d| d.flag("BaseDimension")),
            docs,
            bps,
            addressing: obj
                .children
                .iter()
                .any(|c| c.tag == ChildTag::AddressingAttribute),
            ed,
            coa,
            predefined: false,
            in_exchange_plan: false,
            full_name: full,
        };
        cx.predefined = has_predefined(&cx);
        cx.in_exchange_plan = in_exchange_plans(&cx);
        Ok(cx)
    }

    pub fn child_name(&self, c: &Child) -> String {
        md_name(&self.full_name, c)
    }
}

pub(crate) fn has_predefined(cx: &Ctx) -> bool {
    cx.obj.predefined().is_some_and(|p| !p.items.is_empty())
}

pub(crate) fn in_exchange_plans(cx: &Ctx) -> bool {
    cx.md.of_kind(MdKind::ExchangePlan).iter().any(|plan| {
        plan.exchange_content()
            .is_some_and(|c| c.iter().any(|it| refers_to(&it.object, cx.obj)))
    })
}
