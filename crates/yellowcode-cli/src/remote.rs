use std::io::{Read, Write};
use std::path::PathBuf;

use yellowcode_core::output::Out;
use yellowcode_core::remote::{
    CallRequest, DEFAULT_LIMIT, DEFAULT_PAGE_SIZE, DcsChange, DcsRequest, Edit, Filter,
    InfoRequest, ListRequest, Login, MetadataRequest, ModuleUrn, ModulesRequest, ObjectRef,
    ObjectRequest, Order, Param, Password, ReportFormat, ReportRequest, Request, Uuid,
    WriteCommand, WriteMode, WriteTarget,
};

macro_rules! after_help {
    () => {
        "\
Пароль пользователя (--user) не передаётся в командной строке и не набирается с клавиатуры: --password-env ИМЯ \
берёт его из переменной окружения, а без этого флага пароль читается одной строкой из stdin, поданного \
конвейером или из файла. Если stdin — терминал, команда отказывает до первого запроса. Пустой пароль — пустая \
строка: `printf '\\n' | yellowcode remote …` в sh, bash, zsh; `echo.| yellowcode remote …` в cmd. Ввод, который \
кончился раньше первой строки, — ошибка.

Адрес вида Srvr=\"хост[:порт]\";Ref=\"база\"; (строка подключения 1С) или tcp://хост[:порт]/база — подключение \
к серверу напрямую, как у тонкого клиента по /S, без веб-публикации: к менеджеру кластера (порт по умолчанию \
1541) или к ibsrv --direct-regport. Менеджер называет рабочий процесс по имени компьютера сервера; если это имя \
отсюда не разрешается, адрес рабочего процесса задаёт --worker хост:порт. Поддерживается только уровень \
безопасности соединения 0 (сервер, требующий шифрованного канала, получает отказ с объяснением).

Адрес https:// — соединение по TLS. Сертификат сервера проверяется всегда: по встроенным корням Mozilla \
(webpki-roots) и по сертификатам из файлов --ca-cert; отключить проверку нельзя.

Примеры:
  # Сводка сервера при входе без пользователя
  yellowcode remote info http://127.0.0.1:28736

  # Параметры входа базы с пользователями, без входа и без пароля
  yellowcode remote info http://server/base --auth-settings-only

  # Метаданные под пользователем, пароль из переменной окружения YC_PASSWORD
  yellowcode remote metadata http://server/base --user Администратор --password-env YC_PASSWORD

  # Напрямую к кластеру (/S), без веб-публикации
  yellowcode remote info 'Srvr=\"server:1541\";Ref=\"base\";' --user Администратор --password-env YC_PASSWORD

  # Напрямую к ibsrv --direct-regport через туннель: рабочий процесс тоже через туннель
  yellowcode remote info tcp://127.0.0.1:28730/base --worker 127.0.0.1:28731

  # Веб-публикация по HTTPS с корпоративным центром сертификации
  yellowcode remote info https://server/base --ca-cert corp-ca.pem

  # Пароль из файла
  yellowcode remote info http://server/base --user Администратор < password.txt

  # Пользователь без пароля (в cmd: echo.| yellowcode remote info …)
  printf '\\n' | yellowcode remote info http://server/base --user Гость

  # Вызвать экспортную функцию общего модуля с параметрами
  yellowcode remote call http://server/base --call МойМодуль.МояФункция --param n:10 --param s:текст

  # Записать тексты клиентских модулей в каталог
  yellowcode remote modules http://server/base --out client-modules

  # Вторая страница списка документов, по убыванию даты
  yellowcode remote list http://server/base --object Документ.ПриходнаяНакладная --order 'Дата desc' --page 2

  # Реквизиты формы отчёта, затем отчёт: серверная процедура формы с параметрами и её табличный документ
  yellowcode remote report http://server/base --form Отчет.Остатки.Форма.ФормаОтчета
  yellowcode remote report http://server/base --form Отчет.Остатки.Форма.ФормаОтчета \
    --call СформироватьНаСервере --param d:2026-11-30T23:59:59 --format csv > report.csv"
    };
}

pub(crate) const EXAMPLES: &str = after_help!();

