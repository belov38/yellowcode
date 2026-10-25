use ibschema::values::RefId;

pub fn hex32(id: &RefId) -> String {
    id.0.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn broken_ref_text(table_no: u32, id: &RefId) -> String {
    format!("<Объект не найден> ({table_no}:{})", hex32(id))
}
