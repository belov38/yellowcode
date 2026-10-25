use super::L5Msg;

pub(crate) static MESSAGES: &[L5Msg] = &[
    L5Msg {
        key: "accnt:IDS_ACCNTREG_EXTDIMCOLLECTION_TYPE_PRESENT",
        ru: "Регистр бухгалтерии субконто",
        en: "Accounting register Ext dimensions",
    },
    L5Msg {
        key: "accnt:IDS_ACCNTREG_LIST_TYPE_PRESENT",
        ru: "Регистр бухгалтерии список",
        en: "Accounting register List",
    },
    L5Msg {
        key: "accnt:IDS_ACCNTREG_MANAGER_TYPE_PRESENT",
        ru: "Регистр бухгалтерии  менеджер",
        en: "Accounting register Manager",
    },
    L5Msg {
        key: "accnt:IDS_ACCNTREG_RECORDKEY_TYPE_PRESENT",
        ru: "Регистр бухгалтерии ключ записи",
        en: "Accounting register Record key",
    },
    L5Msg {
        key: "accnt:IDS_ACCNTREG_RECORDSET_TYPE_PRESENT",
        ru: "Регистр бухгалтерии набор записей",
        en: "Accounting register Record set",
    },
    L5Msg {
        key: "accnt:IDS_ACCNTREG_RECORD_TYPE_PRESENT",
        ru: "Регистр бухгалтерии запись",
        en: "Accounting register Record",
    },
    L5Msg {
        key: "accnt:IDS_ACCNTREG_SEL_TYPE_PRESENT",
        ru: "Регистр бухгалтерии выборка",
        en: "Accounting register Selected data",
    },
    L5Msg {
        key: "accnt:IDS_CHART_OF_ACCOUNTS_EXTDIMROW_TYPE_PRESENT",
        ru: "План счетов виды субконто строка",
        en: "Chart of accounts Ext dimension types Row",
    },
    L5Msg {
        key: "accnt:IDS_CHART_OF_ACCOUNTS_EXTDIM_TYPE_PRESENT",
        ru: "План счетов виды субконто",
        en: "Chart of accounts Ext dimension types",
    },
    L5Msg {
        key: "accnt:IDS_CHART_OF_ACCOUNTS_LIST_TYPE_PRESENT",
        ru: "План счетов список",
        en: "Chart of accounts List",
    },
    L5Msg {
        key: "accnt:IDS_CHART_OF_ACCOUNTS_MANAGER_TYPE_PRESENT",
        ru: "План счетов менеджер",
        en: "Chart of accounts Manager",
    },
    L5Msg {
        key: "accnt:IDS_CHART_OF_ACCOUNTS_OBJECT_TYPE_PRESENT",
        ru: "План счетов объект",
        en: "Chart of accounts Object",
    },
    L5Msg {
        key: "accnt:IDS_CHART_OF_ACCOUNTS_REF_TYPE_PRESENT",
        ru: "План счетов ссылка",
        en: "Chart of accounts Ref",
    },
    L5Msg {
        key: "accnt:IDS_CHART_OF_ACCOUNTS_SEL_TYPE_PRESENT",
        ru: "План счетов выборка",
        en: "Chart of accounts Selected data",
    },
    L5Msg {
        key: "accnt:IDS_CHART_OF_ACCOUNTS_TABULARPARTROW_TYPE_PRESENT",
        ru: "План счетов табличная часть строка",
        en: "Chart of accounts Tabular section Row",
    },
    L5Msg {
        key: "accnt:IDS_CHART_OF_ACCOUNTS_TABULARPART_TYPE_PRESENT",
        ru: "План счетов табличная часть",
        en: "Chart of accounts Tabular section",
    },
    L5Msg {
        key: "backend:IDS_CANT_ORDER_BY_THIS_FIELD",
        ru: "Выборка может быть упорядочена только по реквизитам, имеющими базовый тип и для которых установлен признак индексирования",
        en: "Selected data can only be ordered by attributes of base type with indexing flag set",
    },
    L5Msg {
        key: "backend:IDS_DATAEXCHANGEENDPOINTS_OBJECT_TYPE_PRESENT",
        ru: "План обмена объект",
        en: "Exchange plan Object",
    },
    L5Msg {
        key: "backend:IDS_DATAEXCHANGEENDPOINT_LIST_TYPE_PRESENT",
        ru: "План обмена список",
        en: "Exchange plan List",
    },
    L5Msg {
        key: "backend:IDS_DATAEXCHANGEENDPOINT_MANAGER_TYPE_PRESENT",
        ru: "План обмена менеджер",
        en: "Exchange plan Manager",
    },
    L5Msg {
        key: "backend:IDS_DATAEXCHANGEENDPOINT_REF_TYPE_PRESENT",
        ru: "План обмена ссылка",
        en: "Exchange plan Ref",
    },
    L5Msg {
        key: "backend:IDS_DATAEXCHANGEENDPOINT_SEL_TYPE_PRESENT",
        ru: "План обмена выборка",
        en: "Exchange plan Selected data",
    },
    L5Msg {
        key: "backend:IDS_DATAEXCHANGEENDPOINT_TABULARPARTROW_TYPE_PRESENT",
        ru: "План обмена табличная часть строка",
        en: "Exchange plan Tabular section Row",
    },
    L5Msg {
        key: "backend:IDS_DATAEXCHANGEENDPOINT_TABULARPART_TYPE_PRESENT",
        ru: "План обмена табличная часть",
        en: "Exchange plan Tabular section",
    },
    L5Msg {
        key: "backend:IDS_DBERROR_LOCKING",
        ru: "Конфликт блокировок при выполнении транзакции:",
        en: "Lock conflict during the transaction:",
    },
    L5Msg {
        key: "backend:IDS_DBOBJECT_NOTFOUND",
        ru: "<Объект не найден>",
        en: "<Object not found>",
    },
    L5Msg {
        key: "backend:IDS_ERR_INVALID_PROPERTY_NAME",
        ru: "Свойство %s не является свойством объекта метаданных %s.",
        en: "Property %s does not belong to metadata object %s.",
    },
    L5Msg {
        key: "backend:IDS_PREDEFINEDOBJ_NOT_EXISTS",
        ru: "%s. Предопределенный элемент отсутствует в данных",
        en: "%s. The predefined item is not found in the infobase data",
    },
    L5Msg {
        key: "backend:IDS_PREDEFINED_ITEM_NOT_EXISTS",
        ru: "Предопределенный элемент не существует",
        en: "Predefined object does not exist",
    },
    L5Msg {
        key: "backend:IDS_PREDEFINED_ITEM_NOT_UNIQUE",
        ru: "Предопределенный элемент не уникален",
        en: "Predefined object is not unique",
    },
    L5Msg {
        key: "backend:IDS_SESSIONPARAMS_MANAGER_TYPE_PRESENT",
        ru: "Параметры сеанса",
        en: "Session parameters",
    },
    L5Msg {
        key: "backend:IDS_SESSION_PARAM_NOT_INITIALIZED",
        ru: "Попытка получения неинициализированного значения параметра сеанса",
        en: "Attempt to obtain an uninitialized value of session parameter",
    },
    L5Msg {
        key: "backend:IDS_TYPE_PRESENTATION_MDCONTEXT",
        ru: "Объект метаданных",
        en: "Metadata object",
    },
    L5Msg {
        key: "basic:IDS_ACCUMREG_LIST_TYPE_PRESENT",
        ru: "Регистр накопления список",
        en: "Accumulation register List",
    },
    L5Msg {
        key: "basic:IDS_ACCUMREG_MANAGER_TYPE_PRESENT",
        ru: "Регистр накопления менеджер",
        en: "Accumulation register Manager",
    },
    L5Msg {
        key: "basic:IDS_ACCUMREG_RECORDKEY_TYPE_PRESENT",
        ru: "Регистр накопления ключ записи",
        en: "Accumulation register Record key",
    },
    L5Msg {
        key: "basic:IDS_ACCUMREG_RECORDSET_TYPE_PRESENT",
        ru: "Регистр накопления набор записей",
        en: "Accumulation register Record set",
    },
    L5Msg {
        key: "basic:IDS_ACCUMREG_RECORD_TYPE_PRESENT",
        ru: "Регистр накопления запись",
        en: "Accumulation register Record",
    },
    L5Msg {
        key: "basic:IDS_ACCUMREG_SEL_TYPE_PRESENT",
        ru: "Регистр накопления выборка",
        en: "Accumulation register Selected data",
    },
    L5Msg {
        key: "basic:IDS_CANT_PROCEE_FOR_NON_HIERACICAL",
        ru: "Операция не может быть выполнена для неиерархического справочника.",
        en: "Operation cannot be performed for non-hierarchical catalog.",
    },
    L5Msg {
        key: "basic:IDS_CONST_MANAGER_TYPE_PRESENT",
        ru: "Константа менеджер",
        en: "Constant Manager",
    },
    L5Msg {
        key: "basic:IDS_CONST_VALUEKEY_TYPE_PRESENT",
        ru: "Константа ключ значения",
        en: "Constant key value",
    },
    L5Msg {
        key: "basic:IDS_CONST_VALUEMANAGER_TYPE_PRESENT",
        ru: "Константа менеджер значения",
        en: "Constant value manager",
    },
    L5Msg {
        key: "basic:IDS_DOCJOURNAL_LIST_TYPE_PRESENT",
        ru: "Журнал документов список",
        en: "Document journal List",
    },
    L5Msg {
        key: "basic:IDS_DOCJOURNAL_MANAGER_TYPE_PRESENT",
        ru: "Журнал документов менеджер",
        en: "Document journal Manager",
    },
    L5Msg {
        key: "basic:IDS_DOCJOURNAL_SEL_TYPE_PRESENT",
        ru: "Журнал документов выборка",
        en: "Document journal Selected data",
    },
    L5Msg {
        key: "basic:IDS_DOCUMENT_LIST_TYPE_PRESENT",
        ru: "Документ список",
        en: "Document List",
    },
    L5Msg {
        key: "basic:IDS_DOCUMENT_MANAGER_TYPE_PRESENT",
        ru: "Документ менеджер",
        en: "Document Manager",
    },
    L5Msg {
        key: "basic:IDS_DOCUMENT_OBJECT_TYPE_PRESENT",
        ru: "Документ объект",
        en: "Document Object",
    },
    L5Msg {
        key: "basic:IDS_DOCUMENT_REF_TYPE_PRESENT",
        ru: "Документ ссылка",
        en: "Document Ref",
    },
    L5Msg {
        key: "basic:IDS_DOCUMENT_SEL_TYPE_PRESENT",
        ru: "Документ выборка",
        en: "Document Selected data",
    },
    L5Msg {
        key: "basic:IDS_DOCUMENT_TABULARPARTROW_TYPE_PRESENT",
        ru: "Документ табличная часть строка",
        en: "Document Tabular section Row",
    },
    L5Msg {
        key: "basic:IDS_DOCUMENT_TABULARPART_TYPE_PRESENT",
        ru: "Документ табличная часть",
        en: "Document Tabular section",
    },
    L5Msg {
        key: "basic:IDS_ELEMENT_NOT_SELECTED",
        ru: "Элемент не выбран!",
        en: "No element selected!",
    },
    L5Msg {
        key: "basic:IDS_ENUM_LIST_TYPE_PRESENT",
        ru: "Перечисление список",
        en: "Enumeration list",
    },
    L5Msg {
        key: "basic:IDS_ENUM_MANAGER_TYPE_PRESENT",
        ru: "Перечисление менеджер",
        en: "Enumeration Manager",
    },
    L5Msg {
        key: "basic:IDS_ENUM_REF_TYPE_PRESENT",
        ru: "Перечисление ссылка",
        en: "Enumeration Ref",
    },
    L5Msg {
        key: "basic:IDS_INFOREG_LIST_TYPE_PRESENT",
        ru: "Регистр сведений список",
        en: "Information register List",
    },
    L5Msg {
        key: "basic:IDS_INFOREG_MANAGER_TYPE_PRESENT",
        ru: "Регистр сведений менеджер",
        en: "Information register Manager",
    },
    L5Msg {
        key: "basic:IDS_INFOREG_RECORDKEY_TYPE_PRESENT",
        ru: "Регистр сведений ключ записи",
        en: "Information register Record key",
    },
    L5Msg {
        key: "basic:IDS_INFOREG_RECORDMANAGER_TYPE_PRESENT",
        ru: "Регистр сведений менеджер записи",
        en: "Information register Record manager",
    },
    L5Msg {
        key: "basic:IDS_INFOREG_RECORDSET_TYPE_PRESENT",
        ru: "Регистр сведений набор записей",
        en: "Information register Record set",
    },
    L5Msg {
        key: "basic:IDS_INFOREG_RECORD_TYPE_PRESENT",
        ru: "Регистр сведений запись",
        en: "Information register Record",
    },
    L5Msg {
        key: "basic:IDS_INFOREG_SEL_TYPE_PRESENT",
        ru: "Регистр сведений выборка",
        en: "Information register Selected data",
    },
    L5Msg {
        key: "basic:IDS_PLANOFCHARACTERISTICKIND_LIST_TYPE_PRESENT",
        ru: "План видов характеристик список",
        en: "Chart of characteristic types List",
    },
    L5Msg {
        key: "basic:IDS_PLANOFCHARACTERISTICKIND_MANAGER_TYPE_PRESENT",
        ru: "План видов характеристик менеджер",
        en: "Chart of characteristic types Manager",
    },
    L5Msg {
        key: "basic:IDS_PLANOFCHARACTERISTICKIND_OBJECT_TYPE_PRESENT",
        ru: "План видов характеристик объект",
        en: "Chart of characteristic types Object",
    },
    L5Msg {
        key: "basic:IDS_PLANOFCHARACTERISTICKIND_REF_TYPE_PRESENT",
        ru: "План видов характеристик ссылка",
        en: "Chart of characteristic types Ref",
    },
    L5Msg {
        key: "basic:IDS_PLANOFCHARACTERISTICKIND_SEL_TYPE_PRESENT",
        ru: "План видов характеристик выборка",
        en: "Chart of characteristic types Selected data",
    },
    L5Msg {
        key: "basic:IDS_PLANOFCHARACTERISTICKIND_TABULARPARTROW_TYPE_PRESENT",
        ru: "План видов характеристик табличная часть строка",
        en: "Chart of characteristic types Tabular section Row",
    },
    L5Msg {
        key: "basic:IDS_PLANOFCHARACTERISTICKIND_TABULARPART_TYPE_PRESENT",
        ru: "План видов характеристик табличная часть",
        en: "Chart of characteristic types Tabular section",
    },
    L5Msg {
        key: "basic:IDS_PROCESSING_MANAGER_TYPE_PRESENT",
        ru: "Обработка менеджер",
        en: "Data processor Manager",
    },
    L5Msg {
        key: "basic:IDS_PROCESSING_OBJECT_TYPE_PRESENT",
        ru: "Обработка объект",
        en: "Data processor Object",
    },
    L5Msg {
        key: "basic:IDS_PROCESSING_TABULARPARTROW_TYPE_PRESENT",
        ru: "Обработка табличная часть строка",
        en: "Data processor Tabular section Row",
    },
    L5Msg {
        key: "basic:IDS_PROCESSING_TABULARPART_TYPE_PRESENT",
        ru: "Обработка табличная часть",
        en: "Data processor Tabular section",
    },
    L5Msg {
        key: "basic:IDS_REFERENCES_OBJECT_TYPE_PRESENT",
        ru: "Справочник объект",
        en: "Catalog Object",
    },
    L5Msg {
        key: "basic:IDS_REFERENCE_LIST_TYPE_PRESENT",
        ru: "Справочник список",
        en: "Catalog List",
    },
    L5Msg {
        key: "basic:IDS_REFERENCE_MANAGER_TYPE_PRESENT",
        ru: "Справочник менеджер",
        en: "Catalog Manager",
    },
    L5Msg {
        key: "basic:IDS_REFERENCE_REF_TYPE_PRESENT",
        ru: "Справочник ссылка",
        en: "Catalog Ref",
    },
    L5Msg {
        key: "basic:IDS_REFERENCE_SEL_TYPE_PRESENT",
        ru: "Справочник выборка",
        en: "Catalog Selected data",
    },
    L5Msg {
        key: "basic:IDS_REFERENCE_TABULARPARTROW_TYPE_PRESENT",
        ru: "Справочник табличная часть строка",
        en: "Catalog Tabular section Row",
    },
    L5Msg {
        key: "basic:IDS_REFERENCE_TABULARPART_TYPE_PRESENT",
        ru: "Справочник табличная часть",
        en: "Catalog Tabular section",
    },
    L5Msg {
        key: "basic:IDS_REPORT_MANAGER_TYPE_PRESENT",
        ru: "Отчет менеджер",
        en: "Report Manager",
    },
    L5Msg {
        key: "basic:IDS_REPORT_OBJECT_TYPE_PRESENT",
        ru: "Отчет объект",
        en: "Report Object",
    },
    L5Msg {
        key: "basic:IDS_REPORT_TABULARPARTROW_TYPE_PRESENT",
        ru: "Отчет табличная часть строка",
        en: "Report Tabular section Row",
    },
    L5Msg {
        key: "basic:IDS_REPORT_TABULARPART_TYPE_PRESENT",
        ru: "Отчет табличная часть",
        en: "Report Tabular section",
    },
    L5Msg {
        key: "basic:IDS_SEQUENCE_MANAGER_TYPE_PRESENT",
        ru: "Последовательность менеджер",
        en: "Sequence Manager",
    },
    L5Msg {
        key: "basic:IDS_SEQUENCE_RECORDSET_TYPE_PRESENT",
        ru: "Последовательность набор записей",
        en: "Sequence Record set",
    },
    L5Msg {
        key: "basic:IDS_SEQUENCE_RECORD_TYPE_PRESENT",
        ru: "Последвательность запись",
        en: "Sequence Record",
    },
    L5Msg {
        key: "basic:IDS_WRONG_PARAMETER_FLDNAME",
        ru: "Неверно указан реквизит",
        en: "Attribute specified incorrectly",
    },
    L5Msg {
        key: "bp:IDS_BPPROCESSROUTEPOINT_REF_TYPE_PRESENT",
        ru: "Точка маршрута бизнес-процесса ссылка",
        en: "Business process route point Reference",
    },
    L5Msg {
        key: "bp:IDS_BPPROCESS_LIST_TYPE_PRESENT",
        ru: "Бизнес-процесс список",
        en: "Business process List",
    },
    L5Msg {
        key: "bp:IDS_BPPROCESS_MANAGER_TYPE_PRESENT",
        ru: "Бизнес-процесс менеджер",
        en: "Business process Manager",
    },
    L5Msg {
        key: "bp:IDS_BPPROCESS_OBJECT_TYPE_PRESENT",
        ru: "Бизнес-процесс объект",
        en: "Business process Object",
    },
    L5Msg {
        key: "bp:IDS_BPPROCESS_REF_TYPE_PRESENT",
        ru: "Бизнес-процесс ссылка",
        en: "Business process Ref",
    },
    L5Msg {
        key: "bp:IDS_BPPROCESS_SEL_TYPE_PRESENT",
        ru: "Бизнес-процесс выборка",
        en: "Business process Selected data",
    },
    L5Msg {
        key: "bp:IDS_BPPROCESS_TABULARPARTROW_TYPE_PRESENT",
        ru: "Бизнес-процесс табличная часть строка",
        en: "Business process Tabular section Row",
    },
    L5Msg {
        key: "bp:IDS_BPPROCESS_TABULARPART_TYPE_PRESENT",
        ru: "Бизнес-процесс табличная часть",
        en: "Business process Tabular section",
    },
    L5Msg {
        key: "bp:IDS_BPTASK_LIST_TYPE_PRESENT",
        ru: "Задача список",
        en: "Task List",
    },
    L5Msg {
        key: "bp:IDS_BPTASK_MANAGER_TYPE_PRESENT",
        ru: "Задача менеджер",
        en: "Task Manager",
    },
    L5Msg {
        key: "bp:IDS_BPTASK_OBJECT_TYPE_PRESENT",
        ru: "Задача объект",
        en: "Task Object",
    },
    L5Msg {
        key: "bp:IDS_BPTASK_REF_TYPE_PRESENT",
        ru: "Задача ссылка",
        en: "Task Ref",
    },
    L5Msg {
        key: "bp:IDS_BPTASK_SEL_TYPE_PRESENT",
        ru: "Задача выборка",
        en: "Task Selected data",
    },
    L5Msg {
        key: "bp:IDS_BPTASK_TABULARPARTROW_TYPE_PRESENT",
        ru: "Задача табличная часть строка",
        en: "Task Tabular section Row",
    },
    L5Msg {
        key: "bp:IDS_BPTASK_TABULARPART_TYPE_PRESENT",
        ru: "Задача табличная часть",
        en: "Task Tabular section",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_BASECKTABULARPARTROW_TYPE_PRESENT",
        ru: "Базовые виды расчета строка",
        en: "Baseline calculation types Row",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_BASECKTABULARPART_TYPE_PRESENT",
        ru: "Базовые виды расчета",
        en: "Baseline calculation types",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_DISPLCKTABULARPARTROW_TYPE_PRESENT",
        ru: "Вытесняющие виды расчета строка",
        en: "Overriding calculation types Row",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_DISPLCKTABULARPART_TYPE_PRESENT",
        ru: "Вытесняющие виды расчета",
        en: "Overriding calculation types",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_LEADINGCKTABULARPARTROW_TYPE_PRESENT",
        ru: "Ведущие виды расчета строка",
        en: "Primary calculation types Row",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_LEADINGCKTABULARPART_TYPE_PRESENT",
        ru: "Ведущие виды расчета",
        en: "Primary calculation types",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_LIST_TYPE_PRESENT",
        ru: "План видов расчета список",
        en: "Chart of calculation types List",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_MANAGER_TYPE_PRESENT",
        ru: "План видов расчета менеджер",
        en: "Chart of calculation types Manager",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_OBJ_TYPE_PRESENT",
        ru: "План видов расчета объект",
        en: "Chart of calculation types Object",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_REF_TYPE_PRESENT",
        ru: "План видов расчета ссылка",
        en: "Chart of calculation types Ref",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_SEL_TYPE_PRESENT",
        ru: "План видов расчета выборка",
        en: "Chart of calculation types Selected data",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_TABULARPARTROW_TYPE_PRESENT",
        ru: "План видов расчета табличная часть строка",
        en: "Chart of calculation types Tabular section Row",
    },
    L5Msg {
        key: "calc:IDS_CALCKIND_TABULARPART_TYPE_PRESENT",
        ru: "План видов расчета табличная часть",
        en: "Chart of calculation types Tabular section",
    },
    L5Msg {
        key: "calc:IDS_CALCREG_LIST_TYPE_PRESENT",
        ru: "Регистр расчета список",
        en: "Calculation register List",
    },
    L5Msg {
        key: "calc:IDS_CALCREG_MANAGER_TYPE_PRESENT",
        ru: "Регистр расчета менеджер",
        en: "Calculation register Manager",
    },
    L5Msg {
        key: "calc:IDS_CALCREG_RECCALCSMANAGER_TYPE_PRESENT",
        ru: "Регистр расчета перерасчеты",
        en: "Calculation register Recalculations",
    },
    L5Msg {
        key: "calc:IDS_CALCREG_RECORDKEY_TYPE_PRESENT",
        ru: "Регистр расчета ключ записи",
        en: "Calculation register Record key",
    },
    L5Msg {
        key: "calc:IDS_CALCREG_RECORDSET_TYPE_PRESENT",
        ru: "Регистр расчета набор записей",
        en: "Calculation register Record set",
    },
    L5Msg {
        key: "calc:IDS_CALCREG_RECORD_TYPE_PRESENT",
        ru: "Регистр расчета запись",
        en: "Calculation register Record",
    },
    L5Msg {
        key: "calc:IDS_CALCREG_SEL_TYPE_PRESENT",
        ru: "Регистр расчета выборка",
        en: "Calculation register Selected data",
    },
    L5Msg {
        key: "core85:IDS_ERROR_SAFEMODE",
        ru: "Установлен безопасный режим. Выполнение операции запрещено",
        en: "The operation is not allowed because the application is running in safe mode.",
    },
    L5Msg {
        key: "core85:IDS_LISTSTREAMERROR",
        ru: "Ошибка формата потока",
        en: "Stream format error",
    },
    L5Msg {
        key: "core85:IDS_PARAMETER_N_TYPE_MISMATCH",
        ru: "Несоответствие типов (параметр номер '%d')",
        en: "Type mismatch (parameter number '%d')",
    },
    L5Msg {
        key: "core85:IDS_PARAMETER_N_VALUE_MISMATCH",
        ru: "Недопустимое значение параметра (параметр номер '%d')",
        en: "Invalid parameter value (parameter number '%d')",
    },
    L5Msg {
        key: "core85:IDS_PROPERTY_TYPE_MISMATCH",
        ru: "Несоответствие типов",
        en: "Type mismatch",
    },
    L5Msg {
        key: "frntend:IDS_ERR_PREDEFINEDVALUE_NOTFOUND",
        ru: "Предопределенное значение \"%s\" не найдено.",
        en: "Predefined value \"%s\" not found.",
    },
    L5Msg {
        key: "xml2:IDS_XMLERR_DATACONVERSION",
        ru: "Ошибка преобразования данных XML",
        en: "Error transforming XML data",
    },
];

