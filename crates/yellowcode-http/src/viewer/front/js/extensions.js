const L = {
  ru: { title: "Расширения", unspecified: "Не указана", unknown: "Неизвестное значение", purpose: ["Исправление", "Адаптация", "Дополнение"], scope: "Информационная база" },
  en: { title: "Extensions", unspecified: "Not specified", unknown: "Unknown value", purpose: ["Patch", "Customization", "Add-on"], scope: "Infobase" },
};
const language = (lang) => L[lang] || L.ru;

export function displayExtensionText(raw) {
  return String(raw).normalize("NFC").replace(/[\u0000-\u001f\u007f-\u009f\u061c\u200b-\u200f\u2028-\u202e\u2060\u2066-\u2069\ufeff]/gu,
    (ch) => "\\u" + ch.codePointAt(0).toString(16).padStart(4, "0"));
}
export function extensionCalendar(value, lang) {
  return value === null ? language(lang).unspecified : displayExtensionText(value).replace("T", " ");
}
export function extensionPurpose(code, lang) {
  const t = language(lang);
  return t.purpose[code] || `${t.unknown} (${code})`;
}
export function extensionScope(code, lang) {
  const t = language(lang);
  return code === 1 ? t.scope : `${t.unknown} (${code})`;
}

const tokenOK = (scan) => typeof scan === "string" && scan.length === 34 && /^e[0-9a-f]{16}-[0-9a-f]{16}$/.test(scan);
function protocolError() { return Object.assign(new Error("Extensions could not be read"), { code: "unavailable" }); }
function validateItems(items) {
  if (!Array.isArray(items) || items.length > 256) throw protocolError();
  const ids = new Set();
  for (const item of items) {
    if (!item || typeof item.id !== "string" || !/^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(item.id) || ids.has(item.id)
      || typeof item.name !== "string" || item.name.length > 255 || typeof item.used_in_distributed_infobase !== "boolean"
      || !Number.isSafeInteger(item.order) || Math.abs(item.order) > 999999999
      || !Number.isSafeInteger(item.purpose_code) || Math.abs(item.purpose_code) > 99
      || !Number.isSafeInteger(item.scope_code) || Math.abs(item.scope_code) > 99
      || !(item.update_time === null || typeof item.update_time === "string" && /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}$/.test(item.update_time))) throw protocolError();
    for (const ch of item.name) if (ch.codePointAt(0) >= 0xd800 && ch.codePointAt(0) <= 0xdfff) throw protocolError();
    ids.add(item.id);
  }
  return items;
}
const contentCopy = value => value === "cas" || value === "saved";
const validCopies = reply => reply.version === 1 && contentCopy(reply.copy) && Array.isArray(reply.copies)
  && reply.copies.length <= 2 && reply.copies.every(contentCopy) && reply.copies.includes(reply.copy);
function validateContent(reply) {
  if (!validCopies(reply) || !Array.isArray(reply.groups) || reply.groups.length > 2048) throw protocolError();
  let count = 0;
  for (const group of reply.groups) {
    if (!group?.class || !Array.isArray(group.objects) || group.objects.some(row => !row || typeof row !== "object")) throw protocolError();
    count += group.objects.length;
    if (count > 2048) throw protocolError();
  }
  return reply;
}
function delay(ms, signal) {
  return new Promise((resolve, reject) => {
    const stop = () => { clearTimeout(timer); reject(new DOMException("aborted", "AbortError")); };
    const timer = setTimeout(() => { signal.removeEventListener("abort", stop); resolve(); }, ms);
    if (signal.aborted) stop(); else signal.addEventListener("abort", stop, { once: true });
  });
}

