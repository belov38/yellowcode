use std::io::{Read, Write};
use std::path::PathBuf;

use yellowcode_core::output::Out;
use yellowcode_core::remote::{
    CallRequest, InfoRequest, Login, MetadataRequest, ModuleUrn, ModulesRequest, Param, Password,
    Request,
};

macro_rules! after_help {
    () => {
        "\
Пароль пользователя (--user) не передаётся в командной строке и не набирается с клавиатуры: --password-env ИМЯ \
берёт его из переменной окружения, а без этого флага пароль читается одной строкой из stdin, поданного \
конвейером или из файла. Если stdin — терминал, команда отказывает до первого запроса. Пустой пароль — пустая \
строка: `printf '\\n' | yellowcode remote …` в sh, bash, zsh; `echo.| yellowcode remote …` в cmd. Ввод, который \
кончился раньше первой строки, — ошибка.

Примеры:
  # Сводка сервера при входе без пользователя
  yellowcode remote info http://127.0.0.1:28736

  # Параметры входа базы с пользователями, без входа и без пароля
  yellowcode remote info http://server/base --auth-settings-only

  # Метаданные под пользователем, пароль из переменной окружения YC_PASSWORD
  yellowcode remote metadata http://server/base --user Администратор --password-env YC_PASSWORD

  # Пароль из файла
  yellowcode remote info http://server/base --user Администратор < password.txt

  # Пользователь без пароля (в cmd: echo.| yellowcode remote info …)
  printf '\\n' | yellowcode remote info http://server/base --user Гость

  # Вызвать экспортную функцию общего модуля с параметрами
  yellowcode remote call http://server/base --call МойМодуль.МояФункция --param n:10 --param s:текст

  # Записать тексты клиентских модулей в каталог
  yellowcode remote modules http://server/base --out client-modules"
    };
}

pub(crate) const EXAMPLES: &str = after_help!();

/// The server and the login, common to every subcommand.
#[derive(clap::Args)]
pub(crate) struct LoginArgs {
    #[arg(
        help = "Адрес сервера 1С, например http://127.0.0.1:28736 (ibsrv --http-port) или адрес веб-публикации"
    )]
    url: String,
    #[arg(
        long,
        value_name = "ПОЛЬЗОВАТЕЛЬ",
        help = "Имя пользователя информационной базы для входа с паролем; пароль берётся из --password-env или строкой из stdin (не с терминала)"
    )]
    user: Option<String>,
    #[arg(
        long,
        value_name = "ИМЯ",
        requires = "user",
        help = "Взять пароль из этой переменной окружения"
    )]
    password_env: Option<String>,
    #[arg(
        long,
        requires = "user",
        help = "Пользователь со стойким паролем: пароль передаётся как набран (по умолчанию — как у прежних версий, в верхнем регистре)"
    )]
    strong_password: bool,
}

impl LoginArgs {
    fn login(self, stdin_terminal: bool) -> Result<Login, String> {
        let password = match self.password_env {
            Some(var) => Password::Env(var),
            None if self.user.is_some() && stdin_terminal => return Err(TERMINAL.to_string()),
            None => Password::Stdin,
        };
        Ok(Login {
            url: self.url,
            user: self.user,
            password,
            strong: self.strong_password,
        })
    }
}

const TERMINAL: &str = "the password of --user is not read from a terminal: give --password-env VAR, or pipe the \
                        password into stdin";

