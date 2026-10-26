use super::names;
use super::typemap::map_type;
use super::{
    Builder, FieldRole, Only, QField, QFieldId, QTable, QTableId, REF_KINDS, RefInfo, SchemaError,
    StdRole, TableKind, VtKind, number, string,
};
use crate::types::{DateQ, Other, QType, RefAtom};
use brace::Guid;
use bsl_prim::fold_name;
use md_model::{Child, ChildTag, MdObject, MdRef, TypeDescription, Value};
use mdschema::MdKind;
use std::collections::HashMap;

const PREDEFINED_NAME_LEN: u32 = 256;
const DATA_VERSION_LEN: u32 = 12;
const LINE_NUMBER_DIGITS: u8 = 9;
const MESSAGE_NO_DIGITS: u8 = 10;

struct Declared<'a> {
    obj: &'a MdObject,
    table: QTableId,
    reference: RefAtom,
}

pub(super) struct Common {
    pub id: Guid,
    pub name: String,
    pub ty: QType,
    auto_use: bool,
    pub separator: bool,
    content: Vec<(String, String)>,
}

impl Common {
    pub fn used_by(&self, o: &MdObject) -> bool {
        let full = format!("{}.{}", o.kind.as_str(), o.name);
        match self.content.iter().find(|(n, _)| *n == full) {
            Some((_, u)) if u == "Use" => true,
            Some((_, u)) if u == "DontUse" => false,
            _ => self.auto_use,
        }
    }
}

struct Fields {
    kind: MdKind,
    fields: Vec<QField>,
    star: Vec<QFieldId>,
}

impl Fields {
    fn new(kind: MdKind) -> Fields {
        Fields {
            kind,
            fields: Vec::new(),
            star: Vec::new(),
        }
    }

    fn push(&mut self, f: QField, in_star: bool) {
        if in_star && let Ok(i) = u16::try_from(self.fields.len()) {
            self.star.push(QFieldId(i));
        }
        self.fields.push(f);
    }

    fn std(&mut self, role: StdRole, ty: QType) {
        let f = std_field(role, self.kind, ty);
        self.push(f, true);
    }

    fn virt(&mut self, role: StdRole, ty: QType) {
        let f = QField {
            role: FieldRole::Virtual,
            ..std_field(role, self.kind, ty)
        };
        self.push(f, false);
    }

    fn into_table(self, t: &mut QTable) {
        t.fields = self.fields;
        t.star = self.star;
    }
}

pub(super) fn std_field(role: StdRole, kind: MdKind, ty: QType) -> QField {
    let (name_ru, name_en) = names::std_name(role, kind);
    QField {
        name_ru,
        name_en,
        ty,
        role: FieldRole::Std(role),
        only: Only::Any,
        nested: None,
        md: None,
        period: None,
        side: None,
    }
}

pub(super) fn text_prop<'o>(o: &'o MdObject, name: &str) -> Option<&'o str> {
    match o.prop(name) {
        Some(Value::Text(s)) => Some(s.as_str()),
        _ => None,
    }
}

