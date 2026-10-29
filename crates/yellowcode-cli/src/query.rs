use std::{
    io::{Read, Write},
    path::PathBuf,
    time::Duration,
};
use yellowcode_core::service::{
    memory,
    query::{self, Format, Request, Source},
};

#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("source").required(true).args(["text", "file", "stdin"])))]
#[command(after_help = "\
Значения параметров записываются как в языке запросов: числа 42 и 12.5, строки \"текст\" в кавычках,
даты '2024-01-15T00:00:00', Истина/Ложь, NULL, Неопределено, ссылки СправочникСсылка.Товары(<UUID>).

Примеры:
  # Простой запрос, результат таблицей в терминале
  yellowcode query base --text \"ВЫБРАТЬ ПЕРВЫЕ 10 Наименование, ИНН ИЗ Справочник.Контрагенты\"

  # Параметр-число, результат в JSON
  yellowcode query base --text \"ВЫБРАТЬ &Число * 2 КАК Ответ\" --param Число=21 --format json

  # Параметр-дата
  yellowcode query base --text \"ВЫБРАТЬ Номер, Дата ИЗ Документ.Счет ГДЕ Дата >= &Начало\" --param \"Начало='2024-01-01T00:00:00'\"

  # Запрос из файла, параметры из JSON-файла, результат в CSV
  # params.json: [{\"name\": \"Имя\", \"repr\": \"\\\"ООО Ромашка\\\"\"}]
  yellowcode query base --file query.txt --params-file params.json --format csv > result.csv

  # Запрос из стандартного ввода
  echo \"ВЫБРАТЬ 1 КАК Один\" | yellowcode query base --stdin

  # Не больше 100 строк, до 5 минут, 2 ГиБ памяти (конфигурации уровня УНФ)
  yellowcode query base --file query.txt --max-rows 100 --timeout 300 --memory-limit 2GiB")]
pub(crate) struct Args {
    #[command(flatten)]
    memory: memory::Settings,
    #[arg(
        help = "Каталог информационной базы или её файл 1Cv8.1CD (открывается только для чтения)"
    )]
    base: PathBuf,
    #[arg(long, help = "Текст запроса")]
    text: Option<String>,
    #[arg(
        long,
        help = "Файл с текстом запроса в UTF-8 (BOM в начале допускается)"
    )]
    file: Option<PathBuf>,
    #[arg(long, help = "Прочитать текст запроса из стандартного ввода")]
    stdin: bool,
    #[arg(
        long = "param",
        value_name = "NAME=REPR",
        help = "Параметр запроса: Имя=Значение; повторить для каждого параметра"
    )]
    params: Vec<String>,
    #[arg(
        long,
        help = "JSON-файл (UTF-8) с массивом параметров вида {\"name\": \"P\", \"repr\": \"42\"}"
    )]
    params_file: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        help = "Формат вывода; по умолчанию таблица в терминале и JSON при перенаправлении"
    )]
    format: Option<Format>,
    #[arg(
        long,
        default_value = query::DEFAULT_TIMEOUT,
        value_parser = query::timeout,
        help = "Предел времени выполнения запроса в секундах; открытие базы в него не входит"
    )]
    timeout: Duration,
    #[arg(
        long,
        default_value = query::DEFAULT_MAX_ROWS,
        value_parser = query::row_limit,
        help = "Сколько строк каждого результата выводить; пределы памяти и строк движка действуют независимо"
    )]
    max_rows: usize,
}

impl From<Args> for Request {
    fn from(a: Args) -> Request {
        Request {
            memory: a.memory,
            base: a.base,
            source: match (a.text, a.file, a.stdin) {
                (Some(text), _, _) => Source::Text(text),
                (None, Some(file), _) => Source::File(file),
                (None, None, _) => Source::Stdin,
            },
            params: a.params,
            params_file: a.params_file,
            format: a.format,
            timeout: a.timeout,
            max_rows: a.max_rows,
        }
    }
}

pub(crate) fn run(
    args: Args,
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
    tty: bool,
) -> yellowcode_core::Result<i32> {
    let request = Request::from(args);
    query::run(request, input, out, err, tty)
}