pub mod keys {
    pub const CANT_ORDER_BY_THIS_FIELD: &str = "backend:IDS_CANT_ORDER_BY_THIS_FIELD";
    pub const DB_LOCKING: &str = "backend:IDS_DBERROR_LOCKING";
    pub const DB_OBJECT_NOT_FOUND: &str = "backend:IDS_DBOBJECT_NOTFOUND";
    pub const ELEMENT_NOT_SELECTED: &str = "basic:IDS_ELEMENT_NOT_SELECTED";
    pub const INVALID_PROPERTY_NAME: &str = "backend:IDS_ERR_INVALID_PROPERTY_NAME";
    pub const LIST_STREAM_ERROR: &str = "core85:IDS_LISTSTREAMERROR";
    pub const MD_OBJECT_TYPE: &str = "backend:IDS_TYPE_PRESENTATION_MDCONTEXT";
    pub const NON_HIERARCHICAL: &str = "basic:IDS_CANT_PROCEE_FOR_NON_HIERACICAL";
    pub const PARAMETER_TYPE_MISMATCH: &str = "core85:IDS_PARAMETER_N_TYPE_MISMATCH";
    pub const PARAMETER_VALUE_MISMATCH: &str = "core85:IDS_PARAMETER_N_VALUE_MISMATCH";
    pub const PREDEFINED_ITEM_NOT_EXISTS: &str = "backend:IDS_PREDEFINED_ITEM_NOT_EXISTS";
    pub const PREDEFINED_ITEM_NOT_UNIQUE: &str = "backend:IDS_PREDEFINED_ITEM_NOT_UNIQUE";
    pub const PREDEFINED_NOT_IN_DATA: &str = "backend:IDS_PREDEFINEDOBJ_NOT_EXISTS";
    pub const PREDEFINED_VALUE_NOT_FOUND: &str = "frntend:IDS_ERR_PREDEFINEDVALUE_NOTFOUND";
    pub const PRES_ACCOUNTINGREGISTER_EXTDIMENSIONS: &str =
        "accnt:IDS_ACCNTREG_EXTDIMCOLLECTION_TYPE_PRESENT";
    pub const PRES_ACCOUNTINGREGISTER_LIST: &str = "accnt:IDS_ACCNTREG_LIST_TYPE_PRESENT";
    pub const PRES_ACCOUNTINGREGISTER_MANAGER: &str = "accnt:IDS_ACCNTREG_MANAGER_TYPE_PRESENT";
    pub const PRES_ACCOUNTINGREGISTER_RECORD: &str = "accnt:IDS_ACCNTREG_RECORD_TYPE_PRESENT";
    pub const PRES_ACCOUNTINGREGISTER_RECORDKEY: &str = "accnt:IDS_ACCNTREG_RECORDKEY_TYPE_PRESENT";
    pub const PRES_ACCOUNTINGREGISTER_RECORDSET: &str = "accnt:IDS_ACCNTREG_RECORDSET_TYPE_PRESENT";
    pub const PRES_ACCOUNTINGREGISTER_SELECTION: &str = "accnt:IDS_ACCNTREG_SEL_TYPE_PRESENT";
    pub const PRES_ACCUMULATIONREGISTER_LIST: &str = "basic:IDS_ACCUMREG_LIST_TYPE_PRESENT";
    pub const PRES_ACCUMULATIONREGISTER_MANAGER: &str = "basic:IDS_ACCUMREG_MANAGER_TYPE_PRESENT";
    pub const PRES_ACCUMULATIONREGISTER_RECORD: &str = "basic:IDS_ACCUMREG_RECORD_TYPE_PRESENT";
    pub const PRES_ACCUMULATIONREGISTER_RECORDKEY: &str =
        "basic:IDS_ACCUMREG_RECORDKEY_TYPE_PRESENT";
    pub const PRES_ACCUMULATIONREGISTER_RECORDSET: &str =
        "basic:IDS_ACCUMREG_RECORDSET_TYPE_PRESENT";
    pub const PRES_ACCUMULATIONREGISTER_SELECTION: &str = "basic:IDS_ACCUMREG_SEL_TYPE_PRESENT";
    pub const PRES_BUSINESSPROCESS_LIST: &str = "bp:IDS_BPPROCESS_LIST_TYPE_PRESENT";
    pub const PRES_BUSINESSPROCESS_MANAGER: &str = "bp:IDS_BPPROCESS_MANAGER_TYPE_PRESENT";
    pub const PRES_BUSINESSPROCESS_OBJECT: &str = "bp:IDS_BPPROCESS_OBJECT_TYPE_PRESENT";
    pub const PRES_BUSINESSPROCESS_REF: &str = "bp:IDS_BPPROCESS_REF_TYPE_PRESENT";
    pub const PRES_BUSINESSPROCESS_ROUTEPOINTREF: &str =
        "bp:IDS_BPPROCESSROUTEPOINT_REF_TYPE_PRESENT";
    pub const PRES_BUSINESSPROCESS_SELECTION: &str = "bp:IDS_BPPROCESS_SEL_TYPE_PRESENT";
    pub const PRES_BUSINESSPROCESS_TABULARSECTION: &str =
        "bp:IDS_BPPROCESS_TABULARPART_TYPE_PRESENT";
    pub const PRES_BUSINESSPROCESS_TABULARSECTIONROW: &str =
        "bp:IDS_BPPROCESS_TABULARPARTROW_TYPE_PRESENT";
    pub const PRES_CALCULATIONREGISTER_LIST: &str = "calc:IDS_CALCREG_LIST_TYPE_PRESENT";
    pub const PRES_CALCULATIONREGISTER_MANAGER: &str = "calc:IDS_CALCREG_MANAGER_TYPE_PRESENT";
    pub const PRES_CALCULATIONREGISTER_RECALCS: &str =
        "calc:IDS_CALCREG_RECCALCSMANAGER_TYPE_PRESENT";
    pub const PRES_CALCULATIONREGISTER_RECORD: &str = "calc:IDS_CALCREG_RECORD_TYPE_PRESENT";
    pub const PRES_CALCULATIONREGISTER_RECORDKEY: &str = "calc:IDS_CALCREG_RECORDKEY_TYPE_PRESENT";
    pub const PRES_CALCULATIONREGISTER_RECORDSET: &str = "calc:IDS_CALCREG_RECORDSET_TYPE_PRESENT";
    pub const PRES_CALCULATIONREGISTER_SELECTION: &str = "calc:IDS_CALCREG_SEL_TYPE_PRESENT";
    pub const PRES_CATALOG_LIST: &str = "basic:IDS_REFERENCE_LIST_TYPE_PRESENT";
    pub const PRES_CATALOG_MANAGER: &str = "basic:IDS_REFERENCE_MANAGER_TYPE_PRESENT";
    pub const PRES_CATALOG_OBJECT: &str = "basic:IDS_REFERENCES_OBJECT_TYPE_PRESENT";
    pub const PRES_CATALOG_REF: &str = "basic:IDS_REFERENCE_REF_TYPE_PRESENT";
    pub const PRES_CATALOG_SELECTION: &str = "basic:IDS_REFERENCE_SEL_TYPE_PRESENT";
    pub const PRES_CATALOG_TABULARSECTION: &str = "basic:IDS_REFERENCE_TABULARPART_TYPE_PRESENT";
    pub const PRES_CATALOG_TABULARSECTIONROW: &str =
        "basic:IDS_REFERENCE_TABULARPARTROW_TYPE_PRESENT";
    pub const PRES_CHARTOFACCOUNTS_EXTDIMENSIONTYPES: &str =
        "accnt:IDS_CHART_OF_ACCOUNTS_EXTDIM_TYPE_PRESENT";
    pub const PRES_CHARTOFACCOUNTS_EXTDIMENSIONTYPESROW: &str =
        "accnt:IDS_CHART_OF_ACCOUNTS_EXTDIMROW_TYPE_PRESENT";
    pub const PRES_CHARTOFACCOUNTS_LIST: &str = "accnt:IDS_CHART_OF_ACCOUNTS_LIST_TYPE_PRESENT";
    pub const PRES_CHARTOFACCOUNTS_MANAGER: &str =
        "accnt:IDS_CHART_OF_ACCOUNTS_MANAGER_TYPE_PRESENT";
    pub const PRES_CHARTOFACCOUNTS_OBJECT: &str = "accnt:IDS_CHART_OF_ACCOUNTS_OBJECT_TYPE_PRESENT";
    pub const PRES_CHARTOFACCOUNTS_REF: &str = "accnt:IDS_CHART_OF_ACCOUNTS_REF_TYPE_PRESENT";
    pub const PRES_CHARTOFACCOUNTS_SELECTION: &str = "accnt:IDS_CHART_OF_ACCOUNTS_SEL_TYPE_PRESENT";
    pub const PRES_CHARTOFACCOUNTS_TABULARSECTION: &str =
        "accnt:IDS_CHART_OF_ACCOUNTS_TABULARPART_TYPE_PRESENT";
    pub const PRES_CHARTOFACCOUNTS_TABULARSECTIONROW: &str =
        "accnt:IDS_CHART_OF_ACCOUNTS_TABULARPARTROW_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_BASECALCULATIONTYPES: &str =
        "calc:IDS_CALCKIND_BASECKTABULARPART_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_BASECALCULATIONTYPESROW: &str =
        "calc:IDS_CALCKIND_BASECKTABULARPARTROW_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_DISPLACINGCALCULATIONTYPES: &str =
        "calc:IDS_CALCKIND_DISPLCKTABULARPART_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_DISPLACINGCALCULATIONTYPESROW: &str =
        "calc:IDS_CALCKIND_DISPLCKTABULARPARTROW_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_LEADINGCALCULATIONTYPES: &str =
        "calc:IDS_CALCKIND_LEADINGCKTABULARPART_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_LEADINGCALCULATIONTYPESROW: &str =
        "calc:IDS_CALCKIND_LEADINGCKTABULARPARTROW_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_LIST: &str = "calc:IDS_CALCKIND_LIST_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_MANAGER: &str = "calc:IDS_CALCKIND_MANAGER_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_OBJECT: &str = "calc:IDS_CALCKIND_OBJ_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_REF: &str = "calc:IDS_CALCKIND_REF_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_SELECTION: &str = "calc:IDS_CALCKIND_SEL_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_TABULARSECTION: &str =
        "calc:IDS_CALCKIND_TABULARPART_TYPE_PRESENT";
    pub const PRES_CHARTOFCALCULATIONTYPES_TABULARSECTIONROW: &str =
        "calc:IDS_CALCKIND_TABULARPARTROW_TYPE_PRESENT";
    pub const PRES_CHARTOFCHARACTERISTICTYPES_LIST: &str =
        "basic:IDS_PLANOFCHARACTERISTICKIND_LIST_TYPE_PRESENT";
    pub const PRES_CHARTOFCHARACTERISTICTYPES_MANAGER: &str =
        "basic:IDS_PLANOFCHARACTERISTICKIND_MANAGER_TYPE_PRESENT";
    pub const PRES_CHARTOFCHARACTERISTICTYPES_OBJECT: &str =
        "basic:IDS_PLANOFCHARACTERISTICKIND_OBJECT_TYPE_PRESENT";
    pub const PRES_CHARTOFCHARACTERISTICTYPES_REF: &str =
        "basic:IDS_PLANOFCHARACTERISTICKIND_REF_TYPE_PRESENT";
    pub const PRES_CHARTOFCHARACTERISTICTYPES_SELECTION: &str =
        "basic:IDS_PLANOFCHARACTERISTICKIND_SEL_TYPE_PRESENT";
    pub const PRES_CHARTOFCHARACTERISTICTYPES_TABULARSECTION: &str =
        "basic:IDS_PLANOFCHARACTERISTICKIND_TABULARPART_TYPE_PRESENT";
    pub const PRES_CHARTOFCHARACTERISTICTYPES_TABULARSECTIONROW: &str =
        "basic:IDS_PLANOFCHARACTERISTICKIND_TABULARPARTROW_TYPE_PRESENT";
    pub const PRES_CONSTANT_MANAGER: &str = "basic:IDS_CONST_MANAGER_TYPE_PRESENT";
    pub const PRES_CONSTANT_VALUEKEY: &str = "basic:IDS_CONST_VALUEKEY_TYPE_PRESENT";
    pub const PRES_CONSTANT_VALUEMANAGER: &str = "basic:IDS_CONST_VALUEMANAGER_TYPE_PRESENT";
    pub const PRES_DATAPROCESSOR_MANAGER: &str = "basic:IDS_PROCESSING_MANAGER_TYPE_PRESENT";
    pub const PRES_DATAPROCESSOR_OBJECT: &str = "basic:IDS_PROCESSING_OBJECT_TYPE_PRESENT";
    pub const PRES_DATAPROCESSOR_TABULARSECTION: &str =
        "basic:IDS_PROCESSING_TABULARPART_TYPE_PRESENT";
    pub const PRES_DATAPROCESSOR_TABULARSECTIONROW: &str =
        "basic:IDS_PROCESSING_TABULARPARTROW_TYPE_PRESENT";
    pub const PRES_DOCUMENTJOURNAL_LIST: &str = "basic:IDS_DOCJOURNAL_LIST_TYPE_PRESENT";
    pub const PRES_DOCUMENTJOURNAL_MANAGER: &str = "basic:IDS_DOCJOURNAL_MANAGER_TYPE_PRESENT";
    pub const PRES_DOCUMENTJOURNAL_SELECTION: &str = "basic:IDS_DOCJOURNAL_SEL_TYPE_PRESENT";
    pub const PRES_DOCUMENT_LIST: &str = "basic:IDS_DOCUMENT_LIST_TYPE_PRESENT";
    pub const PRES_DOCUMENT_MANAGER: &str = "basic:IDS_DOCUMENT_MANAGER_TYPE_PRESENT";
    pub const PRES_DOCUMENT_OBJECT: &str = "basic:IDS_DOCUMENT_OBJECT_TYPE_PRESENT";
    pub const PRES_DOCUMENT_REF: &str = "basic:IDS_DOCUMENT_REF_TYPE_PRESENT";
    pub const PRES_DOCUMENT_SELECTION: &str = "basic:IDS_DOCUMENT_SEL_TYPE_PRESENT";
    pub const PRES_DOCUMENT_TABULARSECTION: &str = "basic:IDS_DOCUMENT_TABULARPART_TYPE_PRESENT";
    pub const PRES_DOCUMENT_TABULARSECTIONROW: &str =
        "basic:IDS_DOCUMENT_TABULARPARTROW_TYPE_PRESENT";
    pub const PRES_ENUM_LIST: &str = "basic:IDS_ENUM_LIST_TYPE_PRESENT";
    pub const PRES_ENUM_MANAGER: &str = "basic:IDS_ENUM_MANAGER_TYPE_PRESENT";
    pub const PRES_ENUM_REF: &str = "basic:IDS_ENUM_REF_TYPE_PRESENT";
    pub const PRES_EXCHANGEPLAN_LIST: &str = "backend:IDS_DATAEXCHANGEENDPOINT_LIST_TYPE_PRESENT";
    pub const PRES_EXCHANGEPLAN_MANAGER: &str =
        "backend:IDS_DATAEXCHANGEENDPOINT_MANAGER_TYPE_PRESENT";
    pub const PRES_EXCHANGEPLAN_OBJECT: &str =
        "backend:IDS_DATAEXCHANGEENDPOINTS_OBJECT_TYPE_PRESENT";
    pub const PRES_EXCHANGEPLAN_REF: &str = "backend:IDS_DATAEXCHANGEENDPOINT_REF_TYPE_PRESENT";
    pub const PRES_EXCHANGEPLAN_SELECTION: &str =
        "backend:IDS_DATAEXCHANGEENDPOINT_SEL_TYPE_PRESENT";
    pub const PRES_EXCHANGEPLAN_TABULARSECTION: &str =
        "backend:IDS_DATAEXCHANGEENDPOINT_TABULARPART_TYPE_PRESENT";
    pub const PRES_EXCHANGEPLAN_TABULARSECTIONROW: &str =
        "backend:IDS_DATAEXCHANGEENDPOINT_TABULARPARTROW_TYPE_PRESENT";
    pub const PRES_INFORMATIONREGISTER_LIST: &str = "basic:IDS_INFOREG_LIST_TYPE_PRESENT";
    pub const PRES_INFORMATIONREGISTER_MANAGER: &str = "basic:IDS_INFOREG_MANAGER_TYPE_PRESENT";
    pub const PRES_INFORMATIONREGISTER_RECORD: &str = "basic:IDS_INFOREG_RECORD_TYPE_PRESENT";
    pub const PRES_INFORMATIONREGISTER_RECORDKEY: &str = "basic:IDS_INFOREG_RECORDKEY_TYPE_PRESENT";
    pub const PRES_INFORMATIONREGISTER_RECORDMANAGER: &str =
        "basic:IDS_INFOREG_RECORDMANAGER_TYPE_PRESENT";
    pub const PRES_INFORMATIONREGISTER_RECORDSET: &str = "basic:IDS_INFOREG_RECORDSET_TYPE_PRESENT";
    pub const PRES_INFORMATIONREGISTER_SELECTION: &str = "basic:IDS_INFOREG_SEL_TYPE_PRESENT";
    pub const PRES_REPORT_MANAGER: &str = "basic:IDS_REPORT_MANAGER_TYPE_PRESENT";
    pub const PRES_REPORT_OBJECT: &str = "basic:IDS_REPORT_OBJECT_TYPE_PRESENT";
    pub const PRES_REPORT_TABULARSECTION: &str = "basic:IDS_REPORT_TABULARPART_TYPE_PRESENT";
    pub const PRES_REPORT_TABULARSECTIONROW: &str = "basic:IDS_REPORT_TABULARPARTROW_TYPE_PRESENT";
    pub const PRES_SEQUENCE_MANAGER: &str = "basic:IDS_SEQUENCE_MANAGER_TYPE_PRESENT";
    pub const PRES_SEQUENCE_RECORD: &str = "basic:IDS_SEQUENCE_RECORD_TYPE_PRESENT";
    pub const PRES_SEQUENCE_RECORDSET: &str = "basic:IDS_SEQUENCE_RECORDSET_TYPE_PRESENT";
    pub const PRES_TASK_LIST: &str = "bp:IDS_BPTASK_LIST_TYPE_PRESENT";
    pub const PRES_TASK_MANAGER: &str = "bp:IDS_BPTASK_MANAGER_TYPE_PRESENT";
    pub const PRES_TASK_OBJECT: &str = "bp:IDS_BPTASK_OBJECT_TYPE_PRESENT";
    pub const PRES_TASK_REF: &str = "bp:IDS_BPTASK_REF_TYPE_PRESENT";
    pub const PRES_TASK_SELECTION: &str = "bp:IDS_BPTASK_SEL_TYPE_PRESENT";
    pub const PRES_TASK_TABULARSECTION: &str = "bp:IDS_BPTASK_TABULARPART_TYPE_PRESENT";
    pub const PRES_TASK_TABULARSECTIONROW: &str = "bp:IDS_BPTASK_TABULARPARTROW_TYPE_PRESENT";
    pub const SAFE_MODE: &str = "core85:IDS_ERROR_SAFEMODE";
    pub const SESSION_PARAMS_TYPE: &str = "backend:IDS_SESSIONPARAMS_MANAGER_TYPE_PRESENT";
    pub const SESSION_PARAM_NOT_INITIALIZED: &str = "backend:IDS_SESSION_PARAM_NOT_INITIALIZED";
    pub const TYPE_MISMATCH: &str = "core85:IDS_PROPERTY_TYPE_MISMATCH";
    pub const WRONG_ATTRIBUTE: &str = "basic:IDS_WRONG_PARAMETER_FLDNAME";
    pub const XML_DATA_CONVERSION: &str = "xml2:IDS_XMLERR_DATACONVERSION";
}

