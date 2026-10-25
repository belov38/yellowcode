import { el } from "./dom.js";
import { iconUrl } from "./api.js";

export function icon(key) {
  if (!key) return el("span", { class: "ico generic" });
  const url = iconUrl(key);
  return el("img", { class: "ico", src: url, srcset: iconUrl(key, "2x=1") + " 2x", alt: "", width: 16, height: 16 });
}

const KINDS_RU = {
  Конфигурация: "Configuration", Подсистема: "Subsystem", ОбщийМодуль: "CommonModule", ПараметрСеанса: "SessionParameter",
  Роль: "Role", ОбщийРеквизит: "CommonAttribute", ПланОбмена: "ExchangePlan", КритерийОтбора: "FilterCriterion",
  ПодпискаНаСобытие: "EventSubscription", РегламентноеЗадание: "ScheduledJob", ФункциональнаяОпция: "FunctionalOption",
  ПараметрФункциональныхОпций: "FunctionalOptionsParameter", ОпределяемыйТип: "DefinedType",
  ХранилищеНастроек: "SettingsStorage", ОбщаяФорма: "CommonForm", ОбщаяКоманда: "CommonCommand",
  ГруппаКоманд: "CommandGroup", ОбщийМакет: "CommonTemplate", ОбщаяКартинка: "CommonPicture", ПакетXDTO: "XDTOPackage",
  WebСервис: "WebService", HTTPСервис: "HTTPService", WSСсылка: "WSReference", ЭлементСтиля: "StyleItem", Стиль: "Style",
  Язык: "Language", Константа: "Constant", Справочник: "Catalog", Документ: "Document",
  Нумератор: "DocumentNumerator", Последовательность: "Sequence", ЖурналДокументов: "DocumentJournal",
  Перечисление: "Enum", Отчет: "Report", Обработка: "DataProcessor", ПланВидовХарактеристик: "ChartOfCharacteristicTypes",
  ПланСчетов: "ChartOfAccounts", ПланВидовРасчета: "ChartOfCalculationTypes", РегистрСведений: "InformationRegister",
  РегистрНакопления: "AccumulationRegister", РегистрБухгалтерии: "AccountingRegister", РегистрРасчета: "CalculationRegister",
  БизнесПроцесс: "BusinessProcess", Задача: "Task", ВнешнийИсточникДанных: "ExternalDataSource",
  СервисИнтеграции: "IntegrationService", Бот: "Bot", Интерфейс: "Interface", WebSocketКлиент: "WebSocketClient",
  ЦветПалитры: "PaletteColor",
};
const QUERY_TABLES = { Константы: "Constant", Constants: "Constant" };
const KINDS_EN = new Set(Object.values(KINDS_RU));

export function kindIconKey(kind) {
  const en = KINDS_RU[kind] || QUERY_TABLES[kind] || kind;
  return KINDS_EN.has(en) ? "object:" + en : null;
}
