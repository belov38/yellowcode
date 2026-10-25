use bsl_vm::{Runtime, TypeId};
fn guid(rt: &Runtime, t: TypeId) -> Option<[u8; 16]> {
    rt.types().app_type(t).map(|a| a.guid).or_else(|| {
        rt.types()
            .catalog_type(t)
            .and_then(|i| platform_catalog::catalog().type_info(i).guid)
    })
}
pub(super) fn sort(types: &mut [TypeId], rt: &Runtime) {
    types.sort_by(|a, b| match (guid(rt, *a), guid(rt, *b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => rt
            .types()
            .name(*a, bsl_vm::Lang::Ru)
            .cmp(rt.types().name(*b, bsl_vm::Lang::Ru)),
    });
}
