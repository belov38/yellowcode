use ibschema::values::RefId;

fn word(id: &RefId, at: usize) -> u32 {
    let mut w = [0u8; 4];
    for (i, o) in w.iter_mut().enumerate() {
        *o = id.0.get(at + i).copied().unwrap_or(0);
    }
    u32::from_le_bytes(w)
}

pub fn ref_hash(table_no: u32, id: &RefId) -> i32 {
    table_no
        .wrapping_add(word(id, 0).wrapping_mul(5))
        .wrapping_add(word(id, 12).wrapping_mul(25)) as i32
}