/// The server and the login, common to every subcommand.
#[derive(clap::Args)]
pub(crate) struct LoginArgs {
    #[arg(
        help = "Адрес сервера 1С: веб-публикация или ibsrv --http-port (http://, https://), либо прямое подключение /S — Srvr=\"хост[:порт]\";Ref=\"база\"; или tcp://хост[:порт]/база"
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
    #[arg(
        long = "ca-cert",
        value_name = "PATH",
        help = "Доверять также сертификатам из этого файла PEM: корпоративный центр сертификации или самоподписанный сертификат сервера; можно повторять. Добавляются к встроенным корням, только для https://"
    )]
    ca_certs: Vec<PathBuf>,
    #[arg(
        long,
        value_name = "ВЕРСИЯ",
        value_parser = server_version,
        help = "Версия платформы, которую клиент сообщает серверу (например 8.3.27.2214); без неё — 8.5.1.1529, \
                а вход, отклонённый из-за версии, повторяется один раз с версией сервера из ответа"
    )]
    server_version: Option<String>,
    #[arg(
        long,
        value_name = "ХОСТ:ПОРТ",
        help = "Только /S: подключаться к рабочему процессу по этому адресу, а не по имени, которое назвал менеджер"
    )]
    worker: Option<String>,
}

fn server_version(v: &str) -> Result<String, String> {
    if yellowcode_core::remote::is_build(v) {
        Ok(v.to_string())
    } else {
        Err(format!(
            "not a platform build: {v:?} (for example 8.3.27.2214)"
        ))
    }
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
            ca_certs: self.ca_certs,
            server_version: self.server_version,
            worker: self.worker,
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
            conflicts_with_all = ["user", "password_env", "strong_password", "server_version", "no_members", "no_sections", "no_auth_settings"],
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
интерфейс (разделы и подразделы, их команды навигации и команды панели функций — в ней отчёты раздела, такие \
команды помечены `function panel`), как их видит пользователь. Объекты, которых нет в types (отчёты, обработки, \
журналы документов, общие команды и другие объекты без типа значения), называют только команды: они показаны в \
[objects] (в --json — objects) со своими командами и разделами. Формы не открываются, модули не вызываются\n\n",
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
        #[arg(
            long,
            help = "Показать реквизиты и табличные части каждого объекта из types, который называет команда навигации: \
описание данных его основной формы (logForm?cmd=dataDescrWMethods), один запрос на объект, поэтому по умолчанию \
выключено. \
Форма при этом не открывается и код конфигурации не выполняется. Объект без команды навигации (нет в командном \
интерфейсе пользователя) показывается без реквизитов; ошибка сервера показывается у объекта, код выхода 0. Для \
отчёта и обработки из [objects] — реквизиты и табличные части самого объекта по id его стандартной команды \
(Открыть; у отчёта она бывает только в панели функций): реквизиты собственной формы отчёта так не видны. Для \
журнала документов — колонки его основной формы списка (logForm?cmd=elements): графы и стандартные поля, которые \
форма показывает. Объект без стандартной команды (только свои команды) показывается без реквизитов"
        )]
        attributes: bool,
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
    #[command(
        about = "Прочитать объект по ссылке так, как его получает форма объекта: реквизиты и табличные части",
        after_help = concat!(
            "Войти на сервер, открыть форму объекта по ссылке, прочитать реквизиты и табличные части и закрыть \
форму, как это делает тонкий клиент (logForm?cmd=query, dataDescrWMethods, processFormsClose). Открытие формы \
выполняет код формы и объекта на сервере, как у пользователя; запись не выполняется, объект не блокируется\n\n",
            after_help!()
        )
    )]
    Object {
        #[command(flatten)]
        login: LoginArgs,
        #[arg(
            long = "ref",
            value_name = "ССЫЛКА",
            value_parser = ObjectRef::parse,
            help = "Ссылка на объект: cfg:<Вид>Ref.<Имя>(<uuid>), как её выводит call, или <Вид>.<Имя>(<uuid>) \
с русским названием вида, например Документ.Счет(<uuid>). Виды: Справочник, Документ, \
ПланВидовХарактеристик, ПланСчетов, ПланВидовРасчета, ПланОбмена, БизнесПроцесс, Задача; на сервере проверены \
справочники и документы"
        )]
        reference: ObjectRef,
        #[arg(
            long,
            help = "Вывести один объект JSON: реквизиты и строки табличных частей в типизированном JSON \
--param-json; значение типа, который клиент не разбирает, — {\"raw\": текст}. Представления ссылок — отдельный \
ключ presentations"
        )]
        json: bool,
        #[arg(
            long,
            help = "Не запрашивать представления ссылок и значений перечислений объекта. По умолчанию они \
запрашиваются после закрытия формы, как в call"
        )]
        no_presentations: bool,
    },
    #[command(
        about = "Строки динамического списка: справочника, журнала документов, записей регистра",
        after_help = concat!(
            "Войти на сервер, открыть форму списка объекта, как её открывает пользователь, вывести строки \
страницы в колонках формы и закрыть форму. Открытие формы выполняет её серверный код (как у пользователя); \
команда ничего не записывает. Ссылки и значения перечислений выводятся представлениями. Строки листаются с \
начала списка порциями сервера (dlist?cmd=query), поэтому дальняя страница стоит нескольких запросов\n\n",
            after_help!()
        )
    )]
    List {
        #[command(flatten)]
        login: LoginArgs,
        #[arg(
            long,
            value_name = "ВИД.ИМЯ",
            help = "Объект списка: <Вид>.<Имя>, например Справочник.Имя, Документ.Имя, РегистрСведений.Имя"
        )]
        object: String,
        #[arg(
            long,
            value_name = "ФОРМА",
            help = "Форма списка объекта (по умолчанию ФормаСписка)"
        )]
        form: Option<String>,
        #[arg(
            long,
            value_name = "UUID",
            value_parser = parse_uuid,
            help = "Строки группы иерархического списка с этой ссылкой (по умолчанию — корень)"
        )]
        parent: Option<Uuid>,
        #[arg(
            long = "filter",
            value_name = "ПОЛЕ ОП ЗНАЧЕНИЕ",
            value_parser = parse_filter,
            help = "Отбор: поле, операция (= <> < <= > >= ~ содержит, !~ не содержит) и значение, например \
'Наименование~Ф05', 'Цена>=10', 'Вид=Товар', 'Дата>=2026-11-01'. Значение ссылки — её uuid, значения \
перечисления — имя. Строка сравнивается как дана, с пробелами; у других значений пробелы вокруг отбрасываются. \
Для нескольких отборов (все сразу) повторите флаг"
        )]
        filters: Vec<Filter>,
        #[arg(
            long = "order",
            value_name = "ПОЛЕ [asc|desc]",
            value_parser = parse_order,
            help = "Порядок строк вместо порядка формы: поле и направление, например 'Дата desc'; для нескольких \
полей повторите флаг"
        )]
        orders: Vec<Order>,
        #[arg(
            long,
            value_name = "N",
            default_value_t = 1,
            conflicts_with = "all",
            help = "Номер страницы, с 1"
        )]
        page: usize,
        #[arg(
            long,
            value_name = "K",
            default_value_t = DEFAULT_PAGE_SIZE,
            conflicts_with = "all",
            help = "Строк на странице"
        )]
        page_size: usize,
        #[arg(long, help = "Все строки списка вместо страницы, не больше --limit")]
        all: bool,
        #[arg(
            long,
            value_name = "N",
            default_value_t = DEFAULT_LIMIT,
            help = "Наибольшее число строк, которое команда читает: у --all в списке больше строк — ошибка, у \
страницы, которая кончается дальше этой строки, — ошибка до открытия формы; ничего не выводится"
        )]
        limit: usize,
        #[arg(
            long,
            help = "Не запрашивать представления ссылок, которых нет в ответе списка, и синонимы значений \
перечислений: они выводятся своими id"
        )]
        no_presentations: bool,
        #[arg(long, help = JSON_HELP)]
        json: bool,
    },
    #[command(
        about = "Открыть форму отчёта и показать её реквизиты; с --call — вызвать серверную процедуру формы, с --generate — сформировать отчёт на СКД; вывести табличный документ",
        group = clap::ArgGroup::new("which").required(true).args(["form", "report"]),
        group = clap::ArgGroup::new("run").multiple(true).args(["call", "generate", "sets", "filters", "offs", "details"]),
        after_help = concat!(
            "Войти на сервер, открыть форму по её имени (logForm?cmd=query) и показать её реквизиты с типами; \
реквизит-табличный документ помечен (spreadsheet). С --call вызвать серверную процедуру этой формы \
(&НаСервере, как её зовёт клиентский обработчик команды «Сформировать») с параметрами --param/--param-json и \
вывести каждый табличный документ, который она изменила: строки документа, тексты ячеек через ` | `. Документ без \
содержимого дочитывается по строкам (moxel?cmd=getRowContents). Форма закрывается в конце (processFormsClose). \
Какую процедуру и с какими параметрами зовёт команда формы, видно в тексте модуля формы \
(remote modules --out, contextModuleId формы).\n\n\
Отчёт на схеме компоновки данных (СКД): --report Отчет.X открывает его форму по умолчанию (Отчет.X.Форма), \
--form — любую форму отчёта. Показываются варианты отчёта (текущий отмечен *) и пользовательские настройки \
(параметры и отборы: имя, заголовок, используется ли, значение и его тип). С --generate отчёт формируется \
платформенной командой «Сформировать» формы (logForm?cmd=call controller::composeResult) с настройками формы и \
изменениями --set, --filter, --off; сервер этих изменений не сохраняет. Значение приводится к типу параметра или поля \
схемы: строка как дана, число, булево (да/нет), дата ГГГГ-ММ-ДД[Тчч:мм:сс], стандартный период \
'ГГГГ-ММ-ДД..ГГГГ-ММ-ДД' или вариант периода (ThisYear, LastMonth, …), значение перечисления по имени, ссылка — \
её uuid или представление (поиск как при вводе текста в поле: ни одного или несколько найденных — ошибка до \
формирования). Неизвестная настройка, неверное значение и составной тип — ошибка до формирования. --details \
добавляет расшифровку каждой ячейки (dcs?cmd=loadDetailsPart): значения полей ячейки и её группировок. Форма, \
«Сформировать» которой — код конфигурации (например, форма отчёта БСП), документа не даёт: это ошибка с \
подсказкой вызвать её серверную процедуру через --call.\n\n\
ВНИМАНИЕ: --variant ЗАПИСЫВАЕТ В БАЗУ. Сервер запоминает выбранный вариант как текущий вариант отчёта у \
пользователя (в хранилище его настроек), так же как при выборе варианта в клиенте 1С: следующее открытие отчёта \
этим пользователем, в том числе в клиенте 1С, начнётся с этого варианта. Без --variant команда открывает текущий \
вариант и настроек не меняет.\n\n\
ВНИМАНИЕ: --call и --generate выполняют код конфигурации на сервере, как remote call: процедура формы и \
обработчики отчёта могут записывать данные базы. Вызывайте только процедуры и отчёты, про которые известно, что \
они только читают.\n\n\
Пределы: 100000 строк, 10000 колонок и 20000000 клеток (строки × колонки) на документ, 2000000 ячеек с \
текстом на отчёт; больший отчёт — ошибка без вывода.\n\n",
            after_help!()
        )
    )]
    Report {
        #[command(flatten)]
        login: LoginArgs,
        #[arg(
            long,
            value_name = "ИМЯ_ФОРМЫ",
            help = "Полное имя формы, например Отчет.Остатки.Форма.ФормаОтчета"
        )]
        form: Option<String>,
        #[arg(
            long,
            value_name = "ОТЧЕТ",
            conflicts_with = "call",
            help = "Отчёт, например Отчет.Остатки: открыть его форму по умолчанию (Отчет.Остатки.Форма) и показать \
варианты и пользовательские настройки СКД"
        )]
        report: Option<String>,
        #[arg(
            long,
            value_name = "ПРОЦЕДУРА",
            help = "Серверная процедура этой формы (&НаСервере), которую вызвать; без флага форма только \
открывается и показываются её реквизиты. Выполняет код конфигурации и может менять данные"
        )]
        call: Option<String>,
        #[arg(
            long = "param",
            value_name = "ЗНАЧЕНИЕ",
            value_parser = Param::parse,
            requires = "call",
            help = "Параметр процедуры, как у remote call --param; для нескольких повторите флаг, по порядку"
        )]
        params: Vec<Param>,
        #[arg(
            long = "param-json",
            value_name = "JSON",
            value_parser = Param::parse_json,
            conflicts_with = "params",
            requires = "call",
            help = "Параметр процедуры в типизированном JSON, как у remote call --param-json (пустой список \
значений: {\"vl\":{}}); не вместе с --param"
        )]
        params_json: Vec<Param>,
        #[arg(
            long,
            conflicts_with = "call",
            help = "Сформировать отчёт на СКД платформенной командой «Сформировать» формы; подразумевается \
флагами --set, --filter, --off, --details"
        )]
        generate: bool,
        #[arg(
            long,
            value_name = "ВАРИАНТ",
            conflicts_with = "call",
            help = "Вариант отчёта: ключ или представление. ЗАПИСЫВАЕТ В БАЗУ: сервер запоминает его как текущий \
вариант отчёта у пользователя, как клиент 1С"
        )]
        variant: Option<String>,
        #[arg(
            long = "set",
            value_name = "ПАРАМЕТР=ЗНАЧЕНИЕ",
            value_parser = parse_set,
            conflicts_with = "call",
            help = "Значение параметра из пользовательских настроек (по имени или заголовку), например \
'Период=2026-01-01..2026-03-31'; параметр включается. Для нескольких повторите флаг"
        )]
        sets: Vec<DcsChange>,
        #[arg(
            long = "filter",
            value_name = "ПОЛЕ ОП ЗНАЧЕНИЕ",
            value_parser = parse_filter,
            conflicts_with = "call",
            help = "Отбор из пользовательских настроек (по полю или заголовку): поле, операция (= <> < <= > >= \
~ содержит, !~ не содержит — только у строки) и значение, например 'Номенклатура=Гвозди'; отбор включается. Для нескольких \
повторите флаг"
        )]
        filters: Vec<Filter>,
        #[arg(
            long = "off",
            value_name = "НАСТРОЙКА",
            conflicts_with = "call",
            help = "Выключить параметр или отбор из пользовательских настроек (по имени или заголовку)"
        )]
        offs: Vec<String>,
        #[arg(
            long,
            conflicts_with = "call",
            help = "Добавить расшифровку ячеек отчёта на СКД: значения полей ячейки и её группировок"
        )]
        details: bool,
        #[arg(
            long,
            value_name = "РЕКВИЗИТ",
            requires = "run",
            help = "Вывести только табличный документ этого реквизита формы"
        )]
        attribute: Option<String>,
        #[arg(
            long,
            value_enum,
            default_value = "text",
            requires_if("csv", "run"),
            help = "text — реквизиты и строки документов; csv — один документ в CSV по RFC 4180 (все строки и \
колонки, строки через CRLF; строка RESULT при ошибке — в stderr), нужен --call или --generate; json — один \
объект JSON с ячейками, расшифровками и объединениями (строка RESULT при ошибке — в stderr)"
        )]
        format: ReportFormatArg,
    },
    #[command(
        about = "Создать объект (справочник, документ) через форму объекта, как тонкий клиент — ИЗМЕНЯЕТ ДАННЫЕ",
        after_help = concat!(
            "Войти на сервер, открыть форму нового объекта (logForm?cmd=query), заполнить реквизиты и строки \
табличных частей и записать объект через форму (logForm?cmd=call controller::write), затем закрыть форму. \
Сообщения пользователю (проверка заполнения, Сообщить) выводятся строками message:. Новый документ \
записывается без проведения, с --post — проводится. Код выхода: 0 — записано (или --dry-run), 1 — не записано \
или ошибка до записи, 3 — запись отправлена, итог неизвестен (RESULT UNKNOWN): прочитайте объект, прежде чем \
повторять.\n\n\
ВНИМАНИЕ: команда ИЗМЕНЯЕТ ДАННЫЕ БАЗЫ. Запись выполняет на сервере код формы и объекта (ПередЗаписью, \
проверку заполнения, проведение) так же, как запись пользователем. Запускайте её только на тестовых базах и \
копиях; сначала проверьте изменения с --dry-run. Отмены записи нет.\n\n",
            after_help!()
        )
    )]
    Create {
        #[command(flatten)]
        login: LoginArgs,
        #[arg(
            long = "set",
            value_name = "ПОЛЕ=ЗНАЧЕНИЕ",
            value_parser = Edit::parse_set,
            help = "Изменить реквизит: Поле=значение; ячейку строки табличной части: ТЧ[N].Поле=значение (N — \
номер строки, как его выводит object), ячейку новой строки: ТЧ[+N].Поле=значение (N — номер новой строки по \
порядку). Значение читается по типу поля: число 12.5, строка как есть, true/false, дата ГГГГ-ММ-ДД или \
ГГГГ-ММ-ДДTчч:мм:сс (время — чч:мм:сс), ссылка — uuid или <Вид>.<Имя>(<uuid>), перечисление — имя значения; \
пустое значение (Поле=) — пустое значение типа. Поле составного типа требует префикс, как у --param: s:, n:, \
d:, b:, u:, undef или ссылку. Имена — русские или английские, без учёта регистра. Для нескольких повторите флаг"
        )]
        set: Vec<Edit>,
        #[arg(
            long = "delete-row",
            value_name = "ТЧ[N]",
            value_parser = Edit::parse_delete,
            help = "Удалить строку N табличной части; для нескольких повторите флаг"
        )]
        delete_rows: Vec<Edit>,
        #[arg(
            long = "clear-table",
            value_name = "ТЧ",
            value_parser = Edit::parse_clear,
            help = "Очистить табличную часть (до добавления строк этой команды)"
        )]
        clear_tables: Vec<Edit>,
        #[arg(
            long,
            value_name = "ФАЙЛ",
            help = "Изменения в JSON (- — из stdin; пароль тогда только через --password-env): \
{\"set\":{\"Поле\":значение},\"tables\":{\"ТЧ\":{\"clear\":true,\"rows\":[{\"line\":2,\"set\":{…}},\
{\"line\":3,\"delete\":true},{\"add\":{…}}]}}}. Значение — строка (читается по типу поля, как в --set) или \
типизированный JSON --param-json. Флаги командной строки применяются после файла"
        )]
        data: Option<PathBuf>,
        #[arg(
            long,
            conflicts_with = "unpost",
            help = "Провести документ (РежимЗаписи Проведение)"
        )]
        post: bool,
        #[arg(
            long,
            help = "Отменить проведение документа (РежимЗаписи ОтменаПроведения)"
        )]
        unpost: bool,
        #[arg(
            long = "dry-run",
            help = "Не записывать: войти, открыть форму (при этом выполняется код формы на сервере), описать и \
закрыть её и вывести изменения и тело запроса controller::write, которое ушло бы на сервер"
        )]
        dry_run: bool,
        #[arg(long, help = JSON_HELP)]
        json: bool,
        #[arg(
            long,
            value_name = "ОБЪЕКТ",
            value_parser = WriteTarget::parse_new,
            help = "Вид и имя объекта: <Вид>.<Имя>, например Справочник.Имя или Документ.Имя. \
Записываются справочники и документы"
        )]
        object: WriteTarget,
    },
    #[command(
        about = "Изменить объект по ссылке через форму объекта, как тонкий клиент — ИЗМЕНЯЕТ ДАННЫЕ",
        after_help = concat!(
            "Войти на сервер, открыть форму объекта по ссылке, изменить реквизиты и строки табличных частей и \
записать объект через форму (logForm?cmd=call controller::write), затем закрыть форму. На сервер уходят только \
изменённые поля и всегда ВерсияДанных: если объект изменили после открытия формы, сервер отказывает. Проведённый \
документ с изменениями без --post/--unpost перепроводится (пометка на удаление снимает проведение), непроведённый \
записывается без проведения; без изменений документ записывается только с --post или --unpost. Если запись \
отправлена, а ответ не получен или не разобран, команда сообщает, что итог неизвестен: прочитайте объект (object), \
прежде чем повторять. Код выхода: 0 — записано (или --dry-run), 1 — не записано или ошибка до записи, 3 — \
запись отправлена, итог неизвестен (RESULT UNKNOWN).\n\n\
ВНИМАНИЕ: команда ИЗМЕНЯЕТ ДАННЫЕ БАЗЫ. Запись выполняет на сервере код формы и объекта (ПередЗаписью, \
проверку заполнения, проведение) так же, как запись пользователем. Запускайте её только на тестовых базах и \
копиях; сначала проверьте изменения с --dry-run. Отмены записи нет.\n\n",
            after_help!()
        )
    )]
    Write {
        #[command(flatten)]
        login: LoginArgs,
        #[arg(
            long = "set",
            value_name = "ПОЛЕ=ЗНАЧЕНИЕ",
            value_parser = Edit::parse_set,
            help = "Изменить реквизит: Поле=значение; ячейку строки табличной части: ТЧ[N].Поле=значение (N — \
номер строки, как его выводит object), ячейку новой строки: ТЧ[+N].Поле=значение (N — номер новой строки по \
порядку). Значение читается по типу поля: число 12.5, строка как есть, true/false, дата ГГГГ-ММ-ДД или \
ГГГГ-ММ-ДДTчч:мм:сс (время — чч:мм:сс), ссылка — uuid или <Вид>.<Имя>(<uuid>), перечисление — имя значения; \
пустое значение (Поле=) — пустое значение типа. Поле составного типа требует префикс, как у --param: s:, n:, \
d:, b:, u:, undef или ссылку. Имена — русские или английские, без учёта регистра. Для нескольких повторите флаг"
        )]
        set: Vec<Edit>,
        #[arg(
            long = "delete-row",
            value_name = "ТЧ[N]",
            value_parser = Edit::parse_delete,
            help = "Удалить строку N табличной части; для нескольких повторите флаг"
        )]
        delete_rows: Vec<Edit>,
        #[arg(
            long = "clear-table",
            value_name = "ТЧ",
            value_parser = Edit::parse_clear,
            help = "Очистить табличную часть (до добавления строк этой команды)"
        )]
        clear_tables: Vec<Edit>,
        #[arg(
            long,
            value_name = "ФАЙЛ",
            help = "Изменения в JSON (- — из stdin; пароль тогда только через --password-env): \
{\"set\":{\"Поле\":значение},\"tables\":{\"ТЧ\":{\"clear\":true,\"rows\":[{\"line\":2,\"set\":{…}},\
{\"line\":3,\"delete\":true},{\"add\":{…}}]}}}. Значение — строка (читается по типу поля, как в --set) или \
типизированный JSON --param-json. Флаги командной строки применяются после файла"
        )]
        data: Option<PathBuf>,
        #[arg(
            long,
            conflicts_with = "unpost",
            help = "Провести документ (РежимЗаписи Проведение)"
        )]
        post: bool,
        #[arg(
            long,
            help = "Отменить проведение документа (РежимЗаписи ОтменаПроведения)"
        )]
        unpost: bool,
        #[arg(
            long = "dry-run",
            help = "Не записывать: войти, открыть форму (при этом выполняется код формы на сервере), описать и \
закрыть её и вывести изменения и тело запроса controller::write, которое ушло бы на сервер"
        )]
        dry_run: bool,
        #[arg(long, help = JSON_HELP)]
        json: bool,
        #[arg(
            long = "ref",
            value_name = "ССЫЛКА",
            value_parser = WriteTarget::parse_existing,
            help = "Ссылка на объект: cfg:<Вид>Ref.<Имя>(<uuid>) или <Вид>.<Имя>(<uuid>), как у object"
        )]
        reference: WriteTarget,
        #[arg(
            long = "mark-deletion",
            conflicts_with = "unmark_deletion",
            help = "Пометить объект на удаление (ПометкаУдаления = Истина)"
        )]
        mark_deletion: bool,
        #[arg(long = "unmark-deletion", help = "Снять пометку удаления")]
        unmark_deletion: bool,
    },
}

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse(s).map_err(|e| e.to_string())
}

