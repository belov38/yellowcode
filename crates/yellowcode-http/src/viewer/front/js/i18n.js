const L = {
  ru: {
    queryConsole: "Консоль запросов", notImplemented: "Не реализовано в Yellowcode", skin: "Скин", actions: "Действия",
    installSkinFile: "Установить из файла…", installSkinFolder: "Установить из папки…",
    gotoLine: "Перейти к строке", about: "О программе Yellowcode", windowList: "Окна", closeDialog: "Закрыть",
    cmPhrases: { "Find": "Найти", "Replace": "Заменить", "next": "следующее", "previous": "предыдущее", "all": "все", "match case": "учитывать регистр", "regexp": "регулярное выражение", "by word": "слово целиком", "replace": "заменить", "replace all": "заменить все", "close": "закрыть", "Go to line": "Перейти к строке", "go": "перейти" },
    props: "Свойства", module: "Модуль", forms: "Формы и макеты", form: "Форма", search: "Поиск: текст, имя…",
    formStats: "элементов: %b, заглушек: %s, пропущено свойств: %k, %m мс", openForm: "Открыть форму", genForm: "Форма (сгенерированная)", genRole: "Роль формы",
    names: "по именам", regex: "регулярное", more: "ещё", moreN: "ещё строк: ", earlierN: "ранее строк: ",
    queryBuilder: "Конструктор запроса из литерала…",
    check: "Проверить", checking: "Проверка…", clean: "Ошибок нет", errors: "Ошибок: ",
    formsH: "Формы", templatesH: "Макеты", modulesH: "Модули", commandsH: "Команды", none: "Нет",
    pick: "Выберите объект в дереве", loading: "Загрузка…", noModule: "У объекта нет модулей",
    found: "Найдено: ", shown: ", показано: ", nothing: "Ничего не найдено",
    objects: "объектов", modulesN: "модулей", linesN: "строк", formsN: "форм", templatesN: "макетов",
    line: "стр.", export: "Экспорт", syntax: "Синтаксическая ошибка: ", error: "Ошибка: ",
    menuFile: "Файл", menuWindow: "Окно", menuView: "Вид", open: "Открыть…", openFolder: "Открыть папку…",
    sourceMissing: "Файл не найден, убран из списка: ", closeTab: "Закрыть вкладку", closeAll: "Закрыть все вкладки",
    palette: "Палитра свойств", close: "Закрыть", loadingSource: "Загрузка источника: ", cancel: "Отмена", dismiss: "Скрыть",
    tree: "Дерево конфигурации", windows: "Окна", prop: "Свойство", value: "Значение",
    byAlphabet: "по алфавиту", otherGroup: "Прочие", openBtn: "Открыть", emptyTab: "На вкладке нет элементов",
    palHidden: "Палитра свойств скрыта", palShow: "Показать палитру", windowTabs: "Вкладки окна объекта",
    truncated: "Показаны не все элементы: ", modulesInline: "Модули:",
    collapseAll: "Свернуть всё", expandAll: "Развернуть всё", collapse: "Свернуть", expand: "Развернуть",
    find: "Поиск", findPlaceholder: "Найти в модуле", matchCase: "Учитывать регистр", prevMatch: "Предыдущее",
    nextMatch: "Следующее", noDefinition: "Определение не найдено", noCaret: "Поставьте курсор на имя",
    formElements: "Элементы", formAttributes: "Реквизиты", formCommands: "Команды", formParams: "Параметры",
    formPreview: "Предпросмотр", formTabs: "Вкладки формы", itemsN: "Элементов: ", name: "Имя", idCol: "Номер",
    tagCol: "Тег", mainCol: "Основной", actionCol: "Действие", keyCol: "Ключевой", expandAllItems: "Развернуть всё",
    openTemplate: "Открыть макет", openRights: "Открыть права роли", moreRows: "Показать ещё: ", viewSheet: "Табличный документ", viewDcs: "Схема компоновки данных",
    setsH: "Наборы данных", fieldsH: "Поля", linksH: "Связи наборов", resourcesH: "Ресурсы", parametersH: "Параметры",
    queryH: "Запрос", dataSource: "Источник данных", objectName: "Объект", kindCol: "Вид", pathCol: "Путь",
    titleCol: "Заголовок", expression: "Выражение", groupsCol: "Группы", sourceCol: "Источник",
    destinationCol: "Приемник", filesH: "Файлы", sizeCol: "Размер, байт", tooLarge: "Макет слишком велик для просмотра",
    inDevelopment: "В разработке", sheetPending: "Просмотр табличных документов в разработке. Доступна техническая информация.",
    technicalInfo: "Техническая информация", sheetRows: "Строк", sheetColumns: "Столбцов", sheetMerges: "Объединённых областей", valueCol: "Значение",
    rightsFilter: "Фильтр по имени объекта", rightsUndefined: "Права не заданы", onlyGranted: "Только с правами",
    flagNew: "Права для новых объектов", flagAttrs: "Права для реквизитов по умолчанию",
    flagIndep: "Независимые права подчинённых объектов", rlsTitle: "Ограничение доступа к данным",
    templatesH2: "Шаблоны ограничений", rowsN: "Объектов: ", objectCol: "Объект", rightsTable: "Права роли",
    menuHelp: "Справка", helperItem: "Синтакс-помощник", helperSearch: "Имя типа, метода или свойства",
    helperFound: "Найдено: ", helperPropsH: "Свойства", helperMethodsH: "Методы", helperValuesH: "Значения",
    helperAvail: "Доступность", helperRead: "Чтение", helperWrite: "Запись", helperOwner: "Тип: ", helperGlobal: "Глобальный контекст",
    helperTypeKind: "Тип", helperMethodKind: "Метод", helperPropKind: "Свойство", helperGlobalKind: "Глобальное",
    helperHint: "Введите имя и выберите результат", helperReturns: "Возвращает значение",
  },
  en: {
    queryConsole: "Query console", notImplemented: "Not implemented in Yellowcode", skin: "Skin", actions: "Actions",
    installSkinFile: "Install from file…", installSkinFolder: "Install from folder…",
    gotoLine: "Go to line", about: "About Yellowcode", windowList: "Windows", closeDialog: "Close",
    cmPhrases: {},
    props: "Properties", module: "Module", forms: "Forms and templates", form: "Form", search: "Search: text, name…",
    formStats: "items: %b, stubs: %s, skipped properties: %k, %m ms", openForm: "Open form", genForm: "Form (generated)", genRole: "Form role",
    names: "names", regex: "regex", more: "more", moreN: "more lines: ", earlierN: "earlier lines: ",
    queryBuilder: "Query builder from a literal…",
    check: "Check", checking: "Checking…", clean: "No errors", errors: "Errors: ",
    formsH: "Forms", templatesH: "Templates", modulesH: "Modules", commandsH: "Commands", none: "None",
    pick: "Select an object in the tree", loading: "Loading…", noModule: "The object has no modules",
    found: "Found: ", shown: ", shown: ", nothing: "Nothing found",
    objects: "objects", modulesN: "modules", linesN: "lines", formsN: "forms", templatesN: "templates",
    line: "line", export: "Export", syntax: "Syntax error: ", error: "Error: ",
    menuFile: "File", menuWindow: "Window", menuView: "View", open: "Open…", openFolder: "Open folder…",
    sourceMissing: "File not found, removed from the list: ", closeTab: "Close tab", closeAll: "Close all tabs",
    palette: "Property palette", close: "Close", loadingSource: "Loading source: ", cancel: "Cancel", dismiss: "Dismiss",
    tree: "Configuration tree", windows: "Windows", prop: "Property", value: "Value",
    byAlphabet: "by alphabet", otherGroup: "Other", openBtn: "Open", emptyTab: "No items on this tab",
    palHidden: "The property palette is hidden", palShow: "Show palette", windowTabs: "Object window tabs",
    truncated: "Not all items are shown: ", modulesInline: "Modules:",
    collapseAll: "Collapse all", expandAll: "Expand all", collapse: "Collapse", expand: "Expand",
    find: "Find", findPlaceholder: "Find in module", matchCase: "Match case", prevMatch: "Previous",
    nextMatch: "Next", noDefinition: "Definition not found", noCaret: "Put the caret on a name",
    formElements: "Elements", formAttributes: "Attributes", formCommands: "Commands", formParams: "Parameters",
    formPreview: "Preview", formTabs: "Form tabs", itemsN: "Elements: ", name: "Name", idCol: "Number",
    tagCol: "Tag", mainCol: "Main", actionCol: "Action", keyCol: "Key", expandAllItems: "Expand all",
    openTemplate: "Open template", openRights: "Open role rights", moreRows: "Show more: ", viewSheet: "Spreadsheet document", viewDcs: "Data composition schema",
    setsH: "Data sets", fieldsH: "Fields", linksH: "Set links", resourcesH: "Resources", parametersH: "Parameters",
    queryH: "Query", dataSource: "Data source", objectName: "Object", kindCol: "Kind", pathCol: "Path",
    titleCol: "Title", expression: "Expression", groupsCol: "Groups", sourceCol: "Source",
    destinationCol: "Destination", filesH: "Files", sizeCol: "Size, bytes", tooLarge: "The template is too large to show",
    inDevelopment: "In development", sheetPending: "Spreadsheet viewing is in development. Technical information is available.",
    technicalInfo: "Technical information", sheetRows: "Rows", sheetColumns: "Columns", sheetMerges: "Merged areas", valueCol: "Value",
    rightsFilter: "Filter by object name", rightsUndefined: "No rights are defined", onlyGranted: "Only with rights",
    flagNew: "Rights for new objects", flagAttrs: "Rights for attributes by default",
    flagIndep: "Independent rights of child objects", rlsTitle: "Row-level restriction",
    templatesH2: "Restriction templates", rowsN: "Objects: ", objectCol: "Object", rightsTable: "Role rights",
    menuHelp: "Help", helperItem: "Syntax helper", helperSearch: "Name of a type, method or property",
    helperFound: "Found: ", helperPropsH: "Properties", helperMethodsH: "Methods", helperValuesH: "Values",
    helperAvail: "Availability", helperRead: "Read", helperWrite: "Write", helperOwner: "Type: ", helperGlobal: "Global context",
    helperTypeKind: "Type", helperMethodKind: "Method", helperPropKind: "Property", helperGlobalKind: "Global",
    helperHint: "Type a name and pick a result", helperReturns: "Returns a value",
  },
};

