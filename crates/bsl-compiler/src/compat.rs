use bsl_prim::CompatMode;
use platform_catalog::GlobalMethod;

pub const BITWISE: [&str; 10] = [
    "ПобитовоеИ",
    "ПобитовоеИли",
    "ПобитовоеИНе",
    "ПобитовоеИсключительноеИли",
    "ПобитовоеНе",
    "ПобитовыйСдвигВлево",
    "ПобитовыйСдвигВправо",
    "ПроверитьБит",
    "ПроверитьПоБитовойМаске",
    "УстановитьБит",
];

pub(crate) fn hides(mode: CompatMode, g: &GlobalMethod) -> bool {
    mode.flags().no_bitwise_functions && BITWISE.contains(&g.info.name_ru.as_str())
}