export function createExtensionsLoader(request, publish, startQueue = { pending: Promise.resolve() }) {
  let epoch = 0, active = null;
  const cancel = async (scan) => {
    if (!tokenOK(scan)) return;
    try { await request("POST", "extensions/cancel", { scan }); } catch {}
  };
  function orderedStart(selection, live) {
    const next = startQueue.pending.then(async () => {
      if (!live()) return null;
      const reply = selection
        ? await request("POST", "extensions/content/start", selection)
        : await request("POST", "extensions/start", {});
      if (!live()) {
        await cancel(reply?.scan);
        return null;
      }
      return reply;
    });
    startQueue.pending = next.catch(() => {});
    return next;
  }
  function retire() {
    ++epoch;
    const old = active;
    active = null;
    old?.abort.abort();
    return old?.scan ? cancel(old.scan) : Promise.resolve();
  }
  return {
    async open(selection = null) {
      void retire();
      const current = { epoch: ++epoch, abort: new AbortController(), scan: null };
      active = current;
      const live = () => active === current && epoch === current.epoch;
      publish({ state: "loading" });
      try {
        if (!selection) {
          const source = await request("GET", "info?extensions=1", null, current.abort.signal);
          if (!live()) return;
          if (!source || !Object.hasOwn(source, "extensions_unavailable")
            || !(source.extensions_unavailable === null || typeof source.extensions_unavailable === "string")) throw protocolError();
          if (source.extensions_unavailable !== null) {
            publish({ state: "failed", code: source.extensions_unavailable });
            return;
          }
        }
        let reply;
        for (;;) {
          try {
            reply = await orderedStart(selection, live);
            break;
          } catch (error) {
            if (!live()) return;
            if (error.code !== "busy") throw error;
            await delay(200, current.abort.signal);
          }
        }
        if (reply === null && !live()) return;
        if (!reply || !tokenOK(reply.scan)) throw protocolError();
        if (!live()) { await cancel(reply.scan); return; }
        current.scan = reply.scan;
        for (;;) {
          if (!live()) return;
          if (!reply || !tokenOK(reply.scan)) throw protocolError();
          if (reply.scan !== current.scan) throw Object.assign(protocolError(), { code: "source_changed" });
          if (selection && ["ready", "partial"].includes(reply.state)) { publish(validateContent(reply)); return; }
          if (!selection && reply.state === "ready") { publish({ state: "ready", scan: current.scan, items: validateItems(reply.items) }); return; }
          if (reply.state === "failed" || reply.state === "cancelled") {
            publish({ state: reply.state, code: reply.code || "unavailable",
              ...(selection && validCopies(reply) && { copy: reply.copy, copies: reply.copies }) });
            return;
          }
          if (reply.state !== "loading") throw protocolError();
          reply = await request("GET", "extensions?scan=" + encodeURIComponent(current.scan), null, current.abort.signal);
          if (live() && reply.state === "loading") await delay(300, current.abort.signal);
        }
      } catch (error) {
        if (live()) publish({ state: "failed", code: error.code || "unavailable" });
      }
    },
    close() { const cleanup = retire(); publish({ state: "closed" }); return cleanup; },
  };
}

