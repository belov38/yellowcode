import { el, fill } from "./dom.js";
import { api, show } from "./api.js";
import { openWindow } from "./tabs.js";
import { T } from "./i18n.js";
import { metadataSection, reverseReferences, informationRegistersForPosting } from "./metadata_data.js";
import { metadataWidgets } from "./metadata_widgets.js";

export function metadataTab(tab, path, allTabs, itemNode) {
  const root = el("div", { class: "metadata-layout" });
  const items = tab.items || [];
  const props = new Map(allTabs.flatMap((t) => t.items || []).filter((it) => it.name).map((it) => [it.name, it]));
  const en = document.documentElement.lang === "en";
  const tr = (ru, english) => en ? english : ru;
  const get = (name) => props.get(name);
  const used = new Set();
  const kind = path.split(".")[0];
  const { short, field: renderField, command, caption, row, sheet, radioGroup, checkRow, treeGroup, standardAttributes } = metadataWidgets(get);
  const field = (name, label) => { used.add(name); return renderField(name, label); };
  const list = (kind, title) => {
    const source = items.find((it) => it.list?.kind === kind)?.list;
    const titles = { Attribute: tr("Реквизиты", "Attributes"), TabularSection: tr("Табличные части", "Tabular sections"), Form: tr("Формы", "Forms"), Column: tr("Графы", "Columns"), EnumValue: tr("Значения", "Values"), Command: tr("Команды", "Commands"), Template: tr("Макеты", "Templates") };
    const body = sheet(title || titles[kind] || source?.title || "", (source?.items || []).map((it) => row(it, 1)), true);
    if (kind === "TabularSection") {
      root.addEventListener("object-tab-open", () => {
        for (const it of source?.items || []) api("object", { path: it.path }).then((d) => {
          if (!root.isConnected) return;
          const parent = [...body.querySelectorAll("[data-path]")].find((n) => n.dataset.path === it.path);
          if (parent) parent.after(...d.tabs.flatMap((t) => t.items || []).filter((i) => i.list?.kind === "Attribute").flatMap((i) => i.list.items.map((child) => row(child, 2))));
        }).catch((e) => body.appendChild(el("div", { class: "msg bad" }, e.message)));
      }, { once: true });
    }
    return body;
  };
  const referenceList = (name, title) => {
    used.add(name);
    const p = get(name);
    return sheet(title, (p?.value?.refs || p?.value?.fields || []).slice().sort((a, b) => short(a).localeCompare(short(b), en ? "en" : "ru")).map((ref) => row({ name: short(ref), path: ref, icon: "object:" + ref.split(".")[0] })), false);
  };
  const memberships = (kind, property, title) => {
    const body = sheet(title, []);
    const content = body.querySelector(".metadata-sheet-body");
    content.setAttribute("aria-busy", "true");
    root.addEventListener("object-tab-open", async () => {
      try {
        const nodes = await metadataSection(kind);
        const rows = [];
        for (let offset = 0; offset < nodes.length; offset += 8) {
          if (!root.isConnected) return;
          const batch = await Promise.all(nodes.slice(offset, offset + 8).map(async (node) => {
            const d = await show(node.path);
            const p = d.properties.find((v) => v.name === property);
            const checked = (p?.value?.refs || []).includes(path);
            return el("label", { class: "metadata-membership", style: "padding-left:" + (4 + node.depth * 14) + "px", "data-path": node.path },
              el("input", { type: "checkbox", disabled: true, checked }), el("span", null, node.label));
          }));
          rows.push(...batch);
        }
        if (root.isConnected) fill(content, rows);
      } catch (e) { fill(content, el("div", { class: "msg bad" }, e.message)); }
      finally { content.removeAttribute("aria-busy"); }
    }, { once: true });
    return body;
  };
  const title = tab.title;
  root.dataset.section = title;
  let roleSeq = 0;
  if (["Подсистемы", "Subsystems"].includes(title)) {
    fill(root, el("div", { class: "metadata-heading" }, items[0]?.caption), memberships("Subsystem", "Content"));
  } else if (["Функциональные опции", "Functional options"].includes(title)) {
    fill(root, el("div", { class: "metadata-heading" }, items[0]?.caption), memberships("FunctionalOption", "Content"));
  } else if (["Иерархия", "Hierarchy"].includes(title)) {
    fill(root, field("Hierarchical"), field("HierarchyType", tr("Вид иерархии", "Hierarchy type")), field("FoldersOnTop"), field("LimitLevelCount"), field("LevelCount"));
  } else if (["Владельцы", "Owners"].includes(title)) {
    fill(root, referenceList("Owners", tr("Список владельцев справочника", "Catalog owners")), field("SubordinationUse"));
  } else if (["Данные", "Data"].includes(title)) {
    root.classList.add("metadata-data");
    if (kind === "ChartOfCharacteristicTypes") root.classList.add("metadata-characteristic-data");
    const commands = el("div", { class: "metadata-command-grid" },
      command(tr("Стандартные реквизиты", "Standard attributes"), get("StandardAttributes") ? () => standardAttributes(get("StandardAttributes")) : null),
      get("Characteristics") ? command(tr("Характеристики", "Characteristics")) : null,
      ...items.filter((it) => it.caption && /^(BtnCommonProperties|BtnAdditionalIndexes)$/.test(it.control)).map(caption));
    used.add("Characteristics"); used.add("StandardAttributes");
    if (kind === "DocumentJournal") {
      fill(root, referenceList("RegisteredDocuments", tr("Регистрируемые документы", "Registered documents")), list("Column"), commands);
    } else if (kind === "Enum") {
      fill(root, el("div", { class: "metadata-heading" }, tr("Значения перечисления", "Enumeration values")), list("EnumValue"), commands);
    } else if (kind === "InformationRegister") {
      const trees = items.filter((it) => it.list).map((it) => treeGroup(it.list.title, it.list.items.map((child) => row(child, 1))));
      fill(root, sheet(null, trees, true), commands);
    } else {
      const top = el("div", { class: "metadata-data-options" }, field("CodeLength"), field("DescriptionLength"));
      if (get("CodeType")) { used.add("CodeType"); top.append(radioGroup(tr("Тип кода", "Code type"), get("CodeType"), [["Number", tr("Число", "Number")], ["String", tr("Строка", "String")]])); }
      if (get("DefaultPresentation")) { used.add("DefaultPresentation"); top.append(radioGroup(tr("Основное представление", "Default presentation"), get("DefaultPresentation"), [["AsCode", tr("В виде кода", "Code")], ["AsDescription", tr("В виде наименования", "Description")]])); }
      fill(root, top.childNodes.length ? top : null, list("Attribute"), list("TabularSection"), commands);
    }
  } else if (["Нумерация", "Numbering"].includes(title)) {
    root.classList.add("metadata-numbering");
    if (kind === "Document") {
      used.add("NumberType");
      fill(root, field("Autonumbering"), field("Numerator"), field("NumberLength"), field("CheckUnique"),
        radioGroup(tr("Тип номера", "Number type"), get("NumberType"), [["Number", tr("Число", "Number")], ["String", tr("Строка", "String")]]),
        field("NumberPeriodicity", tr("Периодичность", "Periodicity")));
    } else {
      used.add("CodeSeries");
      const labels = items.filter((it) => /^RadioCodesSeries/.test(it.control || "")).map((it) => it.caption);
      fill(root, field("Autonumbering"), field("CheckUnique"), radioGroup(tr("Серии кодов", "Code series"), get("CodeSeries"),
        [({ Catalog: "WholeCatalog", ChartOfAccounts: "WholeChartOfAccounts" }[kind] || "WholeCharacteristicKind"), "WithinSubordination", "WithinOwnerSubordination"].slice(0, labels.length).map((value, i) => [value, labels[i]])));
    }
  } else if (["Движения", "Posting"].includes(title)) {
    const registers = sheet(null, []); registers.classList.add("metadata-register-tree");
    const content = registers.querySelector(".metadata-sheet-body"); content.setAttribute("aria-busy", "true");
    fill(root, field("Posting"), field("RealTimePosting"), field("RegisterRecordsDeletion"), registers,
      ...items.filter((it) => it.caption).map(caption), referenceList("RegisterRecords", tr("Движения", "Register records")));
    root.addEventListener("object-tab-open", async () => {
      try {
        const selected = new Set(get("RegisterRecords")?.value?.refs || []);
        const kinds = [["InformationRegister", "Регистры сведений", "Information registers"], ["AccumulationRegister", "Регистры накопления", "Accumulation registers"], ["AccountingRegister", "Регистры бухгалтерии", "Accounting registers"], ["CalculationRegister", "Регистры расчета", "Calculation registers"]];
        const groups = await Promise.all(kinds.map(async ([kind, ru, english]) => {
          const nodes = await (kind === "InformationRegister" ? informationRegistersForPosting() : metadataSection(kind));
          return nodes.length ? treeGroup(tr(ru, english), nodes.map((node) => checkRow(node, selected.has(node.path))), kind === "InformationRegister") : null;
        }));
        if (root.isConnected) fill(content, groups);
      } catch (e) { fill(content, el("div", { class: "msg bad" }, e.message)); }
      finally { content.removeAttribute("aria-busy"); }
    }, { once: true });
  } else if (["Последовательности", "Sequences"].includes(title)) {
    fill(root, field("SequenceFilling"), memberships("Sequence", "Documents", tr("Входит в последовательности", "Included in sequences")));
  } else if (["Журналы", "Journals"].includes(title)) {
    fill(root, el("div", { class: "metadata-heading" }, items[0]?.caption), memberships("DocumentJournal", "RegisteredDocuments"));
  } else if (["Формы", "Forms"].includes(title)) {
    root.classList.add("metadata-forms");
    const fields = kind === "DocumentJournal" ? [["DefaultForm", "Списка", "List"]] : kind === "InformationRegister" ? [["DefaultRecordForm", "Записи", "Record"], ["DefaultListForm", "Списка", "List"]] : [
      ["DefaultObjectForm", kind === "Document" ? "Документа" : "Элемента", kind === "Document" ? "Document" : "Item"],
      ["DefaultFolderForm", "Группы", "Folder"], ["DefaultListForm", "Списка", "List"], ["DefaultChoiceForm", "Выбора", "Choice"], ["DefaultFolderChoiceForm", "Выбора группы", "Folder choice"]];
    fill(root, el("fieldset", null, el("legend", null, tr("Формы", "Forms")), fields.map(([name, ru, english]) => field(name, tr(ru, english)))), list("Form"));
  } else if (["Поле ввода", "Input fields"].includes(title)) {
    root.classList.add("metadata-input");
    fill(root, field("QuickChoice"), field("CreateOnInput"), el("div", { class: "metadata-heading" }, tr("Ввод по строке", "Input by string")),
      field("InputByString", tr("Ввод по строке", "Input by string")), field("SearchStringModeOnInputByString", tr("Способ поиска строки", "String search mode")),
      field("FullTextSearchOnInputByString", tr("Полнотекстовый поиск", "Full-text search")), field("ChoiceDataGetModeOnInputByString", tr("Режим получения данных выбора", "Choice data mode")),
      field("ChoiceHistoryOnInput"));
  } else if (["Команды", "Commands"].includes(title)) {
    fill(root, list("Command"), field("UseStandardCommands"));
  } else if (["Макеты", "Templates"].includes(title)) {
    fill(root, list("Template"), ...items.filter((it) => it.caption).map(caption));
  } else if (["Ввод на основании", "Generation"].includes(title)) {
    const derived = sheet(tr("Является основанием для", "Basis for"), [], false, true);
    const content = derived.querySelector(".metadata-sheet-body"); content.setAttribute("aria-busy", "true");
    fill(root, referenceList("BasedOn", tr("Вводится на основании", "Based on")), derived,
      ...items.filter((it) => it.control === "btnDerivedFromWizard").map(caption));
    root.addEventListener("object-tab-open", async () => {
      try { const nodes = await reverseReferences("BasedOn", path); if (root.isConnected) fill(content, nodes.map((node) => row(node))); }
      catch (e) { fill(content, el("div", { class: "msg bad" }, e.message)); }
      finally { content.removeAttribute("aria-busy"); }
    }, { once: true });
  } else if (["Права", "Access rights"].includes(title)) {
    const roles = sheet(tr("Роли", "Roles"), [], false, true);
    const rights = sheet(tr("Права", "Rights"), [], false, true);
    const restrictions = sheet(tr("Ограничения доступа к данным", "Data access restrictions"), [], false, true);
    const restrictionTable = (rows = [], columns = [], titles = []) => el("table", { class: "metadata-restrictions" },
      el("thead", null, el("tr", null, el("th", null, tr("Поля", "Fields")), el("th", null, tr("Ограничение доступа", "Access restriction")))),
      el("tbody", null, rows.map((r) => el("tr", null, el("td", null, titles[columns.indexOf(r.right)] || r.right),
        el("td", null, r.conditions.map((text) => el("pre", null, text)))))));
    fill(restrictions.querySelector(".metadata-sheet-body"), restrictionTable());
    roles.setAttribute("aria-busy", "true");
    fill(root, roles, rights, restrictions, field("PostInPrivilegedMode"), field("UnpostInPrivilegedMode"));
    root.addEventListener("object-tab-open", async () => {
      try {
        const nodes = await metadataSection("Role");
        fill(roles.querySelector(".metadata-sheet-body"), nodes.map((node) => row({ ...node, icon: null }, 0, async () => {
          const mine = ++roleSeq;
          try {
            const d = await api("rights", { path: node.path, q: path, limit: 300 });
            if (mine !== roleSeq || !root.isConnected) return;
            const selected = (d.rows || []).find((r) => r.object === path);
            fill(rights.querySelector(".metadata-sheet-body"), selected ? (d.columns || []).flatMap((key, i) => selected.values?.[i] == null ? [] : [el("label", { class: "metadata-membership" }, el("input", { type: "checkbox", disabled: true, checked: !!selected.values[i] }), d.titles?.[i] || key)]) : el("div", { class: "msg" }, T.none));
            fill(restrictions.querySelector(".metadata-sheet-body"), restrictionTable(selected?.restricted, d.columns, d.titles));
          } catch (e) {
            if (mine !== roleSeq || !root.isConnected) return;
            fill(rights.querySelector(".metadata-sheet-body"), el("div", { class: "msg bad" }, e.message));
          }
        })));
        roles.querySelector("button")?.click();
      } catch (e) { roles.appendChild(el("div", { class: "msg bad" }, e.message)); }
      finally { roles.removeAttribute("aria-busy"); }
    }, { once: true });
  } else if (["Обмен данными", "Data exchange"].includes(title)) {
    const plans = sheet(null, [], true); fill(root, plans);
    const content = plans.querySelector(".metadata-sheet-body"); content.setAttribute("aria-busy", "true");
    root.addEventListener("object-tab-open", async () => {
      try {
        const data = await api("exchange-membership", { path });
        const table = el("table", { class: "metadata-exchange" }, el("thead", null, el("tr", null,
          el("th", null, tr("План обмена", "Exchange plan")), el("th", null, tr("Авторегистрация", "Auto-registration")))),
          el("tbody", null, data.plans.map((plan) => el("tr", { "data-path": plan.path, "data-registered": String(plan.registered), tabindex: "0",
            ondblclick: () => openWindow({ kind: "object", path: plan.path, icon: "object:ExchangePlan" }),
            onkeydown: (ev) => { if (ev.key === "Enter") openWindow({ kind: "object", path: plan.path, icon: "object:ExchangePlan" }); },
            onclick: (ev) => { table.querySelectorAll(".cur").forEach((r) => r.classList.remove("cur")); ev.currentTarget.classList.add("cur"); } },
            el("td", null, plan.name), el("td", null, plan.registered === false ? "" : plan.auto_record === true ? tr("Разрешить", "Allow") : plan.auto_record === false ? tr("Запретить", "Deny") : tr("Неизвестно", "Unknown"))))));
        if (root.isConnected) fill(content, table, data.truncated ? el("div", { class: "msg" }, tr("Показана часть планов обмена", "Some exchange plans are omitted")) : null);
      } catch (e) { fill(content, el("div", { class: "msg bad" }, e.message)); }
      finally { content.removeAttribute("aria-busy"); }
    }, { once: true });
  } else if (["Прочее", "Other"].includes(title)) {
    root.classList.add("metadata-other");
    fill(root, ...items.filter((it) => ["BtnOpenModule", "BtnOpenManagerModule"].includes(it.control) || /^(btnPredefined|btnCharacterSet)$/i.test(it.control || "")).map((it) =>
        /^(btnPredefined|btnCharacterSet)$/i.test(it.control || "") ? el("div", { "data-control": it.control }, command(it.caption, () => openWindow({ kind: "predefined", path, title: tr("Предопределенные", "Predefined") + " · " + short(path), icon: "object:" + kind }))) : caption(it)),
      field("PredefinedDataUpdate"),
      field("DataLockFields"), field("FullTextSearch"), field("DataHistory"), field("UpdateDataHistoryImmediatelyAfterWrite"), field("ExecuteAfterWriteDataHistoryVersionProcessing"),
      ...items.filter((it) => it.control === "BtnOpenHelp").map(caption), field("IncludeHelpInContents"));
  }
  else { fill(root, items.map(itemNode)); items.filter((it) => it.name).forEach((it) => used.add(it.name)); }
  const extras = items.filter((it) => it.name && !used.has(it.name) && !(kind === "ChartOfCharacteristicTypes" && it.name === "Type"));
  if (extras.length) root.appendChild(el("details", { class: "metadata-extra" }, el("summary", null, tr("Дополнительно", "Additional properties")), extras.map(itemNode)));
  return root;
}

