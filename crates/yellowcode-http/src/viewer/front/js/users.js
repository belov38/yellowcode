import { el, fill } from "./dom.js";
import { desktopBusy } from "./desktop.js";

const WRITE_NOTICE = [
  "Изменения записываются напрямую в файл базы. Сначала закройте базу в 1С и сделайте резервную копию: при сбое записи или питания база может быть повреждена. Автоматической резервной копии и восстановления нет.",
  "Changes are written directly to the infobase file. Close the infobase in 1C and make a backup first: a write or power failure can damage the infobase. There is no automatic backup or recovery.",
];

function enable(node, value, reason = "") {
  node.disabled = !value;
  node.setAttribute("aria-disabled", String(!value));
  node.title = value ? "" : reason;
}

async function read(path, signal) {
  const response = await fetch("/api/" + path, { signal, cache: "no-store" });
  const body = await response.json();
  if (!response.ok) throw { ...body, status: response.status };
  return body;
}

function message(error, tr) {
  const texts = {
    unsupported_source: ["Пользователи доступны для существующей файловой базы .1cd. Этот источник изменить нельзя.", "Users require an existing .1cd infobase. This source cannot be changed."],
    unsupported_platform: ["Изменение пользователей на этой платформе ещё не подтверждено. Доступно только чтение.", "User writes are not qualified on this platform. Read-only access is available."],
    unsupported_layout: ["Этот формат пользователей или разделение данных пока не поддерживает изменение.", "This user format or data separation does not support changes yet."],
    unsupported_policy: ["Пустой пароль или восстановление входа при этих требованиях не подтверждены. Политики и способы входа не изменены. Требования меняются в 1С.", "Empty passwords or login recovery under these requirements are not qualified. Policies and authentication are unchanged. Change requirements in 1C."],
    data_area_read_only: ["Пользователь области данных доступен только для чтения.", "Data-area users are read-only."],
    last_administrator: ["У оставшихся пользователей должен сохраниться административный доступ.", "The remaining users must retain administrative access."],
    stale_revision: ["Данные изменились или карточка уже использована. Обновите карточку перед новой записью.", "The data changed or this card was already used. Refresh the card before another write."],
    write_permission_required: ["Разрешение записи истекло или отозвано. Заново откройте информационную базу. Автоматического повтора нет.", "Write permission expired or was revoked. Reopen the infobase. There is no automatic retry."],
    source_session_closed: ["Источник закрыт или меняется. Заново откройте список пользователей после открытия базы.", "The source is closed or changing. Reopen Users after opening the infobase."],
    invalid_input: ["Значения не приняты. Проверьте имя и роли. Введённые данные сохранены; перед новой записью обновите карточку.", "The values were rejected. Check the name and roles. Unsaved values are retained; refresh before another write."],
    source_busy: ["База занята. Запрос не повторяется автоматически. Обновите карточку перед новой записью.", "The infobase is busy. The request is not retried automatically. Refresh before another write."],
    storage_before_commit: ["Запись не началась. Проверьте доступ к базе и обновите карточку перед новой записью.", "No changes were committed by this request. Check access to the infobase and refresh before another write."],
    commit_unknown: ["Результат записи неизвестен: исходная база могла измениться. Не повторяйте действие. Проверьте состояние базы отдельно; запись в этом окне остановлена.", "The write outcome is unknown: the original infobase may have changed. Do not repeat the action. Inspect the infobase separately; writes in this window are stopped."],
    committed_unverified: ["Изменения записаны, но проверка не завершилась успешно. Не повторяйте действие. Проверьте состояние базы отдельно; запись в этом окне остановлена.", "Changes were committed, but verification failed. Do not repeat the action. Inspect the infobase separately; writes in this window are stopped."],
    last_user_confirmation_required: ["Состав пользователей изменился. Обновите список и подтвердите удаление заново.", "The users changed. Refresh the list and confirm deletion again."],
    not_found: ["Пользователь больше не найден. Обновите список.", "The user no longer exists. Refresh the list."],
  };
  return tr(...(texts[error?.code] || ["Операция отклонена сервисом. Обновите карточку перед новой записью.", "The service refused the operation. Refresh the card before another write."]));
}

