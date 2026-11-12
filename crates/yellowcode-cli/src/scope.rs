use yellowcode_core::plain::scope::ScopeArgs;

#[derive(clap::Args, Default)]
pub(crate) struct ScopeFlags {
    #[arg(
        long,
        value_delimiter = ',',
        help = "Только объекты этих видов через запятую или повтором флага, например Справочник,Документ"
    )]
    pub(crate) kind: Vec<String>,
    #[arg(
        long,
        help = "Объект и всё под ним, например Справочник.Номенклатура; флаг можно повторять"
    )]
    pub(crate) path: Vec<String>,
    #[arg(
        long,
        help = "Только объекты подсистемы, полный (Подсистема.А.Подсистема.Б) или краткий (А.Б) путь; флаг можно повторять"
    )]
    pub(crate) subsystem: Vec<String>,
}

impl From<ScopeFlags> for ScopeArgs {
    fn from(flags: ScopeFlags) -> ScopeArgs {
        ScopeArgs {
            kinds: flags.kind,
            paths: flags.path,
            subsystems: flags.subsystem,
        }
    }
}