pub(super) fn int_prop(o: &MdObject, name: &str) -> u32 {
    text_prop(o, name)
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

pub(super) fn bool_prop(o: &MdObject, name: &str) -> bool {
    matches!(o.prop(name), Some(Value::Bool(true)))
}

fn code_type(o: &MdObject, len: &str, ty: &str, allowed: &str) -> Option<QType> {
    let l = int_prop(o, len);
    if l == 0 {
        return None;
    }
    Some(match text_prop(o, ty) {
        Some("Number") => number(u8::try_from(l).unwrap_or(u8::MAX), 0, true),
        _ => string(l, text_prop(o, allowed) == Some("Fixed")),
    })
}

pub(super) fn sub_table(
    b: &Builder<'_>,
    owner_id: QTableId,
    (ru, en): &(String, String),
    kind: TableKind,
    object: Option<Guid>,
) -> Result<(QTable, Vec<String>), SchemaError> {
    let owner = b
        .table_ref(owner_id)
        .ok_or_else(|| SchemaError::Config("internal: table id out of range".into()))?;
    let t = QTable {
        name_ru: format!("{}.{ru}", owner.name_ru),
        name_en: format!("{}.{en}", owner.name_en),
        kind,
        object,
        owner: Some(owner_id),
        fields: Vec::new(),
        star: Vec::new(),
        params: Vec::new(),
        reference: None,
    };
    let extra = vec![
        format!("{}.{en}", owner.name_ru),
        format!("{}.{ru}", owner.name_en),
    ];
    Ok((t, extra))
}

pub(super) fn build(b: &mut Builder<'_>) -> Result<Vec<Common>, SchemaError> {
    let declared = declare(b)?;
    let commons = commons(b);
    for d in &declared {
        fill(b, d, &declared, &commons)?;
    }
    Ok(commons)
}

fn declare<'a>(b: &mut Builder<'a>) -> Result<Vec<Declared<'a>>, SchemaError> {
    let cfg = b.cfg;
    let rp_names = names::route_point_ref_names(b.cat);
    let mut out = Vec::new();
    for o in cfg.objects.iter().filter(|o| REF_KINDS.contains(&o.kind)) {
        let Some(kn) = names::kind_names(b.cat, o.kind) else {
            b.diag(format!(
                "{}.{}: no query table template for the kind",
                o.kind.as_str(),
                o.name
            ));
            continue;
        };
        let type_uuid = o.type_id("Ref").unwrap_or_else(|| {
            b.diag(format!("{}.{}: no Ref type", o.kind.as_str(), o.name));
            Guid::NIL
        });
        let table = QTableId(
            u32::try_from(b.tables.len())
                .map_err(|_| SchemaError::Config("too many tables".into()))?,
        );
        let xml_name = format!("{}.{}", kn.ref_en, o.name);
        let ru_name = format!("{}.{}", kn.ref_ru, o.name);
        let info = RefInfo {
            object: o.id,
            kind: o.kind,
            name: o.name.clone(),
            type_uuid,
            xml_name: xml_name.clone(),
            table,
        };
        let reference = b.refs.add(info, &[xml_name, ru_name]);
        if o.kind == MdKind::BusinessProcess {
            let rp_uuid = o.type_id("RoutePointRef").unwrap_or_else(|| {
                b.diag(format!("BusinessProcess.{}: no RoutePointRef type", o.name));
                Guid::NIL
            });
            b.refs.rp_uuid.insert(reference, rp_uuid);
            if !rp_uuid.is_nil() {
                b.refs.rp_by_type.insert(rp_uuid, reference);
            }
            let (ru, en) = rp_names
                .clone()
                .unwrap_or_else(|| (String::new(), "BusinessProcessRoutePointRef".into()));
            for n in [en, ru].iter().filter(|n| !n.is_empty()) {
                b.refs
                    .rp_by_qname
                    .insert(fold_name(&format!("{n}.{}", o.name)), reference);
            }
        }
        let t = QTable {
            name_ru: format!("{}.{}", kn.ru, o.name),
            name_en: format!("{}.{}", kn.en, o.name),
            kind: TableKind::Object(o.kind),
            object: Some(o.id),
            owner: None,
            fields: Vec::new(),
            star: Vec::new(),
            params: Vec::new(),
            reference: Some(reference),
        };
        let got = b.add_table(t, &[])?;
        b.add_object(o.kind, &o.name, o.id, got);
        out.push(Declared {
            obj: o,
            table: got,
            reference,
        });
    }
    Ok(out)
}

fn commons(b: &mut Builder<'_>) -> Vec<Common> {
    let cfg = b.cfg;
    let mut out = Vec::new();
    for o in cfg
        .objects
        .iter()
        .filter(|o| o.kind == MdKind::CommonAttribute)
    {
        let owner = format!("CommonAttribute.{}", o.name);
        let ty = match o.prop("Type") {
            Some(Value::Type(td)) => b.map(td, &owner),
            _ => {
                b.diag(format!("{owner}: no Type"));
                QType::default()
            }
        };
        let mut content = Vec::new();
        if let Some(Value::Xml(e)) = o.prop("Content") {
            for item in e.children.iter().filter(|c| c.name == "xr:Item") {
                let leaf = |n: &str| {
                    item.children
                        .iter()
                        .find(|c| c.name == n)
                        .and_then(|c| c.text.clone())
                        .unwrap_or_default()
                };
                content.push((leaf("xr:Metadata"), leaf("xr:Use")));
            }
        }
        out.push(Common {
            id: o.id,
            name: o.name.clone(),
            ty,
            auto_use: text_prop(o, "AutoUse") == Some("Use"),
            separator: text_prop(o, "DataSeparation") == Some("Separate"),
            content,
        });
    }
    out
}