const JSON_HELP: &str = "Вывести один объект JSON вместо текста";

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    #[command(
        about = "Сводка сервера: база, конфигурация, пользователь, сеанс, параметры входа, модули, разделы",
        after_help = concat!(
            "Войти на сервер и показать, что он сообщает клиенту: сервер, база, конфигурация, пользователь, \
сеанс, параметры входа, серверные модули, типы и разделы верхнего уровня. Модули не вызываются\n\n",
            after_help!()
        )
    )]
    Info {
        #[command(flatten)]
        login: LoginArgs,
        #[arg(
            long,
            help = "Не запрашивать экспортные методы каждого серверного модуля (modules/defs?id)"
        )]
        no_members: bool,
        #[arg(
            long,
            help = "Не запрашивать разделы командного интерфейса верхнего уровня (cmi/cmiStartData)"
        )]
        no_sections: bool,
        #[arg(
            long,
            help = "Не запрашивать параметры входа (additionalAuthSettings). По умолчанию они запрашиваются после \
about, как это делает тонкий клиент. Они вспомогательные: если запросить или прочитать их не удалось, сводка \
выводится без них, причина — в stderr (в --json — auth_settings_error), код выхода 0"
        )]
        no_auth_settings: bool,
        #[arg(
            long,
            conflicts_with_all = ["user", "password_env", "strong_password", "no_members", "no_sections", "no_auth_settings"],
            help = "Не входить: запросить и вывести только параметры входа. Сервер отдаёт их без сеанса, поэтому \
политика входа базы с пользователями (восстановление пароля, сохранение аутентификации, виды аутентификации) \
видна без пароля. Их ошибка — ошибка команды; с --json выводится строка RESULT FAIL steps=… в stdout и ошибка \
в stderr, без JSON"
        )]
        auth_settings_only: bool,
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
    #[command(
        about = "Метаданные, которые сервер отдаёт тонкому клиенту: объекты и командный интерфейс",
        after_help = concat!(
            "Войти на сервер и показать метаданные, которые он отдаёт тонкому клиенту: объекты конфигурации \
по видам (имя, представление, id типа, значения перечислений, реквизиты регистров, доступ) и командный \
интерфейс (разделы и их команды навигации), как их видит пользователь. Формы не открываются, модули не вызываются\n\n",
            after_help!()
        )
    )]
    Metadata {
        #[command(flatten)]
        login: LoginArgs,
        #[arg(
            long,
            help = "Показать и синоним каждого значения перечисления, как его запрашивает тонкий клиент: один \
запрос (types/view/<clsid>/values) на каждое перечисление со значениями, поэтому по умолчанию выключено. Синонимы \
приходят на языке сеанса, который выбирает сервер (значение без синонима на нём показывается именем). Они \
вспомогательные: если запросить их не удалось, метаданные выводятся без синонимов, причина — в stderr (в --json — \
enum_synonyms_error), код выхода 0"
        )]
        enum_synonyms: bool,
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
    #[command(
        about = "Вызвать экспортную функцию серверного общего модуля и вывести результат",
        after_help = concat!(
            "Войти на сервер, вызвать <Модуль>.<Функция> серверного общего модуля и вывести результат\n\n",
            after_help!()
        )
    )]
    Call {
        #[command(flatten)]
        login: LoginArgs,
        #[arg(
            long,
            value_name = "МОДУЛЬ.ФУНКЦИЯ",
            help = "Серверный общий модуль и его экспортная функция или процедура; при пустом модуле \
(`.Функция`) берётся первый серверный модуль из списка сервера"
        )]
        call: String,
        #[arg(
            long = "param",
            value_name = "ЗНАЧЕНИЕ",
            value_parser = Param::parse,
            help = "Параметр; для нескольких повторите флаг, по порядку. Префикс задаёт тип: n:1.5 (Число), \
b:true (Булево), d:2020-01-02T03:04:05 (Дата), u:<uuid> (УникальныйИдентификатор), s:текст (Строка), undef \
(Неопределено), null (NULL). Текст без известного префикса — строка"
        )]
        params: Vec<Param>,
        #[arg(
            long = "param-json",
            value_name = "JSON",
            value_parser = Param::parse_json,
            conflicts_with = "params",
            help = "Параметр в типизированном JSON; для нескольких повторите флаг, по порядку. Не вместе с \
--param, чтобы порядок был однозначным. \"текст\" Строка, true/false Булево, null Неопределено, {\"null\":null} \
NULL, {\"n\":\"1.5\"} Число (голое число JSON не принимается: оно может потерять цифры), \
{\"d\":\"2020-01-02T03:04:05\"} Дата, {\"u\":\"<uuid>\"} УникальныйИдентификатор, \
{\"ref\":[\"cfg:CatalogRef.Имя\",\"<uuid>\"]} ссылка, {\"enum\":[\"cfg:EnumRef.Имя\",\"Значение\"]} значение \
перечисления, […] Массив, {\"fix\":[…]} ФиксированныйМассив, {\"st\":{\"ключ\":…}} Структура, {\"fixst\":{…}} \
ФиксированнаяСтруктура, {\"map\":[[ключ,значение],…]} Соответствие, {\"fixmap\":[…]} ФиксированноеСоответствие, \
{\"vt\":{\"columns\":[…],\"indexes\":[[…]],\"rows\":[[…]]}} ТаблицаЗначений, \
{\"tree\":{\"columns\":[…],\"rows\":[{\"values\":[…],\"rows\":[…]}]}} ДеревоЗначений, \
{\"vl\":{\"type\":{…},\"items\":[{\"value\":…,\"presentation\":…,\"check\":true,\"id\":0}],\"last_id\":0}} \
СписокЗначений. Колонка — имя или {\"name\":…,\"type\":{…},\"title\":…,\"width\":…}; тип — \
{\"types\":[\"xs:string\",\"cfg:CatalogRef.Имя\"],\"number\":{\"digits\":10,\"fraction\":2,\"sign\":\"Any\"},\
\"string\":{\"length\":10,\"allowed\":\"Variable\"},\"date\":\"DateTime\"}. В этом виде значения выводит --json"
        )]
        params_json: Vec<Param>,
        #[arg(
            long,
            hide = true,
            help = "Также запросить /e1cib/types, как клиент с холодным кэшем"
        )]
        cold: bool,
        #[arg(
            long,
            help = "Вывести один объект JSON: возвращаемое значение (у процедуры его нет) и параметры в \
типизированном JSON --param-json, так что выведенное значение можно передать обратно как есть. Представления \
ссылок — отдельный ключ presentations: значения остаются в том виде, в каком их читает --param-json"
        )]
        json: bool,
        #[arg(
            long,
            help = "Не запрашивать представления ссылок и значений перечислений в результате. По умолчанию \
каждая ссылка и каждое значение перечисления в возвращаемом значении и параметрах (и внутри коллекций, таблиц, \
деревьев и списков значений) выводится со своим текстом, запрошенным после вызова, как это делает тонкий клиент \
(misc?cmd=refs для ссылок, types/view/<clsid>/values по разу на перечисление для синонимов значений)"
        )]
        no_presentations: bool,
    },
    #[command(
        about = "Модули, которые сервер отдаёт клиенту; с --out — тексты клиентских модулей",
        after_help = concat!(
            "Войти на сервер и показать модули, которые он отдаёт клиенту (имя, вид, id, версия); с --out \
записать текст каждого клиентского модуля так, как его получает тонкий клиент (modules/src)\n\n",
            after_help!()
        )
    )]
    Modules {
        #[command(flatten)]
        login: LoginArgs,
        #[arg(
            long,
            value_name = "КАТАЛОГ",
            help = "Записать тексты в этот каталог (несуществующий или пустой), по файлу <имя>.bsl на модуль: \
все клиентские модули списка или модули из --id. Записываются все файлы или ни одного"
        )]
        out: Option<PathBuf>,
        #[arg(
            long = "id",
            value_name = "URN",
            value_parser = ModuleUrn::parse,
            requires = "out",
            help = "Записать текст модуля с этим id вместо клиентских модулей списка; для нескольких повторите \
флаг. Для частей модуля формы — contextModuleId (ext='3') и woContextModuleId (ext='4') её формы. Текст \
серверного модуля (server в списке) сервер не отдаёт: на его id приходит HTTP 404"
        )]
        ids: Vec<ModuleUrn>,
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
}

