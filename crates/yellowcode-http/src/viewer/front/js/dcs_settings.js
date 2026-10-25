import { el, fill } from './dom.js';
import { browseRows, splitPane, usage, valueText } from './dcs_widgets.js';

export function mountDcsSettings(page, schema, allFields, helpers) {
  const { tabs, table, col, toolbar, tr, source } = helpers;
  page.classList.add('dcs-settings-page');
  const labels = {
    Asc: 'По возрастанию', Desc: 'По убыванию', Items: 'Элементы', Hierarchy: 'Иерархия', HierarchyOnly: 'Только иерархия',
    Equal: 'Равно', NotEqual: 'Не равно', Greater: 'Больше', GreaterOrEqual: 'Больше или равно', Less: 'Меньше', LessOrEqual: 'Меньше или равно',
    InList: 'В списке', NotInList: 'Не в списке', InHierarchy: 'В группе', NotInHierarchy: 'Не в группе', InListByHierarchy: 'В группе из списка', NotInListByHierarchy: 'Не в группе из списка',
    Contains: 'Содержит', NotContains: 'Не содержит', BeginsWith: 'Начинается с', Filled: 'Заполнено', NotFilled: 'Не заполнено',
    AndGroup: 'Группа И', OrGroup: 'Группа ИЛИ', NotGroup: 'Группа НЕ',
    SelectedItemAuto: '<Авто>', SelectedItemGroup: 'Группа полей', SelectedItemField: 'Поле',
    Auto: 'Авто', True: 'Истина', False: 'Ложь', true: 'Истина', false: 'Ложь',
    Together: 'Вместе', Separately: 'Отдельно', Begin: 'Начало', End: 'Конец', None: 'Нет',
    WithOwner: 'Вместе с владельцем', TogetherWithOwner: 'Вместе с владельцем', Horizontal: 'Горизонтально', Vertical: 'Вертикально',
    Column: 'Гистограмма', Histogram: 'Гистограмма', Bar: 'Линейчатая', Line: 'График', Pie: 'Круговая',
    МакетОформления: 'Макет оформления', РасположениеИтогов: 'Расположение итогов', РасположениеПолейГруппировок: 'Расположение полей группировок',
    РасположениеГруппировок: 'Расположение группировок', РасположениеРеквизитов: 'Расположение реквизитов', РасположениеРесурсов: 'Расположение ресурсов',
    РасположениеОбщихИтоговПоГоризонтали: 'Расположение общих итогов по горизонтали', РасположениеОбщихИтоговПоВертикали: 'Расположение общих итогов по вертикали', ТипЗаголовкаПолей: 'Тип заголовка полей', ТипДиаграммы: 'Тип диаграммы',
    AppearanceTemplate: 'Макет оформления', TotalsPlacement: 'Расположение итогов', GroupFieldsPlacement: 'Расположение полей группировок',
    GroupingsPlacement: 'Расположение группировок', AttributesPlacement: 'Расположение реквизитов', ResourcesPlacement: 'Расположение ресурсов',
    OverallHorizontalTotalsPlacement: 'Расположение общих итогов по горизонтали', OverallVerticalTotalsPlacement: 'Расположение общих итогов по вертикали',
    FieldsTitleType: 'Тип заголовка полей', FieldTitleType: 'Тип заголовка полей', ChartType: 'Тип диаграммы',
    SkipBaseValue: 'Пропускать базовое значение', ChartSkipBaseValue: 'Пропускать базовое значение',
    ChartLabelComposition: 'Состав подписей', LabelComposition: 'Состав подписей', Title: 'Заголовок',
    Sea: 'Море', SeriesPercent: 'Серия + процент', SeriesAndPercent: 'Серия + процент',
  };
  const labelText = (v) => tr((Object.hasOwn(labels, v) ? labels[v] : v) || '', v || '');
  const labelled = (v) => ({ text: labelText(v), raw: v || '' });
  const textOf = (v) => v && typeof v === 'object' ? v.text : String(v ?? '');
  const titleOf = (v) => v && typeof v === 'object' ? v.raw : String(v ?? '');
  const valueCell = (v) => ({ text: valueText(v), raw: v ? (v.type ? v.type + ': ' : '') + valueText(v) : '' });
  const outputEnums = [
    [['ChartType', 'ТипДиаграммы'], 'ChartType', ['Column', 'Histogram', 'Bar', 'Line', 'Pie']],
    [['GroupFieldsPlacement', 'РасположениеПолейГруппировок'], 'DataCompositionGroupFieldsPlacement', ['Together', 'Separately']],
    [['GroupingsPlacement', 'РасположениеГруппировок'], 'DataCompositionGroupPlacement', ['Begin', 'End', 'None']],
    [['AttributesPlacement', 'РасположениеРеквизитов'], 'DataCompositionAttributesPlacement', ['Separately', 'Together']],
  ];
  const outputValue = (p) => {
    const v = p.value;
    const known = v && !v.items?.length && outputEnums.some(([parameters, type, values]) => parameters.includes(p.parameter) && v.type === type && values.includes(v.text));
    const raw = v?.items?.length || v?.type === 'Null' ? valueText(v) : v?.text ?? '';
    return { text: known ? labelText(v.text) : raw, raw: v ? (v.type ? v.type + ': ' : '') + raw : '' };
  };
  const report = tr('Отчет', 'Report');
  const variants = schema.variants?.length ? schema.variants : schema.default_settings ? [{ name: tr('По умолчанию', 'Default'), settings: schema.default_settings, structure: [] }] : [];
  const variantTable = table('GridSettingsVariant', variants.map((v) => [v.name, v.presentation]));
  const variantPane = el('section', { class: 'dcs-table-page' }, toolbar(), variantTable);
  const structure = el('div', { class: 'dcs-structure', role: 'tree', 'aria-label': tr('Структура', 'Structure') });
  const upper = el('section', { class: 'dcs-structure-pane' }, toolbar(), structure);
  const settings = el('section', { class: 'dcs-settings-lower' });
  const scopeLabel = el('span', { class: 'dcs-scope-label' });
  const rootScope = el('button', { type: 'button' }, tr('Общие', 'Overall'));
  const selectedScope = el('button', { type: 'button' }, tr('Выбранный элемент', 'Selected item'));
  settings.append(el('div', { class: 'dcs-scope' }, tr('Настройки:', 'Settings:'), rootScope, selectedScope, scopeLabel));
  const lowerPages = tabs(source.get('LayouterTunes').tab_pages, settings);
  const detail = el('section', { class: 'dcs-settings-detail' });
  const stopHeight = splitPane(detail, upper, settings, 'horizontal', tr('Высота структуры', 'Structure height'), 42, { before: 90, after: 200 });
  const stopWidth = splitPane(page, variantPane, detail, 'vertical', tr('Ширина вариантов', 'Variant width'), 23, { before: 120, after: 320 });
  const tabBar = settings.querySelector('.dialog-tabs');
  const revealTab = () => tabBar.querySelector('[aria-selected=true]')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  tabBar.addEventListener('click', revealTab);
  tabBar.addEventListener('keydown', revealTab);
  let variant, selected, selectedLabel = report, scope = 'selected';
  const fieldPairs = [...new Map(allFields.map((f) => [f.path || f.field, f])).values()];
  const note = (text) => el('div', { class: 'dcs-settings-note', role: 'status' }, text);
  function sheet(name, headings, rows, navigation = true) {
    const node = table(name, [], headings.map(([ru, en]) => col(ru, en, 110)));
    node.classList.add('dcs-settings-sheet');
    const useColumn = headings[0]?.[1] === 'Use';
    node.querySelector('table').prepend(el('colgroup', null, headings.map((_, i) => el('col', { style: useColumn && i === 0 ? 'width:28px' : '' }))));
    node.querySelectorAll('th').forEach((th, i) => { th.style.minWidth = '0'; th.title = th.textContent; if (useColumn && i === 0) { th.setAttribute('aria-label', th.textContent); th.textContent = '✓'; } });
    const tbody = node.querySelector('tbody');
    for (const values of rows) {
      const row = el('tr', { 'data-browse-row': '', tabindex: '-1' }, values.map((v) => el('td', { title: titleOf(v) }, textOf(v))));
      tbody.append(row);
    }
    if (navigation) browseRows(node);
    return node;
  }
  function fieldLayout(host, name, headings, rows) {
    const available = sheet('Available' + name, [['Доступные поля', 'Available fields']], fieldPairs.map((f) => [f.title ? f.title + ' (' + f.path + ')' : f.path || f.field]));
    available.classList.add('dcs-available-fields');
    const chosen = el('section', { class: 'dcs-table-page' }, toolbar(), sheet(name, headings, rows));
    const pair = el('div', { class: 'dcs-field-selection' });
    splitPane(pair, available, chosen, 'vertical', tr('Ширина доступных полей', 'Available fields width'), 33);
    host.append(pair);
  }
  function hierarchy(host, name, headings, items, cells) {
    const node = sheet(name, headings, [], false); const body = node.querySelector('tbody');
    function append(items, depth = 0, ancestors = []) {
      for (const item of items || []) {
        const row = el('tr', { 'data-browse-row': '', tabindex: '-1' });
        const values = cells(item); const children = item.items || [];
        const control = children.length ? el('button', { type: 'button', 'aria-expanded': 'true', 'aria-label': tr('Свернуть группу', 'Collapse group') }, '⊟') : null;
        values.forEach((v, i) => row.append(el('td', { title: titleOf(v), style: i === 1 ? `padding-left:${4 + depth * 15}px` : '' }, i === 1 ? control : null, textOf(v))));
        row._ancestors = ancestors; row._expanded = true; body.append(row);
        const toggle = () => {
          row._expanded = !row._expanded; control.setAttribute('aria-expanded', String(row._expanded)); control.textContent = row._expanded ? '⊟' : '⊞';
          body.querySelectorAll('tr').forEach((r) => { r.hidden = r._ancestors.some((a) => !a._expanded); });
        };
        if (control) {
          control.onclick = toggle;
          row.addEventListener('keydown', (e) => { if (['ArrowLeft', 'ArrowRight'].includes(e.key)) { e.preventDefault(); if ((e.key === 'ArrowRight') !== row._expanded) toggle(); } });
        }
        append(children, depth + 1, [...ancestors, row]);
      }
    }
    append(items); browseRows(node); host.append(node);
  }
  function renderSettings() {
    rootScope.setAttribute('aria-pressed', String(scope === 'root'));
    selectedScope.setAttribute('aria-pressed', String(scope === 'selected'));
    selectedScope.disabled = !selected;
    scopeLabel.textContent = scope === 'root' ? report : selectedLabel;
    scopeLabel.title = scopeLabel.textContent;
    const current = scope === 'root' ? variant?.settings : selected?.settings;
    const settingsData = current || {};
    lowerPages.forEach((p) => { p.classList.add('dcs-table-page'); fill(p); });
    lowerPages[0].append(sheet('SettingsParameters', [['Использование', 'Use'], ['Параметр', 'Parameter'], ['Значение', 'Value']], (settingsData.data_parameters || []).map((p) => [usage(p.use), p.parameter, valueCell(p.value)])));
    fieldLayout(lowerPages[1], 'SettingsGrouping', [['Поле', 'Field'], ['Тип группировки', 'Grouping type']], scope === 'selected' ? (selected?.group_items || []).map((g) => [g.field || labelText(g.kind), labelled(g.group_type)]) : []);
    const selectionHost = el('section', { class: 'dcs-table-page' });
    hierarchy(selectionHost, 'SettingsSelection', [['Использование', 'Use'], ['Поле', 'Field'], ['Заголовок', 'Title']], settingsData.selection, (s) => [usage(s.use), s.field || labelled(s.kind), s.title]);
    const available = sheet('AvailableSelection', [['Доступные поля', 'Available fields']], fieldPairs.map((f) => [f.title ? f.title + ' (' + f.path + ')' : f.path || f.field]));
    available.classList.add('dcs-available-fields');
    const selectionPair = el('div', { class: 'dcs-field-selection' });
    splitPane(selectionPair, available, selectionHost, 'vertical', tr('Ширина доступных полей', 'Available fields width'), 33);
    lowerPages[2].append(selectionPair);
    const filterHost = el('section', { class: 'dcs-table-page' });
    hierarchy(filterHost, 'SettingsFilter', [['Использование', 'Use'], ['Левое значение', 'Left value'], ['Вид сравнения', 'Comparison'], ['Правое значение', 'Right value']], settingsData.filter, (f) => [usage(f.use), valueText(f.left) || labelled(f.group_type || f.kind), labelled(f.comparison_type), valueText(f.right)]);
    const filterAvailable = sheet('AvailableFilter', [['Доступные поля', 'Available fields']], fieldPairs.map((f) => [f.path || f.field]));
    filterAvailable.classList.add('dcs-available-fields');
    const filterPair = el('div', { class: 'dcs-field-selection' }); splitPane(filterPair, filterAvailable, filterHost, 'vertical', tr('Ширина полей отбора', 'Filter fields width'), 33); lowerPages[3].append(filterPair);
    fieldLayout(lowerPages[4], 'SettingsSorting', [['Использование', 'Use'], ['Поле', 'Field'], ['Направление', 'Direction']], (settingsData.order || []).map((o) => [usage(o.use), o.field || labelled(o.kind), labelled(o.order_type)]));
    const summarizeSelection = (items) => (items || []).map((i) => [i.title || i.field || labelText(i.kind), summarizeSelection(i.items)].filter(Boolean).join(': ')).join('; ');
    const summarizeFilter = (items) => (items || []).map((i) => {
      const mark = usage(i.use);
      if (i.items?.length || i.group_type) {
        const operator = { AndGroup: tr('И', 'AND'), OrGroup: tr('ИЛИ', 'OR'), NotGroup: tr('НЕ', 'NOT') }[i.group_type] || labelText(i.group_type || i.kind);
        return [mark, operator + ' (' + summarizeFilter(i.items) + ')'].filter(Boolean).join(' ');
      }
      return [mark, valueText(i.left), labelText(i.comparison_type), valueText(i.right)].filter(Boolean).join(' ');
    }).join('; ');
    lowerPages[5].append(sheet('SettingsAppearance', [['Использование', 'Use'], ['Оформление', 'Appearance'], ['Условие', 'Condition'], ['Поля', 'Fields']], (settingsData.conditional_appearance || []).map((a) => [usage(a.use), (a.appearance || []).map((p) => p.parameter + ': ' + valueText(p.value)).join('; '), summarizeFilter(a.filter), summarizeSelection(a.selection)])));
    lowerPages[6].append(sheet('SettingsUserFields', [['Поле', 'Field'], ['Заголовок', 'Title'], ['Выражение', 'Expression']], []), note(tr('Пользовательские поля недоступны в текущем представлении схемы.', 'User fields are unavailable in this schema projection.')));
    lowerPages[7].append(sheet('SettingsOutput', [['Использование', 'Use'], ['Параметр', 'Parameter'], ['Значение', 'Value']], (settingsData.output_parameters || []).map((p) => [usage(p.use), labelled(p.parameter), outputValue(p)])));
    if (!current) lowerPages.forEach((p, i) => { if (i !== 6) p.append(note(tr('Настройки этой области недоступны.', 'Settings for this scope are unavailable.'))); });
  }
  rootScope.onclick = () => { scope = 'root'; renderSettings(); };
  selectedScope.onclick = () => { scope = 'selected'; renderSettings(); };
  function selectNode(node, label, row) {
    selected = node; selectedLabel = label; scope = 'selected';
    structure.querySelectorAll('[role=treeitem]').forEach((r) => { r.setAttribute('aria-selected', String(r === row)); r.tabIndex = r === row ? 0 : -1; });
    renderSettings();
  }
  function renderStructure() {
    fill(structure);
    const add = (node, label, parent, children, depth = 1) => {
      const group = el('div', { role: 'group' });
      const toggle = el('button', { type: 'button', tabindex: '-1', class: 'dcs-tree-toggle', 'aria-label': tr('Свернуть / развернуть', 'Collapse / expand') }, children.length ? '⊟' : '');
      toggle.disabled = !children.length;
      const row = el('div', { role: 'treeitem', 'aria-level': depth, 'data-browse-row': '', tabindex: '-1', title: label, 'aria-expanded': children.length ? 'true' : null }, toggle, label);
      row.onclick = (e) => { if (e.target !== toggle) selectNode(node, label, row); };
      const collapse = (open) => {
        const hidesSelection = !open && group.querySelector('[aria-selected=true]');
        const hadFocus = row.contains(document.activeElement) || group.contains(document.activeElement);
        if (hidesSelection) selectNode(node, label, row);
        group.hidden = !open; row.setAttribute('aria-expanded', String(open)); toggle.textContent = open ? '⊟' : '⊞';
        if (hidesSelection && hadFocus) row.focus({ preventScroll: true });
      };
      toggle.onclick = (e) => { e.stopPropagation(); collapse(group.hidden); };
      row.onkeydown = (e) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); selectNode(node, label, row); }
        if (children.length && ['ArrowLeft', 'ArrowRight'].includes(e.key)) { e.preventDefault(); collapse(e.key === 'ArrowRight'); }
      };
      parent.append(row, group);
      children.forEach((child) => {
        const fieldLabel = (child.group_items || []).map((g) => g.field + (g.group_type && g.group_type !== 'Items' ? ' (' + labelText(g.group_type) + ')' : '')).join(', ') || (child.fields || []).join(', ');
        const label = child._label || fieldLabel || (child.kind === 'StructureItemTable' ? tr('Таблица', 'Table') : child.kind === 'StructureItemChart' ? tr('Диаграмма', 'Chart') : tr('<Детальные записи>', '<Detail records>'));
        const subs = [...(child.items || [])];
        if (child.rows?.length) subs.push({ _label: tr('Строки', 'Rows'), items: child.rows });
        if (child.columns?.length) subs.push({ _label: tr('Колонки', 'Columns'), items: child.columns });
        add(child, label, group, subs, depth + 1);
      });
      if (depth === 1) selectNode(node, label, row);
    };
    if (variant) add(variant, report, structure, variant.structure || []);
    else { structure.append(note(tr('Варианты настроек отсутствуют.', 'No settings variants.'))); selected = null; renderSettings(); }
    browseRows(structure);
  }
  variantTable.querySelectorAll('tbody tr').forEach((row, i) => {
    row.dataset.browseRow = ''; row.tabIndex = i ? -1 : 0;
    row.onclick = () => { variant = variants[i]; renderStructure(); };
    row.onkeydown = (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); row.click(); } };
  });
  browseRows(variantTable); variant = variants[0]; renderStructure();
  return () => { stopHeight(); stopWidth(); };
}