impl Builder<'_> {
    pub(super) fn map(&mut self, td: &TypeDescription, owner: &str) -> QType {
        let td = self.cfg.expand_type(td);
        map_type(
            &self.refs,
            &self.sys_enums,
            &td,
            owner,
            &mut self.diagnostics,
        )
    }

    pub(super) fn child_type(&mut self, c: &Child, owner: &str) -> QType {
        let owner = format!("{owner}.{}", c.name);
        match c.type_desc() {
            Some(td) => self.map(td, &owner),
            None => {
                self.diag(format!("{owner}: no Type"));
                QType::default()
            }
        }
    }

    pub(super) fn reference_of(&self, t: QTableId) -> Option<RefAtom> {
        self.table_ref(t)?.reference
    }

    pub(super) fn resolve(&self, r: &MdRef) -> Option<QTableId> {
        r.id.and_then(|id| self.by_object.get(&id).copied())
            .or_else(|| self.by_full.get(&fold_name(&r.name)).copied())
    }

    pub(super) fn refs_of(&mut self, list: &[MdRef], owner: &str) -> Vec<RefAtom> {
        let mut out = Vec::new();
        for r in list {
            match self.resolve(r).and_then(|t| self.reference_of(t)) {
                Some(a) => out.push(a),
                None => self.diag(format!("{owner}: unknown object {}", r.name)),
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

pub(super) fn refs_type(refs: Vec<RefAtom>) -> QType {
    QType {
        refs,
        ..QType::default()
    }
}

fn fill(
    b: &mut Builder<'_>,
    d: &Declared<'_>,
    all: &[Declared<'_>],
    commons: &[Common],
) -> Result<(), SchemaError> {
    let o = d.obj;
    let kind = o.kind;
    let owner = format!("{}.{}", kind.as_str(), o.name);
    let mut fs = Fields::new(kind);
    let r = QType::reference(d.reference);
    fs.std(StdRole::Ref, r.clone());
    if kind == MdKind::Enum {
        fs.std(StdRole::Order, number(0, 0, false));
        fs.virt(StdRole::Presentation, string(0, false));
        let values = o.enum_values().map(|c| (fold_name(&c.name), c.id)).fold(
            HashMap::new(),
            |mut m, (k, v)| {
                m.entry(k).or_insert(v);
                m
            },
        );
        b.values.enums.insert(d.table, values);
        return finish_table(b, d.table, fs);
    }
    fs.std(StdRole::DataVersion, string(DATA_VERSION_LEN, true));
    fs.std(StdRole::DeletionMark, QType::boolean());
    let mut folders = false;
    match kind {
        MdKind::Catalog | MdKind::ChartOfCharacteristicTypes => {
            if let Some(Value::RefList(list)) = o.prop("Owners")
                && !list.is_empty()
            {
                let owners = b.refs_of(list, &format!("{owner}.Owners"));
                fs.std(StdRole::Owner, refs_type(owners));
            }
            let hierarchical = bool_prop(o, "Hierarchical");
            if hierarchical {
                fs.std(StdRole::Parent, r.clone());
            }
            folders = hierarchical
                && (kind == MdKind::ChartOfCharacteristicTypes
                    || text_prop(o, "HierarchyType") != Some("HierarchyOfItems"));
            if folders {
                fs.std(StdRole::IsFolder, QType::boolean());
            }
            code_and_description(&mut fs, o);
            if kind == MdKind::ChartOfCharacteristicTypes {
                fs.std(StdRole::ValueType, QType::other(Other::TypeDescription));
            }
        }
        MdKind::ChartOfAccounts => {
            fs.std(StdRole::Parent, r.clone());
            code_and_description(&mut fs, o);
            let order = int_prop(o, "OrderLength");
            if order > 0 {
                fs.std(StdRole::Order, string(order, false));
            }
            let account_type = b.sys.account_type.map(Other::SysEnum);
            fs.std(
                StdRole::Type,
                account_type.map_or_else(QType::default, QType::other),
            );
            fs.std(StdRole::OffBalance, QType::boolean());
            children(b, &mut fs, o, ChildTag::AccountingFlag, false, &owner);
        }
        MdKind::ChartOfCalculationTypes => {
            code_and_description(&mut fs, o);
            let basic = match text_prop(o, "DependenceOnCalculationTypes") {
                Some(dep) => dep == "OnActionPeriod",
                None => {
                    o.type_id("BaseCalculationTypes").is_some() && bool_prop(o, "ActionPeriodUse")
                }
            };
            if basic {
                fs.std(StdRole::ActionPeriodIsBasic, QType::boolean());
            }
        }
        MdKind::ExchangePlan => {
            code_and_description(&mut fs, o);
            fs.std(StdRole::SentNo, number(MESSAGE_NO_DIGITS, 0, true));
            fs.std(StdRole::ReceivedNo, number(MESSAGE_NO_DIGITS, 0, true));
            fs.std(StdRole::ExchangeDate, QType::date(DateQ::DateTime));
            fs.std(StdRole::ThisNode, QType::boolean());
        }
        MdKind::Document => {
            number_and_date(&mut fs, o);
            fs.std(StdRole::Posted, QType::boolean());
        }
        MdKind::BusinessProcess => {
            number_and_date(&mut fs, o);
            fs.std(StdRole::Completed, QType::boolean());
            fs.std(StdRole::HeadTask, refs_type(b.refs.of_kind(MdKind::Task)));
            fs.std(StdRole::Started, QType::boolean());
        }
        MdKind::Task => {
            number_and_date(&mut fs, o);
            let bps = task_processes(b, d, all);
            let points: Vec<Other> = bps.iter().map(|a| Other::RoutePoint(*a)).collect();
            fs.std(StdRole::BusinessProcess, refs_type(bps));
            fs.std(
                StdRole::RoutePoint,
                QType {
                    others: points,
                    ..QType::default()
                },
            );
            if let Some(t) = description(o) {
                fs.std(StdRole::Description, t);
            }
            fs.std(StdRole::Executed, QType::boolean());
        }
        _ => {}
    }
    children(b, &mut fs, o, ChildTag::Attribute, folders, &owner);
    if kind == MdKind::Task {
        children(b, &mut fs, o, ChildTag::AddressingAttribute, false, &owner);
    }
    for c in commons.iter().filter(|c| c.used_by(o)) {
        fs.push(common_field(c), true);
    }
    tabular_sections(b, &mut fs, d, &owner)?;
    match kind {
        MdKind::ChartOfAccounts => ext_dimension_types(b, &mut fs, d, &owner)?,
        MdKind::ChartOfCalculationTypes => calculation_type_lists(b, &mut fs, d, &owner)?,
        MdKind::BusinessProcess => route_points(b, d)?,
        _ => {}
    }
    if matches!(
        kind,
        MdKind::Catalog
            | MdKind::ChartOfCharacteristicTypes
            | MdKind::ChartOfAccounts
            | MdKind::ChartOfCalculationTypes
    ) {
        fs.std(StdRole::Predefined, QType::boolean());
        fs.std(
            StdRole::PredefinedDataName,
            string(PREDEFINED_NAME_LEN, false),
        );
        let predefined = o
            .predefined()
            .map(|p| {
                p.iter().fold(HashMap::new(), |mut m, it| {
                    m.entry(fold_name(&it.name))
                        .or_insert_with(|| it.name.clone());
                    m
                })
            })
            .unwrap_or_default();
        b.values.predefined.insert(d.table, predefined);
    }
    if kind == MdKind::Document {
        fs.virt(StdRole::PointInTime, QType::other(Other::PointInTime));
    }
    fs.virt(StdRole::Presentation, string(0, false));
    finish_table(b, d.table, fs)
}

fn finish_table(b: &mut Builder<'_>, t: QTableId, fs: Fields) -> Result<(), SchemaError> {
    let table = b
        .table_mut(t)
        .ok_or_else(|| SchemaError::Config("internal: table id out of range".into()))?;
    fs.into_table(table);
    Ok(())
}

fn description(o: &MdObject) -> Option<QType> {
    let l = int_prop(o, "DescriptionLength");
    (l > 0).then(|| string(l, false))
}

fn code_and_description(fs: &mut Fields, o: &MdObject) {
    if let Some(t) = code_type(o, "CodeLength", "CodeType", "CodeAllowedLength") {
        fs.std(StdRole::Code, t);
    }
    if let Some(t) = description(o) {
        fs.std(StdRole::Description, t);
    }
}

fn number_and_date(fs: &mut Fields, o: &MdObject) {
    if let Some(t) = code_type(o, "NumberLength", "NumberType", "NumberAllowedLength") {
        fs.std(StdRole::Number, t);
    }
    fs.std(StdRole::Date, QType::date(DateQ::DateTime));
}

fn only_of(c: &Child, folders: bool) -> Only {
    if !folders {
        return Only::Any;
    }
    match c.prop("Use") {
        Some(Value::Text(u)) if u == "ForFolder" => Only::Folders,
        Some(Value::Text(u)) if u == "ForFolderAndItem" => Only::Any,
        _ => Only::Items,
    }
}

fn children(
    b: &mut Builder<'_>,
    fs: &mut Fields,
    o: &MdObject,
    tag: ChildTag,
    folders: bool,
    owner: &str,
) {
    for c in o.children.iter().filter(|c| c.tag == tag) {
        let ty = b.child_type(c, owner);
        fs.push(attribute(c, ty, only_of(c, folders)), true);
    }
}

fn attribute(c: &Child, ty: QType, only: Only) -> QField {
    QField {
        name_ru: c.name.clone(),
        name_en: c.name.clone(),
        ty,
        role: FieldRole::Attribute,
        only,
        nested: None,
        md: Some(c.id),
        period: None,
        side: None,
    }
}

pub(super) fn common_field(c: &Common) -> QField {
    QField {
        name_ru: c.name.clone(),
        name_en: c.name.clone(),
        ty: c.ty.clone(),
        role: FieldRole::Common,
        only: Only::Any,
        nested: None,
        md: Some(c.id),
        period: None,
        side: None,
    }
}

fn nested(
    b: &mut Builder<'_>,
    fs: &mut Fields,
    d: &Declared<'_>,
    suffix: (String, String),
    object: Option<Guid>,
    role: FieldRole,
    rest: Vec<QField>,
) -> Result<QTableId, SchemaError> {
    let (mut t, extra) = sub_table(b, d.table, &suffix, TableKind::Tabular, object)?;
    let mut sub = Fields::new(d.obj.kind);
    sub.std(StdRole::Ref, QType::reference(d.reference));
    sub.std(StdRole::LineNumber, number(LINE_NUMBER_DIGITS, 0, true));
    for f in rest {
        sub.push(f, true);
    }
    sub.into_table(&mut t);
    let id = b.add_table(t, &extra)?;
    let (name_ru, name_en) = suffix;
    fs.push(
        QField {
            name_ru,
            name_en,
            ty: QType::other(Other::QueryResult),
            role,
            only: Only::Any,
            nested: Some(id),
            md: object,
            period: None,
            side: None,
        },
        true,
    );
    Ok(id)
}

fn tabular_sections(
    b: &mut Builder<'_>,
    fs: &mut Fields,
    d: &Declared<'_>,
    owner: &str,
) -> Result<(), SchemaError> {
    for ts in d.obj.tabular_sections() {
        let ts_owner = format!("{owner}.{}", ts.name);
        let mut rest = Vec::new();
        for c in ts.children.iter().filter(|c| c.tag == ChildTag::Attribute) {
            let ty = b.child_type(c, &ts_owner);
            rest.push(attribute(c, ty, Only::Any));
        }
        let suffix = (ts.name.clone(), ts.name.clone());
        nested(b, fs, d, suffix, Some(ts.id), FieldRole::Tabular, rest)?;
    }
    Ok(())
}

fn ext_dimension_types(
    b: &mut Builder<'_>,
    fs: &mut Fields,
    d: &Declared<'_>,
    owner: &str,
) -> Result<(), SchemaError> {
    let o = d.obj;
    let Some(Value::Ref(chart)) = o.prop("ExtDimensionTypes") else {
        return Ok(());
    };
    if chart.name.is_empty() || int_prop(o, "MaxExtDimensionCount") == 0 {
        return Ok(());
    }
    let kinds = b.refs_of(
        std::slice::from_ref(chart),
        &format!("{owner}.ExtDimensionTypes"),
    );
    let mut rest = vec![
        std_field(StdRole::ExtDimensionType(0), o.kind, refs_type(kinds)),
        std_field(StdRole::Predefined, o.kind, QType::boolean()),
        std_field(StdRole::TurnoversOnly, o.kind, QType::boolean()),
    ];
    for c in o
        .children
        .iter()
        .filter(|c| c.tag == ChildTag::ExtDimensionAccountingFlag)
    {
        let ty = b.child_type(c, owner);
        rest.push(attribute(c, ty, Only::Any));
    }
    let role = StdRole::ExtDimensionTypes;
    nested(
        b,
        fs,
        d,
        names::std_name(role, o.kind),
        None,
        FieldRole::Std(role),
        rest,
    )
    .map(|_| ())
}

fn calculation_type_lists(
    b: &mut Builder<'_>,
    fs: &mut Fields,
    d: &Declared<'_>,
    owner: &str,
) -> Result<(), SchemaError> {
    let o = d.obj;
    let base_charts = match o.prop("BaseCalculationTypes") {
        Some(Value::RefList(list)) if !list.is_empty() => {
            b.refs_of(list, &format!("{owner}.BaseCalculationTypes"))
        }
        _ => vec![d.reference],
    };
    let lists = [
        (StdRole::LeadingCalculationTypes, base_charts.clone()),
        (StdRole::BaseCalculationTypes, base_charts),
        (StdRole::DisplacingCalculationTypes, vec![d.reference]),
    ];
    let dep = text_prop(o, "DependenceOnCalculationTypes").unwrap_or("DontUse") != "DontUse";
    let apu = bool_prop(o, "ActionPeriodUse");
    for (role, kinds) in lists {
        if o.type_id(&names::std_name(role, o.kind).1).is_none() {
            continue;
        }
        let stored = match role {
            StdRole::BaseCalculationTypes => dep,
            StdRole::DisplacingCalculationTypes => apu,
            _ => true,
        };
        let rest = vec![
            std_field(StdRole::CalculationType, o.kind, refs_type(kinds)),
            std_field(StdRole::Predefined, o.kind, QType::boolean()),
        ];
        let id = nested(
            b,
            fs,
            d,
            names::std_name(role, o.kind),
            None,
            FieldRole::Std(role),
            rest,
        )?;
        if !stored {
            b.values.unstored.insert(id);
        }
    }
    Ok(())
}

fn route_points(b: &mut Builder<'_>, d: &Declared<'_>) -> Result<(), SchemaError> {
    let suffix = names::template_suffix(b.cat, MdKind::BusinessProcess, "Points")
        .unwrap_or_else(|| ("Точки".to_string(), "Points".to_string()));
    let kind = TableKind::Virtual(VtKind::BusinessProcessPoints);
    let (mut t, extra) = sub_table(b, d.table, &suffix, kind, None)?;
    let mut fs = Fields::new(MdKind::BusinessProcess);
    fs.std(StdRole::Ref, QType::other(Other::RoutePoint(d.reference)));
    fs.std(StdRole::Order, number(0, 0, false));
    fs.into_table(&mut t);
    let id = b.add_table(t, &extra)?;
    b.values.points.insert(d.table, id);
    Ok(())
}

fn task_processes(b: &Builder<'_>, d: &Declared<'_>, all: &[Declared<'_>]) -> Vec<RefAtom> {
    let mut out: Vec<RefAtom> = all
        .iter()
        .filter(|p| p.obj.kind == MdKind::BusinessProcess)
        .filter(|p| match p.obj.prop("Task") {
            Some(Value::Ref(r)) => b.resolve(r) == Some(d.table),
            _ => false,
        })
        .map(|p| p.reference)
        .collect();
    if out.is_empty() {
        out = b.refs.of_kind(MdKind::BusinessProcess);
    }
    out
}