export const extensionTitle = (lang) => language(lang).title;
const UI = {
  ru: { statusUnavailable: "Статус активации и применения здесь недоступен.", reload: "Обновить", cancel: "Отменить", loading: "Чтение расширений…", opening: "Открывается источник…", empty: "Установленных расширений нет", cancelled: "Чтение расширений отменено", list: "Установленные расширения", properties: "Свойства", pick: "Выберите расширение", name: "Имя", id: "Идентификатор", order: "Порядок", purpose: "Назначение", scope: "Область действия", updated: "Время обновления", distributed: "Используется в распределённой базе", yes: "Да", no: "Нет" },
  en: { statusUnavailable: "Activation and application status is unavailable in this view.", reload: "Reload", cancel: "Cancel", loading: "Reading extensions…", opening: "Opening source…", empty: "No installed extensions", cancelled: "Reading extensions was cancelled", list: "Installed extensions", properties: "Properties", pick: "Select an extension", name: "Name", id: "Identifier", order: "Order", purpose: "Purpose", scope: "Scope", updated: "Update time", distributed: "Used in a distributed infobase", yes: "Yes", no: "No" },
};
function failureText(code, lang) {
  const texts = {
    unsupported: ["Просмотр расширений для этого источника пока недоступен", "Viewing extensions is unavailable for this source"],
    source_changed: ["Источник изменился. Откройте его заново", "The source changed; reopen it"],
    source_closed: ["Источник закрыт. Откройте его заново", "The source is closed; reopen it"],
    stale: ["Запрос устарел. Обновите список", "This request expired; reload the list"],
    deadline: ["Чтение расширений заняло слишком много времени", "Reading extensions took too long"],
    memory_limit: ["Недостаточно ресурсов для просмотра расширений. Закройте ненужные окна и повторите", "There are not enough resources to view extensions; close unused windows and retry"],
    work_limit: ["Этот источник превышает поддерживаемые ограничения просмотра расширений", "This source exceeds the supported extension viewing limits"],
    cancelled: ["Чтение расширений отменено", "Reading extensions was cancelled"],
    no_content: ["Сохранённый состав отсутствует", "No stored extension content"],
    content_layout: ["Формат сохранённого состава не поддерживается", "The stored extension layout is unsupported"],
    corrupt_content: ["Сохранённый состав повреждён или не прошёл проверку", "The stored extension content could not be verified"],
  };
  return (texts[code] || ["Не удалось прочитать расширения", "Extensions could not be read"])[lang === "en" ? 1 : 0];
}
async function requestExtensions(method, path, body, signal) {
  const response = await fetch("/api/" + path, {
    method, cache: "no-store", signal,
    ...(body === null ? {} : { headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }),
  });
  const reader = response.body?.getReader();
  if (!reader) throw protocolError();
  let text = "", size = 0;
  const decoder = new TextDecoder("utf-8", { fatal: true });
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > 1048576) throw protocolError();
      text += decoder.decode(value, { stream: true });
    }
    text += decoder.decode();
  } catch (error) { await reader.cancel().catch(() => {}); throw error; }
  finally { reader.releaseLock(); }
  const reply = JSON.parse(text);
  if (!response.ok) throw Object.assign(new Error("Extensions could not be read"), { code: reply?.code || "unavailable" });
  return reply;
}

const CONTENT = {
  ru: {title:"Состав", loading:"Чтение состава…", empty:"Объектов верхнего уровня нет", partial:"Часть записей прочитана не полностью",
    copy:"Копия", cas:"Поколение из базы", saved:"Сохранённая копия", type:"Тип", belonging:"Принадлежность", own:"Собственный", adopted:"Заимствованный", unknown:"Неизвестно",
    comment:"Комментарий", target:"UUID цели", header:"UUID объекта", properties:"Пар свойств", missing:"Запись отсутствует",
    unsupported:"Тип записи не поддерживается", damaged:"Данные записи повреждены"},
  en: {title:"Contents", loading:"Reading contents…", empty:"No top-level objects", partial:"Some entries could only be partially read",
    copy:"Copy", cas:"Stored generation", saved:"Saved copy", type:"Type", belonging:"Belonging", own:"Own", adopted:"Adopted", unknown:"Unknown",
    comment:"Comment", target:"Target UUID", header:"Object UUID", properties:"Property pairs", missing:"Entry is missing",
    unsupported:"Unsupported entry type", damaged:"Entry data is damaged"},
};

