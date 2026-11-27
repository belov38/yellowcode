use std::{io::Write, path::PathBuf};
use yellowcode_core::plain::metadata::{self, Format};

pub(crate) const EXAMPLES: &str = "\
Примеры:
  # Свойства расширения
  yellowcode ext info extension.cfe --format text

  # Объекты верхнего уровня расширения
  yellowcode ext objects extension.cfe

  # Модули расширения, затем текст одного модуля
  yellowcode ext modules extension.cfe
  yellowcode ext modules extension.cfe --entry-id <entryId> --module <moduleKind>

  # Перехваты методов
  yellowcode ext intercepts extension.cfe --format text

Справка по команде: yellowcode ext <команда> --help";

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    #[command(about = "Свойства расширения из файла CFE (только чтение)")]
    Info {
        #[arg(help = "Файл расширения CFE")]
        source: PathBuf,
        #[arg(long, value_enum, default_value = "json", help = "Формат вывода")]
        format: Format,
    },
    #[command(
        about = "Объекты верхнего уровня расширения CFE (только чтение)",
        after_help = "Ссылки выводятся в исходном порядке, включая дубликаты. Без дочерних объектов, форм и модулей. Это не полная проверка CFE."
    )]
    Objects {
        #[arg(help = "Файл расширения CFE")]
        source: PathBuf,
        #[arg(long, value_enum, default_value = "json", help = "Формат вывода")]
        format: Format,
    },
    #[command(about = "Модули объектов расширения CFE (только чтение)")]
    Modules {
        #[arg(help = "Файл расширения CFE")]
        source: PathBuf,
        #[arg(long, value_enum, default_value = "json", help = "Формат списка")]
        format: Format,
        #[arg(
            long,
            requires = "module",
            conflicts_with = "format",
            value_parser = metadata::parse_entry_id,
            help = "UUID записи объекта из поля entryId списка"
        )]
        entry_id: Option<String>,
        #[arg(
            long,
            requires = "entry_id",
            value_name = "KIND",
            help = "Вид модуля из поля moduleKind списка. Выводит исходные байты BSL"
        )]
        module: Option<String>,
    },
    #[command(about = "Перехваты методов в модулях расширения CFE (только чтение)")]
    Intercepts {
        #[arg(help = "Файл расширения CFE")]
        source: PathBuf,
        #[arg(long, value_enum, default_value = "json", help = "Формат списка")]
        format: Format,
    },
}

impl From<Command> for metadata::Command {
    fn from(c: Command) -> metadata::Command {
        match c {
            Command::Info { source, format } => metadata::Command::ExtensionInfo { source, format },
            Command::Objects { source, format } => {
                metadata::Command::ExtensionObjects { source, format }
            }
            Command::Modules {
                source,
                format,
                entry_id,
                module,
            } => metadata::Command::ExtensionModules {
                source,
                format,
                selection: entry_id.zip(module),
            },
            Command::Intercepts { source, format } => {
                metadata::Command::ExtensionIntercepts { source, format }
            }
        }
    }
}

pub(crate) fn run(command: Command, out: &mut dyn Write) -> yellowcode_core::Result<()> {
    metadata::run_with_memory_limit(command.into(), None, out)
}