function snapshotReason(snapshot, tr) {
  if (snapshot.writable) return "";
  const reason = snapshot.reason || "";
  if (/platform/i.test(reason)) return message({ code: "unsupported_platform" }, tr);
  if (/separat|layout|format/i.test(reason)) return message({ code: "unsupported_layout" }, tr);
  return message({ code: "unsupported_source" }, tr);
}

export function mountUsers(ctx, { tr, icon, openCard }) {
  const list = ctx.nodes.get("users/UserList");
  const body = list.querySelector("tbody");
  const count = ctx.nodes.get("users/TotalUsersCntSt");
  const status = el("div", { class: "users-status", "data-users-status": "", role: "status" });
  ctx.dialog.append(status);
  list.removeAttribute("aria-disabled"); list.removeAttribute("title");
  list.setAttribute("aria-label", tr("Пользователи", "Users"));
  const toolbar = el("div", { class: "dialog-users-toolbar" });
  const menu = el("div", { class: "dialog-users-actions", hidden: true });
  const buttons = [];
  const forms = new Set();
  let snapshot = null, selected = null, sortColumn = "name", descending = false;
  let permission = "", deadline = 0, timer = null, permissionSeen = false;
  let busy = false, alive = true, sourceInvalid = false, generation = 0, needsRefresh = false, blocked = "", permissionBlocked = "", notice = "";
  let sourceLoading = desktopBusy();
  let reader = null, cancelConfirmation = null;
  const current = () => snapshot?.users.find((u) => u.id === selected);
  const area = (card) => Boolean(card?.data_separation?.length);
  const loadingReason = tr("Загружается другой источник. Новая запись приостановлена; результат уже отправленного запроса будет показан.", "Another source is loading. New writes are paused; an admitted request's outcome will still be shown.");
  const writable = (card) => alive && snapshot?.writable && permission && performance.now() < deadline
    && !sourceLoading && !blocked && !permissionBlocked && !needsRefresh && !area(card);
  const readonly = (card) => blocked || permissionBlocked || (sourceLoading ? loadingReason : area(card) ? message({ code: "data_area_read_only" }, tr)
    : needsRefresh ? message({ code: "stale_revision" }, tr) : snapshot ? snapshotReason(snapshot, tr) : "");
  const statusText = (text, card) => blocked || [...new Set([text, permissionBlocked, sourceLoading ? loadingReason : ""].filter(Boolean))].join("\n") || readonly(card);

  function controls() {
    status.textContent = statusText(notice);
    for (const { node, action } of buttons) {
      const yes = !busy && (action === "refresh" ? alive && !sourceInvalid && !sourceLoading : action === "edit" ? Boolean(current())
        : action === "add" ? writable() : writable(current()) && Boolean(current()));
      enable(node, yes, busy ? tr("Операция выполняется", "Operation in progress") : readonly(current()));
    }
    list.setAttribute("aria-busy", String(busy));
    for (const form of forms) form.update();
  }

  function invalidate(code) {
    const reason = message({ code }, tr);
    if (code === "source_session_closed") {
      sourceInvalid = true;
      blocked = notice = reason;
      reader?.abort();
      generation++;
    } else {
      permissionBlocked = reason;
    }
    permission = "";
    permissionSeen = true;
    clearTimeout(timer);
    cancelConfirmation?.();
    controls();
  }
  const sourceChanged = (event) => {
    if (event.detail?.state === "replaced") invalidate("source_session_closed");
    else if (["loading", "retained"].includes(event.detail?.state)) {
      sourceLoading = event.detail.state === "loading";
      if (sourceLoading) cancelConfirmation?.();
      controls();
    }
  };
  const dispose = () => {
    alive = false;
    invalidate("source_session_closed");
    document.removeEventListener("viewer-source-state", sourceChanged);
    window.removeEventListener("pagehide", dispose);
    for (const form of forms) form.ctx.close();
  };
  ctx.dialog.addEventListener("close", dispose, { once: true });
  document.addEventListener("viewer-source-state", sourceChanged);
  window.addEventListener("pagehide", dispose, { once: true });

  function paint() {
    const users = [...(snapshot?.users || [])].sort((a, b) =>
      (descending ? -1 : 1) * a[sortColumn].localeCompare(b[sortColumn]));
    fill(body, users.map((card) => el("tr", {
      "data-user-id": card.id, "aria-selected": String(card.id === selected),
      onclick: () => { if (!busy) { selected = card.id; paint(); } },
      ondblclick: () => { if (!busy) { selected = card.id; showCard(false); } },
    }, el("td", null, card.name), el("td", null, card.full_name))));
    count.textContent = String(users.length);
    controls();
  }
  [...list.querySelectorAll("th")].forEach((th, i) => {
    const key = i ? "full_name" : "name";
    const sort = () => { if (!busy) { descending = sortColumn === key && !descending; sortColumn = key; paint(); } };
    fill(th, el("button", { type: "button", onclick: sort }, th.textContent));
  });

  async function load() {
    const version = ++generation;
    reader?.abort(); reader = new AbortController();
    try {
      const source = await read("x/data/source", reader.signal);
      if (!alive || version !== generation) return false;
      if (!source.base) {
        blocked = message({ code: "unsupported_source" }, tr);
        snapshot = { users: [], roles: [], writable: false };
        selected = null;
        paint();
        return false;
      }
      const { write_permission: incoming, permission_lifetime_seconds: lifetime, ...fresh } = await read("users/write-snapshot", reader.signal);
      if (!alive || version !== generation) return false;
      if (!permissionSeen) {
        permissionSeen = true;
        permission = incoming || "";
        deadline = performance.now() + Math.max(0, Number(lifetime) || 0) * 1000;
        timer = setTimeout(() => invalidate("write_permission_required"), Math.max(0, deadline - performance.now()));
      } else if (incoming !== permission && !blocked && !permissionBlocked) {
        invalidate("write_permission_required");
        return false;
      }
      snapshot = fresh;
      needsRefresh = false;
      if (!snapshot.users.some((u) => u.id === selected)) selected = snapshot.users[0]?.id || null;
      paint();
      return true;
    } catch (error) {
      if (!alive || version !== generation || error?.name === "AbortError") return false;
      notice = message(error, tr);
      if (error.status === 403) invalidate("write_permission_required");
      try {
        const readVersion = generation;
        reader = new AbortController();
        const users = await read("users", reader.signal);
        if (!alive || readVersion !== generation) return false;
        snapshot = { users, roles: [], writable: false };
        if (!users.some((u) => u.id === selected)) selected = users[0]?.id || null;
        paint();
      } catch {}
      return false;
    }
  }

  async function refresh(form) {
    if (busy || !alive || sourceInvalid || sourceLoading) return;
    busy = true; menu.hidden = true; controls();
    notice = blocked;
    try {
      if (await load()) {
        if (form?.alive) form.populate(form.card ? snapshot.users.find((u) => u.id === form.card.id) : null);
      }
    } finally { busy = false; controls(); }
  }

  function confirmation(kind, card, revision) {
    return new Promise((resolve) => {
      const text = kind === "last"
        ? tr(`Удалить последнего пользователя «${card.name}»? После удаления база открывается без входа с полными правами.`, `Delete the sole remaining user “${card.name}”? The infobase will open without login with full rights.`)
        : kind === "clear" ? tr(`Установить пустой пароль пользователю «${card.name}»? Способы входа и политика сохраняются. Восстановление входа не обещается.`, `Set an empty password for “${card.name}”? Authentication and policy are preserved. Login recovery is not guaranteed.`)
          : tr(`Удалить пользователя «${card.name}»?`, `Delete user “${card.name}”?`);
      const d = el("dialog", { class: "service-dialog users-confirm", "data-user-confirm": kind,
        "aria-label": tr("Подтверждение", "Confirmation") });
      let settled = false;
      const finish = (answer) => {
        if (settled) return;
        settled = true; cancelConfirmation = null;
        d.close(); d.remove();
        resolve(Boolean(answer && alive && selected === card.id && snapshot?.revision === revision && writable(card)));
      };
      cancelConfirmation = () => finish(false);
      d.append(el("p", null, text + " " + tr(...WRITE_NOTICE)), el("div", { class: "users-confirm-buttons" },
        el("button", { type: "button", "data-confirm-accept": "", onclick: () => finish(true) }, tr("Подтвердить", "Confirm")),
        el("button", { type: "button", "data-confirm-cancel": "", onclick: () => finish(false) }, tr("Отмена", "Cancel"))));
      d.addEventListener("cancel", (event) => { event.preventDefault(); finish(false); });
      document.body.append(d); d.showModal();
      d.querySelector("[data-confirm-cancel]").focus();
    });
  }

  async function apply(action, form = null) {
    const card = form?.card || current();
    if (busy || !writable(action.action === "create" ? null : card)) return;
    busy = true; menu.hidden = true;
    const revision = snapshot.revision;
    const version = generation;
    controls();
    let allowLast = false;
    try {
      if (["delete", "clear_password"].includes(action.action)) {
        if (!await confirmation(action.action === "delete" ? "delete" : "clear", card, revision)) return;
        if (action.action === "delete" && snapshot.users.length === 1) {
          if (!await confirmation("last", card, revision)) return;
          allowLast = true;
        }
      }
      if (!alive || version !== generation || !writable(action.action === "create" ? null : card)) return;
      needsRefresh = true;
      controls();
      let response, outcome;
      try {
        response = await fetch("/api/users/apply", {
          method: "POST", headers: { "Content-Type": "application/json", "X-User-Write-Permission": permission },
          body: JSON.stringify({ revision, confirmed: true, allow_last_user: allowLast, action }),
        });
        outcome = await response.json();
      } catch {
        throw { code: "commit_unknown" };
      }
      if (!alive || version !== generation) return;
      if (outcome === null || typeof outcome !== "object" || Array.isArray(outcome)) throw { code: "commit_unknown" };
      if (!response.ok) {
        if (outcome.phase !== "before_commit" || outcome.committed !== false) throw {
          code: outcome.phase === "committed_unverified" || outcome.committed === true ? "committed_unverified" : "commit_unknown",
        };
        throw { ...outcome, status: response.status };
      }
      if (!outcome.revision || !Number.isInteger(outcome.users_remaining)) throw { code: "commit_unknown" };
      selected = outcome.user?.id || (outcome.deleted_id === selected ? null : selected);
      notice = action.action === "clear_password"
        ? tr("Пустой пароль записан. Поведение входа не проверено нативной 1С.", "Empty password written. Native 1C login behavior has not been verified.")
        : tr("Изменения записаны.", "Changes saved.");
      if (await load()) {
        if (form?.alive) form.ctx.close();
      } else {
        notice = tr("Изменения записаны, но обновить список не удалось. Обновите его явно; не повторяйте запись.", "Changes were saved, but the list could not be refreshed. Refresh it explicitly; do not repeat the write.");
        if (form?.alive) form.report(notice);
      }
    } catch (error) {
      if (!alive || version !== generation) return;
      if (["commit_unknown", "committed_unverified"].includes(error.code)
        || error.phase === "commit_unknown" || error.phase === "committed_unverified" || error.committed === true) {
        blocked = message({ code: error.code === "committed_unverified" || error.committed === true
          ? "committed_unverified" : "commit_unknown" }, tr);
        permission = ""; clearTimeout(timer);
      } else if (error.status === 403 || ["write_permission_required", "source_session_closed"].includes(error.code)) {
        invalidate(error.code === "source_session_closed" ? error.code : "write_permission_required");
      }
      notice = blocked || message(error, tr);
      if (form?.alive) form.report(notice);
    } finally {
      busy = false;
      if (alive) controls();
    }
  }

  function showCard(create) {
    if (busy || !alive || (create ? !writable() : !current())) return;
    menu.hidden = true;
    openCard({ mount: (cardCtx) => mountCard(cardCtx, create ? null : current()) });
  }
  const handlers = {
    add: () => showCard(true), edit: () => showCard(false), refresh: () => refresh(),
    delete: () => current() && apply({ action: "delete", identity: current().id }),
    clear: () => current() && apply({ action: "clear_password", identity: current().id }),
  };
  const captions = {
    add: tr("Добавить", "Add"), edit: tr("Изменить", "Edit"), delete: tr("Удалить", "Delete"),
    clear: tr("Установить пустой пароль", "Set empty password"), refresh: tr("Обновить", "Refresh"),
  };
  function actionButton(action, picture) {
    const node = el("button", { type: "button", "data-user-action": action,
      "aria-label": captions[action], onclick: handlers[action] }, picture ? icon("pic/" + picture) : captions[action]);
    buttons.push({ node, action });
    return node;
  }
  const toggle = el("button", { type: "button", onclick: () => { if (!busy) menu.hidden = !menu.hidden; } }, tr("Действия ▾", "Actions ▾"));
  toolbar.append(toggle, menu);
  for (const action of ["add", "edit", "delete", "clear", "refresh"]) menu.append(actionButton(action));
  for (const [picture, action] of [["CreateListItem", "add"], ["CloneObject"], ["Change", "edit"], ["Delete", "delete"],
    ["FilterAndSort"], ["FilterByCurrentValue"], ["ClearFilter"], ["Refresh", "refresh"]]) {
    const node = action ? actionButton(action, picture) : el("button", { type: "button", disabled: true,
      title: tr("Не реализовано в Yellowcode", "Not implemented in Yellowcode") }, icon("pic/" + picture));
    toolbar.append(node);
    ctx.nodes.set("users/" + picture, node);
  }
  ctx.body.append(toolbar);
  list.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    if (busy) return;
    const row = event.target.closest("tr[data-user-id]");
    if (row && body.contains(row)) { selected = row.dataset.userId; paint(); }
    menu.hidden = false;
  });
  list.addEventListener("keydown", (event) => {
    if (busy || event.target.closest("button,input,select,textarea,a[href]")) return;
    const rows = [...body.children];
    const index = rows.findIndex((r) => r.dataset.userId === selected);
    if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key) && rows.length) {
      event.preventDefault();
      const next = event.key === "Home" ? 0 : event.key === "End" ? rows.length - 1
        : Math.max(0, Math.min(rows.length - 1, index + (event.key === "ArrowDown" ? 1 : -1)));
      selected = rows[next].dataset.userId; paint();
      body.querySelector('[aria-selected=true]')?.scrollIntoView({ block: "nearest" });
    } else {
      const action = { Enter: "edit", Insert: "add", Delete: "delete", F5: "refresh" }[event.key];
      if (action) { event.preventDefault(); handlers[action](); }
    }
  });

  function mountCard(cardCtx, initial) {
    if (!alive) { cardCtx.close(); return; }
    const get = (key) => cardCtx.nodes.get("user/" + key);
    const note = el("div", { class: "users-status", "data-user-status": "", role: "status" });
    const reload = el("button", { type: "button", "data-user-action": "refresh", onclick: () => refresh(form) },
      tr("Обновить карточку (сбросить изменения)", "Refresh card (discard changes)"));
    cardCtx.dialog.append(el("p", null, tr(...WRITE_NOTICE)), note, reload);
    const form = { ctx: cardCtx, alive: true, card: initial, message: "", update, populate,
      report: (text) => { form.message = text; update(); } };
    forms.add(form);
    cardCtx.dialog.addEventListener("close", () => { form.alive = false; forms.delete(form); }, { once: true });
    const name = get("EditName"), full = get("EditFullName"), roles = get("ListRoles"), show = get("InList").querySelector("input");
    name.setAttribute("aria-label", tr("Имя", "Name")); full.setAttribute("aria-label", tr("Полное имя", "Full name"));
    roles.multiple = true; roles.setAttribute("aria-label", tr("Роли", "Roles"));
    get("InList").removeAttribute("aria-disabled"); get("InList").removeAttribute("title");
    for (const key of ["EditPassword2", "Control112", "PasswordStrength", "PasswordConfirmed", "PasswordGenerationHyperlink", "UseRuleKeyEdit", "8"]) get(key).hidden = true;
    const credentials = get("EditPassword1");
    credentials.removeAttribute("aria-disabled"); credentials.removeAttribute("title");
    const clear = el("button", { type: "button", class: "users-clear", "data-user-action": "clear", onclick: () => {
      if (form.card) apply({ action: "clear_password", identity: form.card.id }, form);
    } }, captions.clear);
    get("Control112").parentElement.append(clear);
    const publicInfo = el("dl", { class: "users-public-info" });
    get("RunModeEdit").parentElement.append(publicInfo);
    get("BtnOK").addEventListener("click", () => {
      if (busy || !writable(form.card)) return;
      if (!name.value.trim()) { form.report(tr("Укажите имя пользователя.", "Enter a user name.")); name.focus(); return; }
      const ids = [...roles.selectedOptions].map((o) => o.value);
      for (const role of form.card?.roles || []) {
        if (!(snapshot?.roles || []).some((r) => r.id === role.id) && !ids.includes(role.id)) ids.push(role.id);
      }
      const draft = { name: name.value, full_name: full.value, roles: ids, show_in_list: show.checked };
      if (!form.card) { apply({ action: "create", draft }, form); return; }
      const patch = {};
      for (const key of ["name", "full_name", "show_in_list"]) if (draft[key] !== form.card[key]) patch[key] = draft[key];
      const prior = form.card.roles.map((r) => r.id);
      if (ids.length !== prior.length || ids.some((id) => !prior.includes(id))) patch.roles = ids;
      if (!Object.keys(patch).length) { cardCtx.close(); return; }
      apply({ action: "update", identity: form.card.id, patch }, form);
    });
    function populate(card) {
      form.card = card;
      if (initial && !card) {
        form.missing = true;
        form.report(message({ code: "not_found" }, tr)); return;
      }
      form.missing = false;
      name.value = card?.name || ""; full.value = card?.full_name || ""; show.checked = card ? card.show_in_list : true;
      const known = snapshot?.roles || [];
      const prior = card?.roles || [];
      fill(roles, known.map((r) => el("option", { value: r.id }, r.name)),
        prior.filter((r) => !known.some((k) => k.id === r.id)).map((r) => el("option", { value: r.id, disabled: true },
          tr("Неизвестная роль: ", "Unresolved role: ") + (r.name || r.id))));
      for (const option of roles.options) option.selected = prior.some((r) => r.id === option.value);
      credentials.textContent = card ? card.password_set ? tr("Установлен", "Set") : tr("Не установлен", "Not set") : tr("Пустой пароль", "Empty password");
      for (const [key, label] of [["EntAuth", "standard"], ["WinAuth", "os"], ["OpenIDAutentication", "openid"],
        ["OpenIDConnectAutentication", "openid_connect"], ["AccessTokenAutentication", "access_token"],
        ["QRCodeAutentication", "qr_code"], ["EmailAuthentication", "email"]]) {
        get(key).title = tr("Только чтение", "Read-only");
        get(key).querySelector("input").checked = Boolean(card ? card.authentication[label] : label === "standard");
      }
      for (const [key, field] of [["EditEmail", "email"], ["WinUser", "os_user"], ["MDRefEditInterface", "default_interface"],
        ["MDRefEditLanguage", "language"], ["RunModeEdit", "run_mode"]]) {
        get(key).value = card?.[field] ?? "";
        get(key).title = tr("Только чтение", "Read-only");
      }
      for (const [key, field] of [["CannotChangePassword", "cannot_change_password"], ["CannotRecoveryPassword", "cannot_recover_password"],
        ["InteractiveSecurity", "unsafe_operation_protection"]]) {
        get(key).querySelector("input").checked = Boolean(card?.[field]);
        get(key).title = tr("Только чтение", "Read-only");
      }
      fill(get("SeparatorsLayouterCtrl"), (card?.data_separation || []).map((s) => el("p", null, s.name + ": " + s.value)));
      fill(publicInfo, card ? [
        [tr("UUID", "UUID"), card.id], [tr("Парольная политика", "Password policy"), card.password_policy],
        [tr("Изменён", "Changed"), card.changed_local], [tr("Пароль изменён (UTC)", "Password changed (UTC)"), card.password_changed_utc],
      ].flatMap(([label, value]) => [el("dt", null, label), el("dd", null, value || "—")]) : []);
      form.report(card ? "" : tr("Новый пользователь создаётся с пустым паролем. Возможность записи проверяет сервис.", "A new user has an empty password. The service checks whether this is permitted."));
    }
    function update() {
      const can = !busy && !form.missing && writable(form.card);
      const why = readonly(form.card);
      for (const node of [name, full, roles, show, get("BtnOK")]) enable(node, can, why);
      enable(clear, can && Boolean(form.card), why);
      clear.hidden = !form.card;
      enable(reload, !busy && alive && !sourceInvalid && !sourceLoading, why);
      note.textContent = statusText(form.message, form.card);
    }
    populate(initial);
  }

  count.textContent = "0";
  busy = true; controls();
  load().finally(() => { busy = false; if (alive) controls(); });
}