fn request(command: Command, stdin_terminal: bool) -> Result<Request, String> {
    Ok(match command {
        Command::Info {
            login,
            no_members,
            no_sections,
            no_auth_settings,
            auth_settings_only,
            json,
        } => Request::Info(InfoRequest {
            login: login.login(stdin_terminal)?,
            members: !no_members,
            sections: !no_sections,
            auth_settings: !no_auth_settings,
            auth_settings_only,
            json,
        }),
        Command::Metadata {
            login,
            enum_synonyms,
            json,
        } => Request::Metadata(MetadataRequest {
            login: login.login(stdin_terminal)?,
            enum_synonyms,
            json,
        }),
        Command::Call {
            login,
            call,
            params,
            params_json,
            cold,
            json,
            no_presentations,
        } => Request::Call(CallRequest {
            login: login.login(stdin_terminal)?,
            call,
            params: params.into_iter().chain(params_json).collect(),
            cold,
            json,
            presentations: !no_presentations,
        }),
        Command::Modules {
            login,
            out,
            ids,
            json,
        } => Request::Modules(ModulesRequest {
            login: login.login(stdin_terminal)?,
            out,
            ids,
            json,
        }),
    })
}

pub(crate) fn run(
    command: Command,
    input: &mut dyn Read,
    out: &mut Out,
    err: &mut dyn Write,
    stdin_terminal: bool,
) -> yellowcode_core::Result<i32> {
    yellowcode_core::remote::run(
        request(command, stdin_terminal)?,
        out,
        err,
        &mut std::io::BufReader::new(input),
    )
}