const PL = {
  ru: {
    objects: ["объект", "объекта", "объектов"], modulesN: ["модуль", "модуля", "модулей"],
    linesN: ["строка", "строки", "строк"], formsN: ["форма", "формы", "форм"],
    templatesN: ["макет", "макета", "макетов"],
  },
  en: {
    objects: ["object", "objects"], modulesN: ["module", "modules"], linesN: ["line", "lines"],
    formsN: ["form", "forms"], templatesN: ["template", "templates"],
  },
};

const MODRU = {
  Module: "Модуль", ObjectModule: "МодульОбъекта", ManagerModule: "МодульМенеджера",
  RecordSetModule: "МодульНабораЗаписей", ValueManagerModule: "МодульМенеджераЗначения",
  CommandModule: "МодульКоманды", FormModule: "МодульФормы",
  ManagedApplicationModule: "МодульУправляемогоПриложения",
  OrdinaryApplicationModule: "МодульОбычногоПриложения", SessionModule: "МодульСеанса",
  ExternalConnectionModule: "МодульВнешнегоСоединения",
};

let lang = "ru";

export const T = Object.assign({}, L.ru);

export function setLang(l) {
  lang = l === "en" ? "en" : "ru";
  Object.assign(T, L[lang]);
  document.documentElement.lang = lang;
}

export function plural(n, key) {
  const f = PL[lang][key];
  if (lang === "en") return n + " " + (n === 1 ? f[0] : f[1]);
  const m10 = n % 10, m100 = n % 100;
  const i = m10 === 1 && m100 !== 11 ? 0 : m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14) ? 1 : 2;
  return n + " " + f[i];
}

export function modLabel(m) {
  return lang === "ru" && MODRU[m] ? MODRU[m] : m;
}
