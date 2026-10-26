use bsl_vm::{CallResult, Execution, Value};
pub(crate) fn new(
    ex: &mut Execution,
    host: &dyn crate::QueryHost,
    params: &[query_ir::ParamInfo],
) -> CallResult<Value> {
    let mut items = Vec::new();
    for p in params {
        items.push(crate::columns::record(
            "QueryParameterDescription",
            vec![
                ("Name", Value::from(p.name.as_str())),
                ("ValueType", crate::columns::description(ex, host, &p.ty)?),
            ],
        )?);
    }
    crate::columns::collection(
        "QueryParametersDescription",
        params.iter().map(|p| p.name.clone()).collect(),
        items,
    )
}
