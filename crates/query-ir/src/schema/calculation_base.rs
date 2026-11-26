use super::{Builder, FieldRole, Item, QFieldId, QTableId, QuerySchema, StdRole};
use brace::Guid;
use md_model::{MdObject, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BaseDependence {
    OnActionPeriod,
    OnRegistrationPeriod,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BaseFacts {
    pub owner: QTableId,
    pub base: QTableId,
    pub chart: Option<QTableId>,
    pub base_section: Option<QTableId>,
    pub dependence: Option<BaseDependence>,
    pub owner_action_period: bool,
    pub owner_base_period: bool,
    pub base_action_period: bool,
    pub owner_periodicity_month: bool,
    pub base_periodicity_month: bool,
    pub resources: Vec<(QFieldId, QFieldId, Guid)>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn register(
    b: &mut Builder<'_>,
    vt: QTableId,
    owner: QTableId,
    base: QTableId,
    owner_obj: &MdObject,
    base_obj: &MdObject,
    chart_obj: Option<&MdObject>,
    owner_action_period: bool,
    owner_base_period: bool,
    base_action_period: bool,
    base_resources: &[Item],
) {
    let chart = chart_obj.and_then(|c| b.by_object.get(&c.id).copied());
    let base_section = chart.and_then(|c| {
        b.table_ref(c).and_then(|t| {
            t.fields
                .iter()
                .find(|f| f.role == FieldRole::Std(StdRole::BaseCalculationTypes))
                .and_then(|f| f.nested)
        })
    });
    let dependence = chart_obj.and_then(|c| match c.prop("DependenceOnCalculationTypes") {
        Some(Value::Text(s)) if s == "OnActionPeriod" => Some(BaseDependence::OnActionPeriod),
        Some(Value::Text(s)) if s == "OnRegistrationPeriod" => {
            Some(BaseDependence::OnRegistrationPeriod)
        }
        None => Some(
            if matches!(c.prop("ActionPeriodUse"), Some(Value::Bool(true))) {
                BaseDependence::OnActionPeriod
            } else {
                BaseDependence::OnRegistrationPeriod
            },
        ),
        _ => None,
    });
    let owner_periodicity_month =
        matches!(owner_obj.prop("Periodicity"), Some(Value::Text(s)) if s == "Month");
    let base_periodicity_month =
        matches!(base_obj.prop("Periodicity"), Some(Value::Text(s)) if s == "Month");
    let mut resources = Vec::with_capacity(base_resources.len());
    for res in base_resources {
        let vt_field = b.table_ref(vt).and_then(|t| {
            t.fields
                .iter()
                .enumerate()
                .find(|(_, f)| f.role == FieldRole::Virtual && f.md == Some(res.id))
                .and_then(|(i, _)| u16::try_from(i).ok().map(QFieldId))
        });
        let base_field = b.table_ref(base).and_then(|t| {
            t.fields
                .iter()
                .enumerate()
                .find(|(_, f)| f.role == FieldRole::Resource && f.md == Some(res.id))
                .and_then(|(i, _)| u16::try_from(i).ok().map(QFieldId))
        });
        if let (Some(vf), Some(bf)) = (vt_field, base_field) {
            resources.push((vf, bf, res.id));
        }
    }
    b.bases.insert(
        vt,
        BaseFacts {
            owner,
            base,
            chart,
            base_section,
            dependence,
            owner_action_period,
            owner_base_period,
            base_action_period,
            owner_periodicity_month,
            base_periodicity_month,
            resources,
        },
    );
}

impl QuerySchema {
    pub(crate) fn calculation_base(&self, table: QTableId) -> Option<&BaseFacts> {
        self.bases.get(&table)
    }

    pub(crate) fn schedule_of_register(
        &self,
        register: QTableId,
    ) -> Option<&super::schedule::Facts> {
        self.schedules.get(&register)
    }
}
