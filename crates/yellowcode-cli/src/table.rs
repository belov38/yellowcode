use std::{io::Write, path::PathBuf};
use yellowcode_core::service::{
    memory,
    query::Format,
    table::{self, Bound, IndexSpec, Order, Request, RowFilter},
};

#[derive(clap::Args)]
#[command(after_help = "\
Индекс читается так: --eq задаёт значения первых полей индекса по порядку, затем на следующее поле
можно задать диапазон (--gt или --gte снизу, --lt или --lte сверху). Даты: 2024-01-31 или
2024-01-31T12:00:00. Если указать несуществующий индекс, ошибка перечислит доступные индексы и их поля.
Курсор следующей страницы выводится в stderr строкой «yellowcode: next cursor: ...».

Примеры:
  # Первые 100 строк документа в порядке хранения
  yellowcode table base Документ Счет --limit 100

  # Документы за январь 2024 по индексу даты, от новых к старым
  # (первое поле индекса ByDocDate здесь — разделитель данных, поэтому --eq 0)
  yellowcode table base Документ Счет --index ByDocDate --eq 0 --gte 2024-01-01 --lt 2024-02-01 --order desc

  # Только проведённые документы без пометки удаления
  yellowcode table base Document Счет --posted true --deletion-mark false

  # Справочник в CSV
  yellowcode table base Справочник Контрагенты --format csv > contractors.csv

  # Следующая страница: та же команда и курсор из предыдущего вывода
  yellowcode table base Документ Счет --limit 100 --cursor <курсор>")]
pub(crate) struct Args {
    #[command(flatten)]
    memory: memory::Settings,
    #[arg(
        help = "Каталог информационной базы или её файл 1Cv8.1CD (открывается только для чтения)"
    )]
    base: PathBuf,
    #[arg(help = "Вид объекта метаданных, например Документ, Справочник, AccumulationRegister")]
    kind: String,
    #[arg(help = "Имя объекта метаданных, например РеализацияТоваров")]
    name: String,
    #[arg(
        long,
        help = "Читать по этому индексу в порядке ключа; без него таблица читается в порядке хранения"
    )]
    index: Option<String>,
    #[arg(
        long,
        value_name = "VALUE",
        requires = "index",
        allow_hyphen_values = true,
        help = "Значение очередного поля индекса; повторять в порядке полей индекса"
    )]
    eq: Vec<String>,
    #[arg(
        long,
        value_name = "VALUE",
        requires = "index",
        conflicts_with = "gte",
        allow_hyphen_values = true,
        help = "Нижняя граница (не включая) поля индекса после полей из --eq"
    )]
    gt: Option<String>,
    #[arg(
        long,
        value_name = "VALUE",
        requires = "index",
        allow_hyphen_values = true,
        help = "Нижняя граница (включая) поля индекса после полей из --eq"
    )]
    gte: Option<String>,
    #[arg(
        long,
        value_name = "VALUE",
        requires = "index",
        conflicts_with = "lte",
        allow_hyphen_values = true,
        help = "Верхняя граница (не включая) поля индекса после полей из --eq"
    )]
    lt: Option<String>,
    #[arg(
        long,
        value_name = "VALUE",
        requires = "index",
        allow_hyphen_values = true,
        help = "Верхняя граница (включая) поля индекса после полей из --eq"
    )]
    lte: Option<String>,
    #[arg(
        long,
        value_enum,
        requires = "index",
        help = "Порядок ключа индекса (по умолчанию asc)"
    )]
    order: Option<Order>,
    #[arg(
        long,
        value_name = "true|false",
        value_parser = table::filter_value,
        help = "Только проведённые (true) или непроведённые (false) строки; для документов и журналов документов"
    )]
    posted: Option<bool>,
    #[arg(
        long,
        value_name = "true|false",
        value_parser = table::filter_value,
        help = "Только помеченные (true) или непомеченные (false) на удаление строки, и группы, и элементы"
    )]
    deletion_mark: Option<bool>,
    #[arg(
        long,
        default_value = table::DEFAULT_LIMIT,
        value_parser = table::page_limit,
        help = "Строк на странице"
    )]
    limit: usize,
    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Продолжить после страницы, которая вернула этот курсор; курсор действует, пока файл 1CD не менялся"
    )]
    cursor: Option<String>,
    #[arg(
        long,
        value_enum,
        help = "Формат вывода; по умолчанию таблица в терминале и JSON при перенаправлении"
    )]
    format: Option<Format>,
}

fn bound(exclusive: Option<String>, inclusive: Option<String>) -> Option<Bound> {
    match (exclusive, inclusive) {
        (Some(value), _) => Some(Bound {
            value,
            inclusive: false,
        }),
        (None, Some(value)) => Some(Bound {
            value,
            inclusive: true,
        }),
        (None, None) => None,
    }
}

impl From<Args> for Request {
    fn from(a: Args) -> Request {
        let index = a.index.map(|name| IndexSpec {
            name,
            eq: a.eq,
            lower: bound(a.gt, a.gte),
            upper: bound(a.lt, a.lte),
            order: a.order,
        });
        Request {
            memory: a.memory,
            base: a.base,
            kind: a.kind,
            name: a.name,
            index,
            limit: a.limit,
            filter: RowFilter {
                posted: a.posted,
                deletion_mark: a.deletion_mark,
            },
            cursor: a.cursor,
            format: a.format,
        }
    }
}

pub(crate) fn run(
    args: Args,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
) -> yellowcode_core::Result<i32> {
    let request = Request::from(args);
    table::run(request, out, err, tty)
}
