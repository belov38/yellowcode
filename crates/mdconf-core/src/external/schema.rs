use md_model::external::ExternalKind;

pub const HEADER: &[usize] = &[1, 3, 1];
pub const TYPE_ID: &[usize] = &[1, 1];
pub const VALUE_ID: &[usize] = &[1, 2];
pub const PROCESSOR_PROPERTIES: &[(&str, usize)] = &[("DefaultForm", 4), ("AuxiliaryForm", 6)];
pub const REPORT_PROPERTIES: &[(&str, usize)] = &[
    ("DefaultForm", 4),
    ("MainDataCompositionSchema", 5),
    ("DefaultSettingsForm", 6),
    ("VariantsStorage", 7),
    ("SettingsStorage", 8),
    ("DefaultVariantForm", 9),
    ("AuxiliaryForm", 11),
    ("AuxiliarySettingsForm", 12),
    ("AuxiliaryVariantForm", 13),
];
pub fn properties(kind: ExternalKind) -> &'static [(&'static str, usize)] {
    match kind {
        ExternalKind::DataProcessor => PROCESSOR_PROPERTIES,
        ExternalKind::Report => REPORT_PROPERTIES,
    }
}
pub fn body_version(kind: ExternalKind) -> u32 {
    match kind {
        ExternalKind::DataProcessor => 4,
        ExternalKind::Report => 5,
    }
}
