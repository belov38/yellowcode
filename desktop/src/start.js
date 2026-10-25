(function () {
  "use strict";
  var $ = function (id) { return document.getElementById(id); };
  var tauri = window.__TAURI__;
  var items = [], selected = "", busy = false, oldPath = null;
  var splash = location.pathname === "/splash.html";
  var lang = "ru";
  var texts = {
    ru: {
      "list-title": "Информационные базы", enterprise: "1С:Предприятие", open: "Конфигуратор", add: "Добавить…", edit: "Изменить…", remove: "Удалить", setup: "Настройка…", quit: "Выход", "follow-link": "Перейти по ссылке", "launch-settings-title": "Настройка диалога запуска", "launch-settings-cancel": "Отмена", "skin-settings-open": "Скин…",
      "wizard-title": "Добавление информационной базы", "name-label": "Наименование информационной базы:", "path-label": "Путь к источнику:", "pick-file": "Файл…", "pick-folder": "Папка…", finish: "Готово", "wizard-cancel": "Отмена", "settings-title": "Настройка", "skin-label": "Скин:", "skin-file": "Установить из файла…", "skin-folder": "Установить из папки…", "settings-close": "Закрыть", "splash-title": "Конфигуратор", "splash-description": "Просмотрщик конфигураций 1С", cancel: "Отмена", "confirm-ok": "Удалить", "confirm-cancel": "Отмена",
      editing: "Редактирование информационной базы", confirm: "Удалить источник из списка? Файл останется на диске.", required: "Укажите имя и путь к источнику.", missing: "Источник не найден", unavailable: "Не реализовано в Yellowcode", container: "Чтение контейнера…", "dt-config": "Извлечение CONFIG из DT (только метаданные)…", metadata: "Загрузка метаданных и модулей…", tree: "Построение дерева конфигурации…", statistics: "Подготовка статистики…", builtin: "1С классический"
    },
    en: {
      "list-title": "Information bases", enterprise: "1C:Enterprise", open: "Designer", add: "Add…", edit: "Edit…", remove: "Remove", setup: "Settings…", quit: "Exit", "follow-link": "Follow link", "launch-settings-title": "Startup dialog settings", "launch-settings-cancel": "Cancel", "skin-settings-open": "Skin…",
      "wizard-title": "Add information base", "name-label": "Information base name:", "path-label": "Source path:", "pick-file": "File…", "pick-folder": "Folder…", finish: "Finish", "wizard-cancel": "Cancel", "settings-title": "Settings", "skin-label": "Skin:", "skin-file": "Install from file…", "skin-folder": "Install from folder…", "settings-close": "Close", "splash-title": "Designer", "splash-description": "1C configuration viewer", cancel: "Cancel", "confirm-ok": "Remove", "confirm-cancel": "Cancel",
      editing: "Edit information base", confirm: "Remove the source from the list? Its file will remain on disk.", required: "Enter a source name and path.", missing: "Source not found", unavailable: "Not implemented in Yellowcode", container: "Reading container…", "dt-config": "Extracting DT CONFIG (metadata only)…", metadata: "Loading metadata and modules…", tree: "Building configuration tree…", statistics: "Preparing statistics…", builtin: "1C Classic"
    }
  };
  function t(key) { return texts[lang][key] || key; }
  function translate() {
    document.documentElement.lang = lang;
    Object.keys(texts[lang]).forEach(function (key) { if ($(key) && key !== "confirm") $(key).textContent = t(key); });
    $("enterprise").title = t("unavailable");
    $("follow-link").title = t("unavailable");
    $("open").title = lang === "ru" ? "Запуск в режиме Конфигуратор (Alt+Enter)" : "Start Designer (Alt+Enter)";
    buildLaunchSettings();
    $("skins").setAttribute("aria-label", t("skin-label"));
  }
  function buildLaunchSettings() {
    var host = $("launch-settings-content"); host.textContent = "";
    function text(ru, en) { return lang === "ru" ? ru : en; }
    function node(tag, content, className) {
      var n = document.createElement(tag); if (content) n.textContent = content; if (className) n.className = className; return n;
    }
    function inert(n) { n.disabled = true; n.title = t("unavailable"); return n; }
    function checkbox(ru, en, checked) {
      var label = node("label"); var input = node("input"); input.type = "checkbox"; input.checked = checked;
      label.append(inert(input), text(ru, en)); host.append(label);
    }
    host.append(node("div", text("Настройка отображения списка информационных баз:", "Information base list display settings:")));
    checkbox("Отображать в виде дерева", "Display as a tree", false);
    checkbox("Сортировать по наименованию", "Sort by name", true);
    checkbox("Показывать последние выбранные информационные базы", "Show recently selected information bases", false);
    var count = node("label", text("   Запоминать последние выбранные:", "   Remember recently selected:"));
    var number = node("input"); number.type = "number"; number.value = "4"; count.append(inert(number)); host.append(count);
    [
      ["Каталоги шаблонов конфигураций и обновлений:", "Configuration template and update directories:", ["+", "✎", "×", "↑", "↓"]],
      ["Адреса Интернет-сервисов и списки общих информационных баз:", "Internet service addresses and shared information base lists:", ["+", "✎", "×"]],
      ["Используемые версии:", "Versions used:", ["+", "▣", "✎", "×"]]
    ].forEach(function (section, index) {
      host.append(node("div", text(section[0], section[1]), "setup-section"));
      var toolbar = node("div", null, "setup-toolbar");
      section[2].forEach(function (symbol) { toolbar.append(inert(node("button", symbol))); }); host.append(toolbar);
      var empty = node("div", null, index === 2 ? "setup-versions" : "setup-empty");
      if (index === 2) {
        var table = node("table"), row = node("tr");
        [["Для версии", "For version"], ["Использовать версию", "Use version"], ["Разрядность", "Bitness"]].forEach(function (title) { row.append(node("th", text(title[0], title[1]))); });
        table.append(row); empty.append(table);
      }
      host.append(empty);
    });
    checkbox("Устанавливать автоматически новую версию", "Install new version automatically", true);
    checkbox("Использовать аппаратную лицензию (ключ защиты)", "Use hardware license (protection key)", true);
    checkbox("Разрешать запуск 1C:Enterprise Development Tools", "Allow starting 1C:Enterprise Development Tools", false);
    host.append(inert(node("button", text("Удалять неиспользуемые версии…", "Remove unused versions…"), "setup-link")));
  }
  function error(e) {
    $("error-text").textContent = String(e);
    if (!$("error").open) $("error").showModal();
  }
  function closeError() {
    $("error").close();
    if (splash && tauri && tauri.core) tauri.core.invoke("dismiss_load_error").catch(error);
  }
  $("error-close").onclick = closeError;
  $("error").addEventListener("cancel", function (e) { e.preventDefault(); closeError(); });
  if (!tauri || !tauri.core) { translate(); error("This page works inside the desktop application only."); return; }
  var invoke = tauri.core.invoke;
  function current() { return items.find(function (item) { return item.path === selected; }); }
  function select(path) {
    selected = path;
    var item = current();
    Array.from($("recent").children).forEach(function (row) { row.setAttribute("aria-selected", String(row.dataset.path === path)); });
    $("source-path").textContent = item ? item.path : "";
    $("source-path").title = item ? item.path : "";
    $("open").disabled = busy || !item || !item.exists;
    $("edit").disabled = busy || !item;
    $("remove").disabled = busy || !item;
  }
  function loadRecent() {
    return invoke("recent").then(function (list) {
      items = list;
      $("recent").textContent = "";
      items.forEach(function (item, index) {
        var row = document.createElement("li");
        row.id = "source-" + index;
        row.dataset.path = item.path;
        row.setAttribute("role", "option");
        row.textContent = item.name;
        row.title = item.path + (item.exists ? "" : " — " + t("missing"));
        if (!item.exists) row.classList.add("missing");
        row.onclick = function () { select(item.path); $("recent").focus(); };
        row.ondblclick = function () { select(item.path); openSelected(); };
        $("recent").appendChild(row);
      });
      select(current() ? selected : (items[0] ? items[0].path : ""));
    }).catch(error);
  }
  $("recent").onkeydown = function (e) {
    if (busy) return;
    var index = items.findIndex(function (item) { return item.path === selected; });
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      index = Math.max(0, Math.min(items.length - 1, index + (e.key === "ArrowDown" ? 1 : -1)));
      if (items[index]) { select(items[index].path); $("recent").setAttribute("aria-activedescendant", "source-" + index); $("source-" + index).scrollIntoView({ block: "nearest" }); }
    } else if (e.key === "Enter") openSelected();
    else if (e.key === "Delete") removeSelected();
  };
  function openSelected() {
    var item = current();
    if (busy || !item || !item.exists) return;
    busy = true; select(selected);
    invoke("open_source", { path: item.path }).catch(function (e) { stopLoading(); error(e); });
  }
  function showWizard(edit) {
    if (busy) return;
    var item = edit ? current() : null;
    oldPath = item ? item.path : null;
    $("wizard-title").textContent = t(item ? "editing" : "wizard-title");
    $("source-name").value = item ? item.name : "";
    $("source-input").value = item ? item.path : "";
    invoke("startup_dialog_size", { dialog: "wizard" }).catch(error);
    $("wizard").showModal(); $("source-name").focus();
  }
  $("add").onclick = function () { showWizard(false); };
  $("edit").onclick = function () { showWizard(true); };
  $("wizard-cancel").onclick = function () { $("wizard").close(); };
  $("wizard-close").onclick = $("wizard-cancel").onclick;
  function pickSource(folder) {
    invoke("pick_source", { folder: folder }).then(function (path) {
      if (!path) return;
      $("source-input").value = path;
      var parts = path.replace(/[\\/]$/, "").split(/[\\/]/), base = /^1cv8\.1cd$/i.test(parts[parts.length - 1]) && parts.length > 1;
      if (!$("source-name").value) $("source-name").value = parts[parts.length - (base ? 2 : 1)].replace(/\.(cf|cfe|dt|1cd)$/i, "");
    }).catch(error);
  }
  $("pick-file").onclick = function () { pickSource(false); };
  $("pick-folder").onclick = function () { pickSource(true); };
  function finish() {
    if ($("finish").disabled) return;
    var name = $("source-name").value.trim(), path = $("source-input").value.trim();
    if (!name || !path) { error(t("required")); return; }
    $("finish").disabled = true;
    invoke("save_source", { old: oldPath, path: path, name: name }).then(function (saved) {
      selected = saved || path; $("wizard").close(); return loadRecent();
    }).catch(error).finally(function () { $("finish").disabled = false; });
  }
  $("finish").onclick = finish;
  $("wizard").addEventListener("keydown", function (e) {
    if (e.key === "Enter" && e.target.tagName === "INPUT") { e.preventDefault(); finish(); }
  });
  function removeSelected() {
    if (busy || !current()) return;
    $("confirm-text").textContent = t("confirm") + "\n" + current().name;
    $("confirm").showModal();
  }
  $("remove").onclick = removeSelected;
  $("confirm-cancel").onclick = function () { $("confirm").close(); };
  $("confirm-ok").onclick = function () {
    var path = selected;
    $("confirm").close(); invoke("forget_recent", { path: path }).then(loadRecent).catch(error);
  };
  $("open").onclick = openSelected;
  document.addEventListener("keydown", function (e) {
    if (e.altKey && e.key === "Enter" && !document.querySelector("dialog[open]") && !splash) {
      e.preventDefault(); openSelected();
    }
  });
  $("quit").onclick = function () { invoke("quit").catch(error); };
  $("setup").onclick = function () {
    invoke("startup_dialog_size", { dialog: "settings" }).catch(error);
    $("launch-settings").showModal();
  };
  $("skin-settings-open").onclick = function () { $("settings").showModal(); };
  document.querySelectorAll("[data-launch-close]").forEach(function (b) { b.onclick = function () { $("launch-settings").close(); }; });
  ["wizard", "launch-settings"].forEach(function (id) {
    $(id).addEventListener("close", function () { invoke("startup_dialog_size", { dialog: "" }).catch(error); });
  });
  $("settings-close").onclick = function () { $("settings").close(); };
  function skinList(data) {
    $("skins").textContent = "";
    data.skins.forEach(function (skin) {
      var option = document.createElement("option"); option.value = skin.id;
      option.textContent = skin.id === "builtin" ? t("builtin") : skin.name;
      option.selected = skin.id === data.active; $("skins").appendChild(option);
    });
  }
  function installSkin(folder) {
    invoke("pick_skin", { folder: folder }).then(function (path) {
      if (path) return invoke("install_skin", { path: path }).then(function () { location.reload(); });
    }).catch(error);
  }
  $("skins").onchange = function () { invoke("select_skin", { id: $("skins").value }).then(function () { location.reload(); }).catch(error); };
  $("skin-file").onclick = function () { installSkin(false); };
  $("skin-folder").onclick = function () { installSkin(true); };
  function showLoading(status) {
    busy = true;
    if (!splash) { select(selected); return; }
    $("launcher").hidden = true; $("splash").hidden = false;
    $("loading-path").textContent = status.path;
    var stage = status.stage || "container";
    $("stage").textContent = t(stage);
    var value = ["container", "metadata", "tree", "statistics"].indexOf(stage) + 1;
    $("progress").setAttribute("aria-valuenow", value);
    $("progress").setAttribute("aria-valuetext", t(stage));
    $("progress-fill").style.width = (value * 25) + "%";
  }
  function stopLoading() { busy = false; select(selected); }
  function cancel() { if (busy) invoke("cancel_load").catch(error); }
  $("cancel").onclick = cancel;
  document.addEventListener("keydown", function (e) { if (e.key === "Escape" && busy) { e.preventDefault(); cancel(); } });
  Promise.all([
    tauri.event.listen("source-loading", function (e) { showLoading(e.payload); }),
    tauri.event.listen("source-progress", function (e) { if (e.payload.state === "loading") showLoading(e.payload); }),
    tauri.event.listen("source-cancelled", stopLoading),
    tauri.event.listen("source-ready", function () { if (!splash) location.href = "/"; }),
    tauri.event.listen("source-error", function (e) { stopLoading(); if (splash) error(e.payload.path + "\n" + e.payload.error); else loadRecent(); })
  ]).then(function () { return Promise.all([invoke("startup_language"), invoke("startup_version")]); }).then(function (values) {
    lang = values[0] === "en" ? "en" : "ru"; translate();
    $("app-version").textContent = values[1];
    if (splash) { $("launcher").hidden = true; $("splash").hidden = false; }
    else { loadRecent(); invoke("list_skins").then(skinList).catch(error); }
    return invoke("status");
  }).then(function (status) {
    if (status.state === "loading") showLoading(status);
    else if (status.state === "ready" && !splash) location.href = "/";
    else if (status.state === "error" && splash) error(status.path + "\n" + status.error);
  }).catch(error);
})();