export function mountExtensions(host, { lang = document.documentElement.lang, request = requestExtensions, sourceLoading = false, kindLabel = kind => kind } = {}) {
  const t = UI[lang] || UI.ru;
  const c = CONTENT[lang] || CONTENT.ru;
  const node = (tag, text, attrs = {}) => {
    const n = document.createElement(tag);
    if (text !== null) n.textContent = text;
    for (const [key, value] of Object.entries(attrs)) n.setAttribute(key, value);
    return n;
  };
  const root = node("section", null, { class: "extensions-view", "aria-label": extensionTitle(lang), "data-extensions-state": "" });
  const toolbar = node("div", null, { class: "fbar" });
  const reload = node("button", t.reload, { type: "button" });
  const cancel = node("button", t.cancel, { type: "button" });
  const status = node("div", null, { class: "msg", role: "status", "aria-live": "polite" });
  const columns = node("div", null, { class: "extensions-columns" });
  const list = node("div", null, { class: "extensions-list", role: "listbox", "aria-label": t.list });
  const card = node("section", null, { class: "extensions-properties", "aria-label": t.properties });
  const summary = node("div", null);
  const details = node("section", null, { "aria-label": c.title, class: "extensions-content" });
  details.hidden = true;
  card.append(summary, details);
  toolbar.append(reload, cancel); columns.append(list, card); root.append(toolbar, status, columns); host.append(root);
  let alive = true, items = [], selected = null, state = { state: "loading" };
  function select(id, focus = false) {
    if (selected !== id) void contentLoader.close();
    selected = id;
    for (const option of list.children) {
      const yes = option.dataset.extensionId === selected;
      option.setAttribute("aria-selected", String(yes)); option.tabIndex = yes ? 0 : -1;
      if (focus && yes) option.focus();
    }
    summary.replaceChildren();
    const item = items.find((i) => i.id === selected);
    if (!item) { summary.append(node("p", t.pick, { class: "msg" })); return; }
    summary.append(node("h2", t.properties));
    const fields = node("dl", null, { class: "extensions-fields" });
    for (const [label, value] of [
      [t.name, displayExtensionText(item.name)], [t.id, item.id], [t.order, item.order],
      [t.purpose, extensionPurpose(item.purpose_code, lang)], [t.scope, extensionScope(item.scope_code, lang)],
      [t.updated, extensionCalendar(item.update_time, lang)], [t.distributed, item.used_in_distributed_infobase ? t.yes : t.no],
    ]) fields.append(node("dt", label), node("dd", value));
    const action = node("button", c.title, { type: "button" });
    action.addEventListener("click", () => openContents());
    summary.append(fields, node("p", t.statusUnavailable, { class: "msg" }), action);
  }
  function openContents(copy) {
    if (alive && selected && tokenOK(state.scan)) {
      void contentLoader.open({ scan: state.scan, id: selected, ...(copy && { copy }) });
    }
  }
  function paintContents(result) {
    details.replaceChildren();
    details.hidden = !alive || result.state === "closed";
    if (details.hidden) return;
    const loading = result.state === "loading";
    details.setAttribute("aria-busy", String(loading));
    if (loading) {
      const stop = node("button", t.cancel, { type: "button" });
      stop.addEventListener("click", () => void contentLoader.close());
      details.append(node("p", c.loading, { role: "status" }), stop);
      return;
    }
    const selector = node("div", null);
    if (result.copies?.length === 1) selector.textContent = c[result.copy];
    if (result.copies?.length > 1) {
      const selectCopy = node("select", null, { "aria-label": c.copy });
      for (const copy of result.copies) selectCopy.append(node("option", c[copy], { value: copy }));
      selectCopy.value = result.copy;
      selectCopy.addEventListener("change", () => openContents(selectCopy.value));
      selector.append(selectCopy);
    }
    if (result.copies) details.append(selector);
    if (!["ready", "partial"].includes(result.state)) {
      details.append(node("p", failureText(result.code, lang), { role: "alert" }));
      return;
    }
    if (result.state === "partial") details.append(node("p", c.partial, { role: "status" }));
    if (!result.groups.some(group => group.objects.length > 0)) {
      details.append(node("p", c.empty, { role: "status" }));
      return;
    }
    const table = node("table", null, { "aria-label": c.title });
    const heading = node("tr", null);
    for (const label of [c.type, t.name, c.belonging]) {
      heading.append(node("th", label, { scope: "col" }));
    }
    const head = node("thead", null), body = node("tbody", null);
    head.append(heading); table.append(head, body); details.append(table);
    for (const group of result.groups) for (const object of group.objects) {
      const row = node("tr", null);
      const type = group.class.kind == null ? group.class.class_id : kindLabel(group.class.kind, lang);
      const name = node("td", null);
      const info = node("details", null);
      const display = value => value == null ? "—" : displayExtensionText(value) || "—";
      info.append(node("summary", display(object.name ?? object.entry_id)));
      const fields = node("dl", null, { class: "extensions-fields" });
      for (const [label, value] of [[c.header, object.header_id], [c.target, object.target_id],
        [c.comment, object.comment], [c.properties, object.property_count]]) {
        fields.append(node("dt", label), node("dd", display(value)));
      }
      info.append(fields);
      const issues = { missing_entry: c.missing, unsupported_kind: c.unsupported, unsupported_header: c.unsupported };
      if (object.issue) info.append(node("p", issues[object.issue] || c.damaged));
      name.append(info);
      const belonging = object.belonging === "unknown" ? `${c.unknown} (${display(object.belonging_code)})`
        : c[object.belonging] ?? "—";
      row.append(node("td", display(type)), name, node("td", belonging));
      body.append(row);
    }

  }
  function paint(next) {
    if (!alive) return;
    state = next;
    void contentLoader.close();
    items = !sourceLoading && next.state === "ready" ? next.items : [];
    selected = items[0]?.id || null;
    root.dataset.extensionsState = sourceLoading ? "source_loading" : next.state;
    const loading = sourceLoading || next.state === "loading";
    root.setAttribute("aria-busy", String(loading));
    status.setAttribute("role", next.state === "failed" && !sourceLoading ? "alert" : "status");
    status.textContent = sourceLoading ? t.opening : next.state === "loading" ? t.loading
      : next.state === "ready" ? items.length ? "" : t.empty
      : ["closed", "cancelled"].includes(next.state) ? t.cancelled : failureText(next.code, lang);
    reload.disabled = sourceLoading; cancel.hidden = !loading || sourceLoading;
    list.replaceChildren();
    for (const item of items) {
      const option = node("button", displayExtensionText(item.name) || "—", { type: "button", role: "option", "data-extension-id": item.id });
      option.addEventListener("click", () => select(item.id));
      option.addEventListener("keydown", (event) => {
        const index = items.findIndex((i) => i.id === selected);
        const target = { ArrowDown: Math.min(index + 1, items.length - 1), ArrowUp: Math.max(0, index - 1), Home: 0, End: items.length - 1 }[event.key];
        if (target === undefined) return;
        event.preventDefault(); select(items[target].id, true);
      });
      list.append(option);
    }
    select(selected);
  }
  const startQueue = { pending: Promise.resolve() };
  const contentLoader = createExtensionsLoader(request, paintContents, startQueue);
  const loader = createExtensionsLoader(request, paint, startQueue);
  const refresh = () => { if (alive && !sourceLoading) void loader.open(); };
  const stop = () => { void loader.close(); };
  reload.addEventListener("click", refresh); cancel.addEventListener("click", stop);
  const sourceChanged = (event) => {
    if (!alive) return;
    if (event.detail?.state === "loading") { sourceLoading = true; void loader.close(); }
    else if (event.detail?.state === "retained") { sourceLoading = false; refresh(); }
    else if (event.detail?.state === "replaced") { sourceLoading = false; void loader.close(); paint({ state: "failed", code: "source_closed" }); }
  };
  const destroy = () => {
    if (!alive) return;
    alive = false; void contentLoader.close(); void loader.close(); items = []; selected = null;
    list.replaceChildren(); card.replaceChildren();
    document.removeEventListener("viewer-source-state", sourceChanged); window.removeEventListener("pagehide", destroy);
  };
  document.addEventListener("viewer-source-state", sourceChanged); window.addEventListener("pagehide", destroy, { once: true });
  if (sourceLoading) paint(state); else refresh();
  return { reload: refresh, destroy };
}

export async function openExtensions() {
  const { openWindow } = await import("./tabs.js");
  return openWindow({ kind: "extensions", path: "Extensions", title: extensionTitle(document.documentElement.lang) });
}
export async function initExtensions() {
  const [{ registerKind }, { desktopBusy }, { localizedKind }] = await Promise.all([
    import("./tabs.js"), import("./desktop.js"), import("./icon.js"),
  ]);
  registerKind("extensions", { create: (win) => mountExtensions(win.pane, {
    sourceLoading: desktopBusy(), kindLabel: localizedKind,
  }) });
}