fn parse_filter(s: &str) -> Result<Filter, String> {
    Filter::parse(s).map_err(|e| e.to_string())
}

fn parse_set(s: &str) -> Result<DcsChange, String> {
    DcsChange::parse_set(s).ok_or_else(|| format!("not ПАРАМЕТР=ЗНАЧЕНИЕ: {s:?}"))
}

fn parse_order(s: &str) -> Result<Order, String> {
    Order::parse(s).map_err(|e| e.to_string())
}

/// The output form of `report`.
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum ReportFormatArg {
    Text,
    Csv,
    Json,
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
            attributes,
            json,
        } => Request::Metadata(MetadataRequest {
            login: login.login(stdin_terminal)?,
            enum_synonyms,
            attributes,
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
        Command::Object {
            login,
            reference,
            json,
            no_presentations,
        } => Request::Object(ObjectRequest {
            login: login.login(stdin_terminal)?,
            reference,
            json,
            presentations: !no_presentations,
        }),
        Command::List {
            login,
            object,
            form,
            parent,
            filters,
            orders,
            page,
            page_size,
            all,
            limit,
            no_presentations,
            json,
        } => Request::List(ListRequest {
            login: login.login(stdin_terminal)?,
            object,
            form,
            parent,
            filters,
            orders,
            page,
            page_size,
            all,
            limit,
            presentations: !no_presentations,
            json,
        }),
        Command::Report {
            login,
            form,
            report,
            call,
            params,
            params_json,
            generate,
            variant,
            sets,
            filters,
            offs,
            details,
            attribute,
            format,
        } => Request::Report(ReportRequest {
            login: login.login(stdin_terminal)?,
            dcs: dcs_request(
                report.is_some(),
                generate,
                variant,
                sets,
                filters,
                offs,
                details,
            ),
            form: form
                .or_else(|| report.map(|r| format!("{r}.Форма")))
                .unwrap_or_default(),
            call,
            params: params.into_iter().chain(params_json).collect(),
            attribute,
            format: match format {
                ReportFormatArg::Text => ReportFormat::Text,
                ReportFormatArg::Csv => ReportFormat::Csv,
                ReportFormatArg::Json => ReportFormat::Json,
            },
        }),
        Command::Create {
            login,
            set,
            delete_rows,
            clear_tables,
            data,
            post,
            unpost,
            dry_run,
            json,
            object,
        } => Request::Write(WriteCommand {
            login: login.login(stdin_terminal)?,
            target: object,
            edits: set
                .into_iter()
                .chain(delete_rows)
                .chain(clear_tables)
                .collect(),
            data,
            mode: write_mode(post, unpost),
            deletion_mark: None,
            dry_run,
            json,
        }),
        Command::Write {
            login,
            set,
            delete_rows,
            clear_tables,
            data,
            post,
            unpost,
            dry_run,
            json,
            reference,
            mark_deletion,
            unmark_deletion,
        } => Request::Write(WriteCommand {
            login: login.login(stdin_terminal)?,
            target: reference,
            edits: set
                .into_iter()
                .chain(delete_rows)
                .chain(clear_tables)
                .collect(),
            data,
            mode: write_mode(post, unpost),
            deletion_mark: match (mark_deletion, unmark_deletion) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            },
            dry_run,
            json,
        }),
    })
}

fn write_mode(post: bool, unpost: bool) -> WriteMode {
    match (post, unpost) {
        (true, _) => WriteMode::Post,
        (_, true) => WriteMode::Unpost,
        _ => WriteMode::Default,
    }
}

fn dcs_request(
    report: bool,
    generate: bool,
    variant: Option<String>,
    sets: Vec<DcsChange>,
    filters: Vec<Filter>,
    offs: Vec<String>,
    details: bool,
) -> Option<DcsRequest> {
    let changes: Vec<DcsChange> = sets
        .into_iter()
        .chain(filters.into_iter().map(DcsChange::Filter))
        .chain(offs.into_iter().map(|name| DcsChange::Off { name }))
        .collect();
    let generate = generate || details || !changes.is_empty();
    (report || generate || variant.is_some()).then_some(DcsRequest {
        variant,
        changes,
        generate,
        details,
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
