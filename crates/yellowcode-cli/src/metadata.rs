use std::{io::Write, path::PathBuf};
use yellowcode_core::plain::metadata::{self, Format};

pub(crate) const EXAMPLES: &str = "\
Пути к объектам — русские или английские имена через точку: Справочник.Номенклатура
или Catalog.Номенклатура, реквизит — Справочник.Номенклатура.Реквизит.Артикул.

Примеры:
  # Всё дерево метаданных в JSON
  yellowcode metadata tree configuration.cf > tree.json

  # Справочники и документы, два уровня, текстом
  yellowcode metadata tree configuration.cf --kind Справочник,Документ --depth 2 --format text

  # Один справочник с реквизитами и табличными частями
  yellowcode metadata tree base --path Справочник.Контрагенты --depth 2 --format text

  # Плоский список узлов с parentId и depth
  yellowcode metadata tree backup.dt --flat

  # Свойства реквизита
  yellowcode metadata show configuration.cf Справочник.Контрагенты.Реквизит.ИНН --format text";

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    #[command(about = "Дерево метаданных, целиком или от одного объекта", after_help = EXAMPLES)]
    Tree {
        #[arg(help = "Конфигурация: CF, DT, файл 1Cv8.1CD или каталог базы")]
        source: PathBuf,
        #[arg(
            long,
            allow_hyphen_values = true,
            help = "Бюджет памяти на чтение конфигурации из DT; память самой модели расходуется сверх него"
        )]
        memory_limit: Option<String>,
        #[arg(
            long,
            help = "Глубина от выбранного корня: 0 — только корень, 1 — с дочерними; без параметра — вся ветка"
        )]
        depth: Option<usize>,
        #[arg(
            long,
            help = "Объект, от которого строить дерево, например Справочник.Номенклатура"
        )]
        path: Option<String>,
        #[arg(
            long,
            help = "Только объекты этих видов через запятую, например Справочник,Документ"
        )]
        kind: Option<String>,
        #[arg(
            long,
            help = "JSON плоским списком узлов (с parentId и depth) вместо вложенного дерева"
        )]
        flat: bool,
        #[arg(long, value_enum, default_value = "json", help = "Формат вывода")]
        format: Format,
    },
    #[command(about = "Свойства объекта метаданных", after_help = EXAMPLES)]
    Show {
        #[arg(help = "Конфигурация: CF, DT, файл 1Cv8.1CD или каталог базы")]
        source: PathBuf,
        #[arg(
            long,
            allow_hyphen_values = true,
            help = "Бюджет памяти на чтение конфигурации из DT; память самой модели расходуется сверх него"
        )]
        memory_limit: Option<String>,
        #[arg(help = "Путь к объекту, например Справочник.Контрагенты.Реквизит.ИНН")]
        object: String,
        #[arg(long, value_enum, default_value = "json", help = "Формат вывода")]
        format: Format,
    },
}

impl From<Command> for metadata::Command {
    fn from(c: Command) -> metadata::Command {
        match c {
            Command::Tree {
                memory_limit: _,
                source,
                depth,
                path,
                kind,
                flat,
                format,
            } => metadata::Command::Tree {
                source,
                depth,
                path,
                kind,
                flat,
                format,
            },
            Command::Show {
                memory_limit: _,
                source,
                object,
                format,
            } => metadata::Command::Show {
                source,
                object,
                format,
            },
        }
    }
}

pub(crate) fn run(command: Command, out: &mut dyn Write) -> yellowcode_core::Result<()> {
    let limit = match &command {
        Command::Tree { memory_limit, .. } | Command::Show { memory_limit, .. } => {
            memory_limit.clone()
        }
    };
    metadata::run_with_memory_limit(command.into(), limit, out)
}
