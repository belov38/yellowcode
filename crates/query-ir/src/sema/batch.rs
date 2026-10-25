use super::{AnalyzeMode, AnalyzeOptions, CompiledStmt, TempOut, analyze_here};
use crate::CompileError;
use crate::params::{NoParams, ParamInfo};
use crate::schema::QuerySchema;
use crate::temp::{TempMap, TempSchema, TempSchemas};
use crate::types::{Other, QType};
use bsl_prim::fold_name;
use query_syntax::{ast, with_deep_stack};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

struct Overlay<'a> {
    base: &'a dyn TempSchemas,
    added: TempMap,
    dropped: HashSet<String>,
}

impl TempSchemas for Overlay<'_> {
    fn get(&self, name: &str) -> Option<Arc<TempSchema>> {
        if let Some(t) = self.added.get(name) {
            return Some(t);
        }
        if self.dropped.contains(&fold_name(name)) {
            return None;
        }
        self.base.get(name)
    }
}

impl Overlay<'_> {
    fn apply(&mut self, st: &CompiledStmt) {
        if let Some(TempOut::Put(t)) = &st.temp_out {
            self.dropped.remove(&fold_name(&t.name));
            self.added.insert(t.clone());
        }
        if let Some(d) = &st.drop {
            self.added.remove(d);
            self.dropped.insert(fold_name(d));
        }
    }
}

pub fn analyze_batch_static(
    batch: &ast::Batch,
    text: &str,
    schema: &QuerySchema,
    temp: &dyn TempSchemas,
) -> Result<Vec<Arc<CompiledStmt>>, CompileError> {
    with_deep_stack(batch.max_depth, || {
        crate::compile::reject_unresolved(batch, text)?;
        let mut overlay = Overlay {
            base: temp,
            added: TempMap::new(),
            dropped: HashSet::new(),
        };
        let mut out = Vec::with_capacity(batch.stmts.len());
        for i in 0..batch.stmts.len() {
            let opts = AnalyzeOptions {
                temp_tables: &overlay,
                params: &NoParams,
                mode: AnalyzeMode::Static,
                inter: false,
            };
            let st = analyze_here(batch, text, i, schema, opts)?;
            overlay.apply(&st);
            out.push(st);
        }
        Ok(out)
    })
}

pub fn find_parameters(
    batch: &ast::Batch,
    text: &str,
    schema: &QuerySchema,
    temp: &dyn TempSchemas,
) -> Result<Vec<ParamInfo>, CompileError> {
    let stmts = analyze_batch_static(batch, text, schema, temp)?;
    let mut by_name: HashMap<String, ParamInfo> = HashMap::new();
    let vt_type = QType::other(Other::ValueTable);
    for name in stmts.iter().flat_map(|s| s.table_params.iter()) {
        by_name.insert(
            fold_name(name),
            ParamInfo {
                name: name.clone(),
                ty: vt_type.clone(),
            },
        );
    }
    for p in stmts.iter().flat_map(|s| s.params.iter()) {
        if by_name
            .get(&fold_name(&p.name))
            .is_some_and(|k| k.ty == vt_type)
        {
            continue;
        }
        match by_name.get_mut(&fold_name(&p.name)) {
            Some(known) => known.ty = known.ty.merge(&p.ty),
            None => {
                by_name.insert(fold_name(&p.name), p.clone());
            }
        }
    }
    let mut out: Vec<(String, ParamInfo)> = by_name.into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out.into_iter().map(|(_, p)| p).collect())
}