pub(crate) static TYPE_PRESENTATIONS: &[(&str, &str, &str)] = &[
    (
        "AccountingRegister",
        "ExtDimensions",
        "accnt:IDS_ACCNTREG_EXTDIMCOLLECTION_TYPE_PRESENT",
    ),
    (
        "AccountingRegister",
        "List",
        "accnt:IDS_ACCNTREG_LIST_TYPE_PRESENT",
    ),
    (
        "AccountingRegister",
        "Manager",
        "accnt:IDS_ACCNTREG_MANAGER_TYPE_PRESENT",
    ),
    (
        "AccountingRegister",
        "Record",
        "accnt:IDS_ACCNTREG_RECORD_TYPE_PRESENT",
    ),
    (
        "AccountingRegister",
        "RecordKey",
        "accnt:IDS_ACCNTREG_RECORDKEY_TYPE_PRESENT",
    ),
    (
        "AccountingRegister",
        "RecordSet",
        "accnt:IDS_ACCNTREG_RECORDSET_TYPE_PRESENT",
    ),
    (
        "AccountingRegister",
        "Selection",
        "accnt:IDS_ACCNTREG_SEL_TYPE_PRESENT",
    ),
    (
        "AccumulationRegister",
        "List",
        "basic:IDS_ACCUMREG_LIST_TYPE_PRESENT",
    ),
    (
        "AccumulationRegister",
        "Manager",
        "basic:IDS_ACCUMREG_MANAGER_TYPE_PRESENT",
    ),
    (
        "AccumulationRegister",
        "Record",
        "basic:IDS_ACCUMREG_RECORD_TYPE_PRESENT",
    ),
    (
        "AccumulationRegister",
        "RecordKey",
        "basic:IDS_ACCUMREG_RECORDKEY_TYPE_PRESENT",
    ),
    (
        "AccumulationRegister",
        "RecordSet",
        "basic:IDS_ACCUMREG_RECORDSET_TYPE_PRESENT",
    ),
    (
        "AccumulationRegister",
        "Selection",
        "basic:IDS_ACCUMREG_SEL_TYPE_PRESENT",
    ),
    (
        "BusinessProcess",
        "List",
        "bp:IDS_BPPROCESS_LIST_TYPE_PRESENT",
    ),
    (
        "BusinessProcess",
        "Manager",
        "bp:IDS_BPPROCESS_MANAGER_TYPE_PRESENT",
    ),
    (
        "BusinessProcess",
        "Object",
        "bp:IDS_BPPROCESS_OBJECT_TYPE_PRESENT",
    ),
    (
        "BusinessProcess",
        "Ref",
        "bp:IDS_BPPROCESS_REF_TYPE_PRESENT",
    ),
    (
        "BusinessProcess",
        "RoutePointRef",
        "bp:IDS_BPPROCESSROUTEPOINT_REF_TYPE_PRESENT",
    ),
    (
        "BusinessProcess",
        "Selection",
        "bp:IDS_BPPROCESS_SEL_TYPE_PRESENT",
    ),
    (
        "BusinessProcess",
        "TabularSection",
        "bp:IDS_BPPROCESS_TABULARPART_TYPE_PRESENT",
    ),
    (
        "BusinessProcess",
        "TabularSectionRow",
        "bp:IDS_BPPROCESS_TABULARPARTROW_TYPE_PRESENT",
    ),
    (
        "CalculationRegister",
        "List",
        "calc:IDS_CALCREG_LIST_TYPE_PRESENT",
    ),
    (
        "CalculationRegister",
        "Manager",
        "calc:IDS_CALCREG_MANAGER_TYPE_PRESENT",
    ),
    (
        "CalculationRegister",
        "Recalcs",
        "calc:IDS_CALCREG_RECCALCSMANAGER_TYPE_PRESENT",
    ),
    (
        "CalculationRegister",
        "Record",
        "calc:IDS_CALCREG_RECORD_TYPE_PRESENT",
    ),
    (
        "CalculationRegister",
        "RecordKey",
        "calc:IDS_CALCREG_RECORDKEY_TYPE_PRESENT",
    ),
    (
        "CalculationRegister",
        "RecordSet",
        "calc:IDS_CALCREG_RECORDSET_TYPE_PRESENT",
    ),
    (
        "CalculationRegister",
        "Selection",
        "calc:IDS_CALCREG_SEL_TYPE_PRESENT",
    ),
    ("Catalog", "List", "basic:IDS_REFERENCE_LIST_TYPE_PRESENT"),
    (
        "Catalog",
        "Manager",
        "basic:IDS_REFERENCE_MANAGER_TYPE_PRESENT",
    ),
    (
        "Catalog",
        "Object",
        "basic:IDS_REFERENCES_OBJECT_TYPE_PRESENT",
    ),
    ("Catalog", "Ref", "basic:IDS_REFERENCE_REF_TYPE_PRESENT"),
    (
        "Catalog",
        "Selection",
        "basic:IDS_REFERENCE_SEL_TYPE_PRESENT",
    ),
    (
        "Catalog",
        "TabularSection",
        "basic:IDS_REFERENCE_TABULARPART_TYPE_PRESENT",
    ),
    (
        "Catalog",
        "TabularSectionRow",
        "basic:IDS_REFERENCE_TABULARPARTROW_TYPE_PRESENT",
    ),
    (
        "ChartOfAccounts",
        "ExtDimensionTypes",
        "accnt:IDS_CHART_OF_ACCOUNTS_EXTDIM_TYPE_PRESENT",
    ),
    (
        "ChartOfAccounts",
        "ExtDimensionTypesRow",
        "accnt:IDS_CHART_OF_ACCOUNTS_EXTDIMROW_TYPE_PRESENT",
    ),
    (
        "ChartOfAccounts",
        "List",
        "accnt:IDS_CHART_OF_ACCOUNTS_LIST_TYPE_PRESENT",
    ),
    (
        "ChartOfAccounts",
        "Manager",
        "accnt:IDS_CHART_OF_ACCOUNTS_MANAGER_TYPE_PRESENT",
    ),
    (
        "ChartOfAccounts",
        "Object",
        "accnt:IDS_CHART_OF_ACCOUNTS_OBJECT_TYPE_PRESENT",
    ),
    (
        "ChartOfAccounts",
        "Ref",
        "accnt:IDS_CHART_OF_ACCOUNTS_REF_TYPE_PRESENT",
    ),
    (
        "ChartOfAccounts",
        "Selection",
        "accnt:IDS_CHART_OF_ACCOUNTS_SEL_TYPE_PRESENT",
    ),
    (
        "ChartOfAccounts",
        "TabularSection",
        "accnt:IDS_CHART_OF_ACCOUNTS_TABULARPART_TYPE_PRESENT",
    ),
    (
        "ChartOfAccounts",
        "TabularSectionRow",
        "accnt:IDS_CHART_OF_ACCOUNTS_TABULARPARTROW_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "BaseCalculationTypes",
        "calc:IDS_CALCKIND_BASECKTABULARPART_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "BaseCalculationTypesRow",
        "calc:IDS_CALCKIND_BASECKTABULARPARTROW_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "DisplacingCalculationTypes",
        "calc:IDS_CALCKIND_DISPLCKTABULARPART_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "DisplacingCalculationTypesRow",
        "calc:IDS_CALCKIND_DISPLCKTABULARPARTROW_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "LeadingCalculationTypes",
        "calc:IDS_CALCKIND_LEADINGCKTABULARPART_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "LeadingCalculationTypesRow",
        "calc:IDS_CALCKIND_LEADINGCKTABULARPARTROW_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "List",
        "calc:IDS_CALCKIND_LIST_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "Manager",
        "calc:IDS_CALCKIND_MANAGER_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "Object",
        "calc:IDS_CALCKIND_OBJ_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "Ref",
        "calc:IDS_CALCKIND_REF_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "Selection",
        "calc:IDS_CALCKIND_SEL_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "TabularSection",
        "calc:IDS_CALCKIND_TABULARPART_TYPE_PRESENT",
    ),
    (
        "ChartOfCalculationTypes",
        "TabularSectionRow",
        "calc:IDS_CALCKIND_TABULARPARTROW_TYPE_PRESENT",
    ),
    (
        "ChartOfCharacteristicTypes",
        "List",
        "basic:IDS_PLANOFCHARACTERISTICKIND_LIST_TYPE_PRESENT",
    ),
    (
        "ChartOfCharacteristicTypes",
        "Manager",
        "basic:IDS_PLANOFCHARACTERISTICKIND_MANAGER_TYPE_PRESENT",
    ),
    (
        "ChartOfCharacteristicTypes",
        "Object",
        "basic:IDS_PLANOFCHARACTERISTICKIND_OBJECT_TYPE_PRESENT",
    ),
    (
        "ChartOfCharacteristicTypes",
        "Ref",
        "basic:IDS_PLANOFCHARACTERISTICKIND_REF_TYPE_PRESENT",
    ),
    (
        "ChartOfCharacteristicTypes",
        "Selection",
        "basic:IDS_PLANOFCHARACTERISTICKIND_SEL_TYPE_PRESENT",
    ),
    (
        "ChartOfCharacteristicTypes",
        "TabularSection",
        "basic:IDS_PLANOFCHARACTERISTICKIND_TABULARPART_TYPE_PRESENT",
    ),
    (
        "ChartOfCharacteristicTypes",
        "TabularSectionRow",
        "basic:IDS_PLANOFCHARACTERISTICKIND_TABULARPARTROW_TYPE_PRESENT",
    ),
    (
        "Constant",
        "Manager",
        "basic:IDS_CONST_MANAGER_TYPE_PRESENT",
    ),
    (
        "Constant",
        "ValueKey",
        "basic:IDS_CONST_VALUEKEY_TYPE_PRESENT",
    ),
    (
        "Constant",
        "ValueManager",
        "basic:IDS_CONST_VALUEMANAGER_TYPE_PRESENT",
    ),
    (
        "DataProcessor",
        "Manager",
        "basic:IDS_PROCESSING_MANAGER_TYPE_PRESENT",
    ),
    (
        "DataProcessor",
        "Object",
        "basic:IDS_PROCESSING_OBJECT_TYPE_PRESENT",
    ),
    (
        "DataProcessor",
        "TabularSection",
        "basic:IDS_PROCESSING_TABULARPART_TYPE_PRESENT",
    ),
    (
        "DataProcessor",
        "TabularSectionRow",
        "basic:IDS_PROCESSING_TABULARPARTROW_TYPE_PRESENT",
    ),
    ("Document", "List", "basic:IDS_DOCUMENT_LIST_TYPE_PRESENT"),
    (
        "Document",
        "Manager",
        "basic:IDS_DOCUMENT_MANAGER_TYPE_PRESENT",
    ),
    (
        "Document",
        "Object",
        "basic:IDS_DOCUMENT_OBJECT_TYPE_PRESENT",
    ),
    ("Document", "Ref", "basic:IDS_DOCUMENT_REF_TYPE_PRESENT"),
    (
        "Document",
        "Selection",
        "basic:IDS_DOCUMENT_SEL_TYPE_PRESENT",
    ),
    (
        "Document",
        "TabularSection",
        "basic:IDS_DOCUMENT_TABULARPART_TYPE_PRESENT",
    ),
    (
        "Document",
        "TabularSectionRow",
        "basic:IDS_DOCUMENT_TABULARPARTROW_TYPE_PRESENT",
    ),
    (
        "DocumentJournal",
        "List",
        "basic:IDS_DOCJOURNAL_LIST_TYPE_PRESENT",
    ),
    (
        "DocumentJournal",
        "Manager",
        "basic:IDS_DOCJOURNAL_MANAGER_TYPE_PRESENT",
    ),
    (
        "DocumentJournal",
        "Selection",
        "basic:IDS_DOCJOURNAL_SEL_TYPE_PRESENT",
    ),
    ("Enum", "List", "basic:IDS_ENUM_LIST_TYPE_PRESENT"),
    ("Enum", "Manager", "basic:IDS_ENUM_MANAGER_TYPE_PRESENT"),
    ("Enum", "Ref", "basic:IDS_ENUM_REF_TYPE_PRESENT"),
    (
        "ExchangePlan",
        "List",
        "backend:IDS_DATAEXCHANGEENDPOINT_LIST_TYPE_PRESENT",
    ),
    (
        "ExchangePlan",
        "Manager",
        "backend:IDS_DATAEXCHANGEENDPOINT_MANAGER_TYPE_PRESENT",
    ),
    (
        "ExchangePlan",
        "Object",
        "backend:IDS_DATAEXCHANGEENDPOINTS_OBJECT_TYPE_PRESENT",
    ),
    (
        "ExchangePlan",
        "Ref",
        "backend:IDS_DATAEXCHANGEENDPOINT_REF_TYPE_PRESENT",
    ),
    (
        "ExchangePlan",
        "Selection",
        "backend:IDS_DATAEXCHANGEENDPOINT_SEL_TYPE_PRESENT",
    ),
    (
        "ExchangePlan",
        "TabularSection",
        "backend:IDS_DATAEXCHANGEENDPOINT_TABULARPART_TYPE_PRESENT",
    ),
    (
        "ExchangePlan",
        "TabularSectionRow",
        "backend:IDS_DATAEXCHANGEENDPOINT_TABULARPARTROW_TYPE_PRESENT",
    ),
    (
        "InformationRegister",
        "List",
        "basic:IDS_INFOREG_LIST_TYPE_PRESENT",
    ),
    (
        "InformationRegister",
        "Manager",
        "basic:IDS_INFOREG_MANAGER_TYPE_PRESENT",
    ),
    (
        "InformationRegister",
        "Record",
        "basic:IDS_INFOREG_RECORD_TYPE_PRESENT",
    ),
    (
        "InformationRegister",
        "RecordKey",
        "basic:IDS_INFOREG_RECORDKEY_TYPE_PRESENT",
    ),
    (
        "InformationRegister",
        "RecordManager",
        "basic:IDS_INFOREG_RECORDMANAGER_TYPE_PRESENT",
    ),
    (
        "InformationRegister",
        "RecordSet",
        "basic:IDS_INFOREG_RECORDSET_TYPE_PRESENT",
    ),
    (
        "InformationRegister",
        "Selection",
        "basic:IDS_INFOREG_SEL_TYPE_PRESENT",
    ),
    ("Report", "Manager", "basic:IDS_REPORT_MANAGER_TYPE_PRESENT"),
    ("Report", "Object", "basic:IDS_REPORT_OBJECT_TYPE_PRESENT"),
    (
        "Report",
        "TabularSection",
        "basic:IDS_REPORT_TABULARPART_TYPE_PRESENT",
    ),
    (
        "Report",
        "TabularSectionRow",
        "basic:IDS_REPORT_TABULARPARTROW_TYPE_PRESENT",
    ),
    (
        "Sequence",
        "Manager",
        "basic:IDS_SEQUENCE_MANAGER_TYPE_PRESENT",
    ),
    (
        "Sequence",
        "Record",
        "basic:IDS_SEQUENCE_RECORD_TYPE_PRESENT",
    ),
    (
        "Sequence",
        "RecordSet",
        "basic:IDS_SEQUENCE_RECORDSET_TYPE_PRESENT",
    ),
    ("Task", "List", "bp:IDS_BPTASK_LIST_TYPE_PRESENT"),
    ("Task", "Manager", "bp:IDS_BPTASK_MANAGER_TYPE_PRESENT"),
    ("Task", "Object", "bp:IDS_BPTASK_OBJECT_TYPE_PRESENT"),
    ("Task", "Ref", "bp:IDS_BPTASK_REF_TYPE_PRESENT"),
    ("Task", "Selection", "bp:IDS_BPTASK_SEL_TYPE_PRESENT"),
    (
        "Task",
        "TabularSection",
        "bp:IDS_BPTASK_TABULARPART_TYPE_PRESENT",
    ),
    (
        "Task",
        "TabularSectionRow",
        "bp:IDS_BPTASK_TABULARPARTROW_TYPE_PRESENT",
    ),
];
