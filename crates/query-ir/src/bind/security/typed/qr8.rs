use super::*;
use crate::schema::{QFieldId, VtKind};

impl Typed<'_> {
    pub(super) fn qr8_dependencies(
        &mut self,
        binding: Binding,
        table: QTableId,
    ) -> Result<(), ExecError> {
        let schema = self.collector.schema;
        match schema.table(table).kind {
            TableKind::Virtual(VtKind::TaskByPerformer) => {
                let Some(owner) = schema.table(table).owner else {
                    return Ok(());
                };
                let Some(meta) = schema.meta(owner).and_then(|m| m.addressing.as_ref()) else {
                    return Ok(());
                };
                let address = self.binding(Some(meta.register), false)?;
                for (task, dimension) in &meta.fields {
                    self.collector.field(
                        Occurrence::Direct(address.src),
                        meta.register,
                        *dimension,
                    )?;
                    let Some(original) = schema.field(owner, *task) else {
                        continue;
                    };
                    if let Some((i, _)) = schema
                        .table(table)
                        .fields
                        .iter()
                        .enumerate()
                        .find(|(_, f)| f.md == original.md && f.role == original.role)
                    {
                        let field = QFieldId(
                            u16::try_from(i)
                                .map_err(|_| ExecError::Fatal("QR8 field index".into()))?,
                        );
                        self.collector
                            .field(Occurrence::Direct(binding.src), table, field)?;
                    }
                }
            }
            TableKind::Virtual(VtKind::FilterCriterion) => {
                let Some(meta) = schema.meta(table).and_then(|m| m.criterion.as_ref()) else {
                    return Ok(());
                };
                for (member, field) in &meta.members {
                    let member_binding = self.binding(Some(*member), false)?;
                    let key = Occurrence::Direct(member_binding.src);
                    self.collector.field(key.clone(), *member, *field)?;
                    self.collector.role(key, *member, StdRole::Ref)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
