use crate::value::RefId;
use brace::Guid;
use ibschema::values::RefId as StoredRefId;

pub fn ref_id(uuid: Guid) -> RefId {
    StoredRefId::from_uuid(uuid).0
}

pub fn uuid_of(id: &RefId) -> Guid {
    StoredRefId(*id).to_uuid()
}
