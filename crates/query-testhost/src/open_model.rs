use ibschema::columns::Part;
use ibschema::storage::StorageMap;

pub const QS_FIXED: u64 = 3 << 19;
pub const PER_QS_FIELD: u64 = 896;
pub const PER_QS_FANOUT: u64 = 10;
pub const PER_BIND_TABLE: u64 = 64;
pub const PER_BIND_FIELD: u64 = 16;
pub const PER_NAMED_REF: u64 = 256;
pub const PER_NAMED_REF_BYTE: u64 = 4;
pub const PER_REF_TYPE: u64 = 128;
pub const INDEX_SCAN: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostInputs {
    pub tables: u64,
    pub fields: u64,
    pub composite_refs: u64,
    pub ref_types: u64,
    pub named_refs: u64,
    pub named_ref_bytes: u64,
}

impl HostInputs {
    pub fn of(map: &StorageMap, cfg: &md_model::Configuration) -> HostInputs {
        let mut inputs = HostInputs {
            tables: u64::try_from(map.tables().len()).unwrap_or(u64::MAX),
            ..HostInputs::default()
        };
        for (i, t) in map.tables().iter().enumerate() {
            inputs.fields = inputs.fields.saturating_add(t.fields.len() as u64);
            inputs.composite_refs = inputs.composite_refs.saturating_add(
                t.fields
                    .iter()
                    .filter(|f| {
                        t.columns
                            .get(f.columns.clone())
                            .is_some_and(|c| c.iter().any(|c| c.part == Part::TRef))
                    })
                    .count() as u64,
            );
            let id = ibschema::storage::TableId(u32::try_from(i).unwrap_or(u32::MAX));
            if map.ref_number(id).is_some() {
                inputs.ref_types = inputs.ref_types.saturating_add(1);
            }
        }
        for o in &cfg.objects {
            for v in o.enum_values() {
                inputs.named_refs = inputs.named_refs.saturating_add(1);
                inputs.named_ref_bytes = inputs.named_ref_bytes.saturating_add(v.name.len() as u64);
            }
            for it in o.predefined().into_iter().flat_map(|p| p.iter()) {
                inputs.named_refs = inputs.named_refs.saturating_add(1);
                inputs.named_ref_bytes =
                    inputs.named_ref_bytes.saturating_add(it.name.len() as u64);
            }
        }
        inputs
    }

    pub fn query_schema(&self) -> u64 {
        self.query_schema_with(self.composite_refs.saturating_mul(self.ref_types))
    }

    pub fn query_schema_with(&self, fanout: u64) -> u64 {
        QS_FIXED
            .saturating_add(PER_QS_FIELD.saturating_mul(self.fields))
            .saturating_add(PER_QS_FANOUT.saturating_mul(fanout))
    }

    pub fn fanout_of(schema: &query_ir::QuerySchema) -> u64 {
        schema
            .tables()
            .iter()
            .flat_map(|t| t.fields.iter())
            .map(|f| f.ty.refs.len() as u64)
            .sum()
    }

    pub fn binding(&self) -> u64 {
        PER_BIND_TABLE
            .saturating_mul(self.tables)
            .saturating_add(PER_BIND_FIELD.saturating_mul(self.fields))
    }

    pub fn reference_maps(&self) -> u64 {
        PER_NAMED_REF
            .saturating_mul(self.named_refs)
            .saturating_add(PER_NAMED_REF_BYTE.saturating_mul(self.named_ref_bytes))
            .saturating_add(PER_REF_TYPE.saturating_mul(self.ref_types))
    }
}
