import { el, fill } from "./dom.js";

const en = () => document.documentElement.lang === "en";
const tr = (ru, english) => en() ? english : ru;
const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
const display = value => String(value || "").replaceAll("\\\\?\\UNC\\", "\\\\").replaceAll("\\\\?\\", "");
let active = null;

export function conversionItems() {
  return window.__TAURI__?.core ? [{ label: tr("Конвертировать…", "Convert…"), onclick: () => openConvert() }] : [];
}

export async function openConvert(path = null) {
  if (active?.isConnected) { active.focus(); return; }
  const previous = document.activeElement;
  const title = tr("Конвертация", "Conversion");
  const dialog = el("dialog", { class: "service-dialog convert-dialog", "aria-label": title });
  active = dialog;
  const source = el("input", { type: "text", id: "convert-source", spellcheck: "false" });
  const target = el("select", { id: "convert-target" });
  const output = el("input", { type: "text", id: "convert-output", spellcheck: "false" });
  const without = el("input", { type: "checkbox", id: "convert-without" });
  const memory = el("input", { type: "text", id: "convert-memory", value: "auto" });
  const layout = el("select", { id: "convert-layout" },
    el("option", { value: "" }, tr("Выберите формат…", "Choose layout…")),
    el("option", { value: "users8" }, "Users8"), el("option", { value: "users-spr11" }, "UsersSpr11"));
  const error = el("p", { role: "alert", class: "dialog-error", hidden: true });
  const status = el("div", { role: "status", "aria-live": "polite", class: "convert-status" });
  const progress = el("progress", { hidden: true, "aria-label": tr("Выполняется конвертация", "Converting") });
  const elapsed = el("span");
  const platform = el("p", { class: "convert-note" });
  let initializing = true, running = false, valid = false, polling = null, generation = 0, rawSource = null, rawOutput = null;
  const controls = [];
  const label = (text, control) => el("label", { for: control.id }, text);
  const run = el("button", { type: "button", disabled: true }, tr("Конвертировать", "Convert"));
  const close = el("button", { type: "button", onclick: () => { if (!running) dialog.close(); } }, tr("Закрыть", "Close"));
  const report = e => { error.hidden = false; error.textContent = display(e.message || e); };
  const clearError = () => { error.hidden = true; error.textContent = ""; };
  const restoreFields = el("fieldset", null, el("legend", null, tr("Параметры восстановления", "Restore options")),
    el("label", { class: "convert-check" }, without, tr("Без пользователей", "Without users")),
    el("p", { class: "convert-note" }, tr(
      "Учётные записи не переносятся. Любой, у кого есть файл, откроет базу без входа с полными правами. Пользователи БСП потеряют связь с учётными записями базы.",
      "User accounts are not transferred. Anyone with the file can open the base without login and with full rights. SSL catalog users lose their infobase accounts.")),
    label(tr("Лимит памяти", "Memory limit"), memory), memory,
    el("p", { class: "convert-note" }, tr("auto или размер, например 512MiB, 2GiB.", "auto or a size, such as 512MiB, 2GiB.")));
  const layoutFields = el("fieldset", null, el("legend", null, tr("Параметры выгрузки", "Dump options")),
    label(tr("Формат списка пользователей", "Users layout"), layout), layout,
    el("p", { class: "convert-note" }, tr("Выберите формат исходной базы. Автоматический выбор пока недоступен.", "Choose the source base's users layout. Automatic selection is not yet available.")));
  const update = () => {
    const restore = target.value === "1cd";
    restoreFields.hidden = !restore;
    layoutFields.hidden = target.value !== "dt";
    run.disabled = initializing || running || !valid || !source.value.trim() || !output.value.trim() || (target.value === "dt" && !layout.value);
    controls.forEach(c => { c.disabled = initializing || running; });
    close.disabled = running;
    progress.hidden = !running;
  };
  const inspect = async selected => {
    const mine = ++generation;
    valid = false; update();
    try {
      const next = await invoke("convert_source_info", { path: selected });
      if (mine !== generation || !dialog.isConnected) return;
      rawSource = next.path;
      source.value = display(next.path);
      fill(target, next.targets.map(value => el("option", { value }, ({ "1cd": "1CD", "dt": "DT", "cf": "CF" })[value])));
      platform.textContent = tr("Платформа: ", "Platform: ") + next.platformBuild;
      valid = next.targets.length > 0;
      rawOutput = null;
      output.value = "";
      if (!valid) report(tr("Для этого источника конвертация пока недоступна. Поддерживаются DT и 1CD.", "Conversion is not available for this source yet. DT and 1CD are supported."));
      else clearError();
      update();
    } catch (e) { if (mine === generation) { report(e); update(); } }
  };
  const pickSource = el("button", { type: "button", onclick: async () => {
    try { const picked = await invoke("pick_source", { folder: false }); if (picked) await inspect(picked); } catch (e) { report(e); }
  } }, tr("Выбрать исходник…", "Choose source…"));
  const pickOutput = el("button", { type: "button", onclick: async () => {
    try { const picked = await invoke("pick_conversion_output", { to: target.value }); if (picked) { rawOutput = picked; output.value = display(picked); update(); } } catch (e) { report(e); }
  } }, tr("Куда сохранить…", "Save as…"));
  controls.push(source, target, output, without, memory, layout, pickSource, pickOutput);
  source.addEventListener("input", () => { ++generation; rawSource = null; valid = false; update(); });
  source.addEventListener("change", () => inspect(source.value));
  target.addEventListener("change", () => { rawOutput = null; output.value = ""; clearError(); update(); });
  output.addEventListener("input", () => { rawOutput = null; update(); });
  layout.addEventListener("change", update);
  const stopPoll = () => { clearInterval(polling); polling = null; };
  const render = snapshot => {
    running = snapshot.state === "running";
    if (snapshot.targets?.length) {
      const chosen = target.value || snapshot.to;
      fill(target, snapshot.targets.map(value => el("option", { value }, value.toUpperCase())));
      target.value = chosen;
    }
    elapsed.textContent = snapshot.state === "idle" ? "" : tr("Время: ", "Elapsed: ") + ((snapshot.elapsedMs || 0) / 1000).toFixed(1) + tr(" с", " s");
    if (running) status.textContent = display(snapshot.progress || tr("Выполняется…", "Working…"));
    else if (snapshot.state === "error") { status.textContent = ""; report(snapshot.error); stopPoll(); }
    else if (snapshot.state === "done") {
      const r = snapshot.result || {};
      fill(status, el("strong", null, tr("Готово", "Done")), el("p", null, display(snapshot.output)),
        r.usersDropped ? el("p", null, tr("Не перенесено пользователей: ", "Users not transferred: ") + r.usersDropped) : null,
        (r.warnings || []).map(w => el("p", null, display(w.kind) + ": " + w.items)),
        snapshot.progress ? el("p", null, display(snapshot.progress)) : null);
      stopPoll();
    }
    update();
  };
  let checking = false;
  const poll = async () => {
    if (checking) return;
    checking = true;
    try { render(await invoke("conversion_status")); } catch (e) { report(e); } finally { checking = false; }
  };
  const startPoll = () => { stopPoll(); polling = setInterval(poll, 500); };
  run.addEventListener("click", async () => {
    if (run.disabled) return;
    running = true; clearError(); status.textContent = tr("Запуск…", "Starting…"); update();
    try {
      await invoke("start_conversion", { input: { source: rawSource ?? source.value, output: rawOutput ?? output.value, to: target.value,
        withoutUsers: target.value === "1cd" && without.checked,
        memoryLimit: target.value === "1cd" ? memory.value.trim() || "auto" : null,
        usersLayout: target.value === "dt" ? layout.value : null } });
      startPoll(); await poll();
    } catch (e) { running = false; report(e); update(); }
  });
  dialog.append(el("header", { class: "dialog-caption" }, el("strong", null, title)),
    el("div", { class: "convert-body" },
      el("p", null, tr("Исходный файл не изменяется. Существующий файл результата не заменяется.", "The source stays unchanged. An existing output file is never replaced.")),
      label(tr("Исходный файл", "Source file"), source), el("div", { class: "convert-path" }, source, pickSource),
      label(tr("Формат результата", "Output format"), target), target,
      label(tr("Сохранить в новый файл", "Save to a new file"), output), el("div", { class: "convert-path" }, output, pickOutput),
      restoreFields, layoutFields, platform, error, progress, elapsed, status,
      el("footer", { class: "convert-actions" }, run, close)));
  dialog.addEventListener("cancel", e => { if (running) e.preventDefault(); });
  dialog.addEventListener("close", () => { stopPoll(); ++generation; dialog.remove(); active = null; if (previous?.isConnected) previous.focus(); });
  document.body.append(dialog); dialog.showModal(); update();
  try {
    const snapshot = await invoke("conversion_status");
    if (!dialog.isConnected) return;
    if (snapshot.state !== "idle" && snapshot.source) {
      rawSource = snapshot.source;
      source.value = display(snapshot.source);
      rawOutput = snapshot.output;
      output.value = display(snapshot.output);
      const targets = snapshot.targets?.length ? snapshot.targets : snapshot.to ? [snapshot.to] : [];
      fill(target, targets.map(value => el("option", { value }, value.toUpperCase())));
      target.value = snapshot.to;
      valid = !!snapshot.to;
      without.checked = !!snapshot.withoutUsers;
      memory.value = snapshot.memoryLimit || "auto";
      layout.value = snapshot.usersLayout || "";
      platform.textContent = snapshot.platformBuild ? tr("Платформа: ", "Platform: ") + snapshot.platformBuild : "";
    } else {
      await inspect(path);
      if (!dialog.isConnected) return;
    }
    render(snapshot);
    if (running) startPoll();
  } catch (e) { report(e); }
  finally { initializing = false; update(); }
}
