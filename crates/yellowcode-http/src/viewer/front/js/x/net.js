import { api } from "../api.js";

export const xget = (route, params, signal) => api("x/query/" + route, params, signal);

function post(prefix, route, body, params) {
  const q = [];
  for (const k of Object.keys(params || {})) q.push(encodeURIComponent(k) + "=" + encodeURIComponent(params[k]));
  const url = "/api/" + prefix + route + (q.length ? "?" + q.join("&") : "");
  return fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body === undefined ? {} : body),
  }).then((r) =>
    r.json().then(
      (j) => {
        if (!r.ok) throw new Error((j && j.error) || String(r.status));
        return j;
      },
      () => {
        throw new Error(String(r.status));
      },
    ),
  );
}

export const xpost = (route, body, params) => post("x/query/", route, body, params);

export const qget = (route, params, signal) => api("x/qb/" + route, params, signal);
export const qpost = (route, body, params) => post("x/qb/", route, body, params);

export const isErrorOut = (j) => !!j && typeof j === "object" && !Array.isArray(j) && typeof j.phase === "string" && Array.isArray(j.chain);
