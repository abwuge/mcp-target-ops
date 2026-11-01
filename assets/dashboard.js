"use strict";

const $ = (id) => document.getElementById(id);
const fmtTime = new Intl.DateTimeFormat("zh-CN", {
  month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false,
});
const fmtShortTime = new Intl.DateTimeFormat("zh-CN", {
  month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false,
});
const nf = new Intl.NumberFormat("zh-CN");
const app = {
  hours: 24, client: "", tool: "", status: "", search: "",
  calls: [], clients: [], nextBefore: null, data: null,
  knownClients: new Set(), knownTools: new Set(), loading: false, requestSeq: 0,
  pagesLoaded: 1, reloadQueued: false, detailSeq: 0,
};

function node(tag, className, text) {
  const n = document.createElement(tag);
  if (className) n.className = className;
  if (text !== undefined && text !== null) n.textContent = String(text);
  return n;
}
function prettyDuration(ms) {
  return ms === null || ms === undefined ? "—" :
    ms < 10 ? `${ms.toFixed(2)} ms` : ms < 1000 ? `${ms.toFixed(1)} ms` : `${(ms / 1000).toFixed(2)} s`;
}
function prettyTime(ms) { return ms ? fmtTime.format(new Date(ms)) : "—"; }
function empty(container, message) { container.replaceChildren(node("div", "muted", message)); }

function renderSummary(data) {
  const s = data.summary;
  $("metricCalls").textContent = nf.format(s.count);
  $("metricClients").textContent = nf.format(s.clients);
  $("metricError").textContent = s.count ? `${(s.errors * 100 / s.count).toFixed(1)}%` : "0%";
  $("metricError").style.color = s.errors ? "var(--danger)" : "";
  $("errorDetail").textContent = `${nf.format(s.errors)} 次失败`;
  $("metricLatency").textContent = prettyDuration(s.p95_ms);
  $("avgDetail").textContent = `平均 ${prettyDuration(s.avg_ms)}`;
  $("sideSessionCount").textContent = nf.format(s.clients);
  $("chartWindow").textContent = $("hours").selectedOptions[0].textContent;
}

function renderTrend(data) {
  const container = $("trendChart");
  container.replaceChildren();
  const interval = data.window_hours <= 24 ? 1 : data.window_hours <= 168 ? 6 : 24;
  const count = Math.ceil(data.window_hours / interval);
  const end = Math.floor(Math.floor(Date.now() / 3600000) / interval) * interval;
  const first = end - (count - 1) * interval;
  const buckets = new Map();
  data.trend.forEach((v) => {
    const start = Math.floor(v.hour / interval) * interval;
    const old = buckets.get(start) || { count: 0, errors: 0 };
    old.count += v.count; old.errors += v.errors;
    buckets.set(start, old);
  });
  const max = Math.max(1, ...Array.from(buckets.values(), (v) => v.count));
  for (let i = 0; i < count; i++) {
    const slot = first + i * interval;
    const value = buckets.get(slot) || { count: 0, errors: 0 };
    const bar = node("div", "chart-bar");
    bar.style.height = `${value.count ? Math.max(3, value.count / max * 100) : 1}%`;
    if (!value.count) bar.style.opacity = ".13";
    const errors = node("div", "chart-errors");
    errors.style.height = value.count ? `${100 * value.errors / value.count}%` : "0";
    bar.append(errors);
    bar.title = `${fmtShortTime.format(new Date(slot * 3600000))} · ${value.count} 请求 · ${value.errors} 失败`;
    container.append(bar);
  }
  $("chartStart").textContent = fmtShortTime.format(new Date(first * 3600000));
  $("chartEnd").textContent = fmtShortTime.format(new Date((end + interval) * 3600000));
}

function renderTopTools(data) {
  const container = $("toolsChart");
  container.replaceChildren();
  if (!data.top_tools.length) return empty(container, "当前筛选范围暂无工具调用");
  const max = data.top_tools[0].count || 1;
  data.top_tools.slice(0, 7).forEach((tool) => {
    const row = node("div", "tool-row");
    const info = node("div", "tool-row-info");
    const head = node("div", "tool-row-head");
    head.append(node("span", "tool-label", tool.name));
    const track = node("div", "tool-track");
    const fill = node("div", "tool-fill");
    fill.style.width = `${100 * tool.count / max}%`;
    track.append(fill);
    info.append(head, track);
    row.append(info, node("div", "tool-count", nf.format(tool.count)));
    row.title = `${tool.name} · ${tool.count} 次 · ${tool.errors} 次失败`;
    container.append(row);
  });
}

function setOptions(id, options, placeholder, selected) {
  const select = $(id);
  const values = [...options].filter(Boolean).sort((a, b) => a.localeCompare(b));
  select.replaceChildren(new Option(placeholder, ""));
  values.forEach((value) => select.add(new Option(value, value)));
  if (selected && !values.includes(selected)) select.add(new Option(selected, selected));
  select.value = selected;
}

function updateOptionCatalog(data) {
  data.sessions.forEach((s) => app.knownClients.add(s.client));
  data.top_tools.forEach((tool) => app.knownTools.add(tool.name));
  data.calls.forEach((call) => app.knownTools.add(call.name));
  setOptions("clientFilter", app.knownClients, "全部客户端", app.client);
  setOptions("toolFilter", app.knownTools, "全部工具", app.tool);
  $("statusFilter").value = app.status;
}

function filterClient(name) {
  app.client = app.client === name ? "" : name;
  $("clientFilter").value = app.client;
  $("clearSession").hidden = !app.client;
  refresh();
}

function clientButton(client, sidebar) {
  const btn = node("button", sidebar ? "sidebar-session" : "session-card");
  btn.type = "button";
  if (app.client === client.name) btn.classList.add("selected");
  btn.title = client.name;
  if (sidebar) {
    btn.append(node("div", "session-title", client.name),
      node("div", "session-meta", `${nf.format(client.count)} 次调用`));
  } else {
    const head = node("div", "session-card-head");
    head.append(node("span", "session-client", client.name));
    const bottom = node("div", "session-card-bottom");
    bottom.append(node("span", "", `${nf.format(client.count)} 次调用`),
      node("span", "", fmtShortTime.format(new Date(client.last))));
    btn.append(head, bottom);
  }
  btn.addEventListener("click", () => {
    filterClient(client.name);
    $("calls").scrollIntoView({behavior: "smooth"});
  });
  return btn;
}

function renderSessions(data) {
  const groups = new Map();
  for (const session of data.sessions) {
    const name = session.client || "未知客户端";
    const previous = groups.get(name) || {name, count: 0, last: 0};
    previous.count += Number(session.count) || 0;
    previous.last = Math.max(previous.last, Number(session.last_call_ms) || 0);
    groups.set(name, previous);
  }
  app.clients = [...groups.values()].sort((a, b) => b.last - a.last);
  const sidebar = $("sessionList"), grid = $("sessionGrid");
  sidebar.replaceChildren(); grid.replaceChildren();
  if (!app.clients.length) {
    empty(sidebar, "暂无客户端");
    empty(grid, "暂无调用记录");
    return;
  }
  app.clients.forEach((c) => sidebar.append(clientButton(c, true)));
  app.clients.forEach((c) => grid.append(clientButton(c, false)));
  $("clearSession").hidden = !app.client;
}

function showActiveFilters() {
  const el = $("activeFilters");
  const labels = [];
  if (app.client) labels.push(`客户端: ${app.client}`);
  if (app.tool) labels.push(`工具: ${app.tool}`);
  if (app.status) labels.push(`状态: ${app.status === "error" ? "失败" : "成功"}`);
  if (app.search) labels.push(`搜索: ${app.search}`);
  el.hidden = !labels.length;
  el.textContent = labels.join(" · ");
}

function renderCalls() {
  const body = $("callsTable"); body.replaceChildren();
  showActiveFilters();
  if (!app.calls.length) {
    const tr = node("tr");
    const td = node("td", "empty-cell", "该筛选范围内没有调用记录");
    td.colSpan = 8; tr.append(td); body.append(tr);
  }
  app.calls.forEach((call) => {
    const tr = node("tr");
    const time = node("td", "time-cell", prettyTime(call.started_ms));
    const client = node("td");
    client.append(node("div", "client-cell", call.client));
    const method = node("td", "tool-cell", call.name);
    const operation = node("td", "command-cell");
    let preview = call.command_preview || "—";
    if (preview.startsWith(call.name + " ")) preview = preview.slice(call.name.length + 1).replace(/^path=/, "");
    const commandPreview = node("div",
      call.command_preview ? "command-preview" : "command-fallback", preview);
    operation.title = preview;
    operation.append(commandPreview);
    if (call.cwd) operation.append(node("div", "command-cwd", call.cwd));
    const target = node("td");
    if (call.target) target.append(node("span", "target-pill", call.target));
    else target.append(node("span", "muted", "—"));
    const duration = node("td", "duration-cell", prettyDuration(call.duration_ms));
    const status = node("td");
    status.append(node("span", `status-pill ${call.status}`, call.status === "ok" ? "成功" : "失败"));
    tr.append(time, client, method, operation, target, duration, status, node("td", "more-glyph", "›"));
    tr.tabIndex = 0;
    tr.setAttribute("aria-label", `查看 ${call.name} 调用详情`);
    tr.addEventListener("click", () => showCall(call));
    tr.addEventListener("keydown", (e) => { if (e.key === "Enter") showCall(call); });
    body.append(tr);
  });
  const total = app.data?.summary?.count ?? app.calls.length;
  $("listCount").textContent = `${nf.format(app.calls.length)} / ${nf.format(total)} 条`;
  $("loadMore").hidden = app.nextBefore === null;
  $("loadMore").disabled = app.loading;
}

async function showCall(call) {
  const content = $("drawerContent");
  content.replaceChildren(node("div", "muted small", "加载中…"));
  $("drawerTitle").textContent = DashboardDetail.readableName(call.name);
  const meta = $("drawerMeta");
  meta.replaceChildren();
  const facts = [call.client, call.target, prettyTime(call.started_ms), prettyDuration(call.duration_ms)].filter(Boolean);
  facts.forEach((fact, i) => {
    if (i) meta.append(node("span", "drawer-meta-separator", "·"));
    meta.append(node("span", "", fact));
  });
  meta.append(node("span", "status-pill " + call.status,
    call.status === "ok" ? "成功" : "失败"));
  $("drawerOverlay").hidden = false;
  $("detailDrawer").classList.add("open");
  $("detailDrawer").setAttribute("aria-hidden", "false");
  $("closeDrawer").focus();

  const seq = ++app.detailSeq;
  try {
    const resp = await fetch(`/dashboard/api/call?id=${encodeURIComponent(call.id)}`,
      {credentials: "same-origin", cache: "no-store"});
    if (!resp.ok) throw new Error(resp.status === 401 ? "登录已过期，请重新登录" : "详情加载失败");
    const details = await resp.json();
    if (seq !== app.detailSeq || $("drawerOverlay").hidden) return;
    $("drawerTitle").textContent = DashboardDetail.title(call, details);
    DashboardDetail.render(content, call, details);
  } catch (err) {
    if (seq === app.detailSeq && !$("drawerOverlay").hidden) {
      content.replaceChildren(node("div", "error-bar", err.message));
    }
  }
}

function closeDrawer() {
  ++app.detailSeq;
  $("detailDrawer").classList.remove("open");
  $("detailDrawer").setAttribute("aria-hidden", "true");
  $("drawerOverlay").hidden = true;
}

async function refresh(more = false) {
  if (app.loading) { app.reloadQueued = true; return; }
  app.loading = true;
  const seq = ++app.requestSeq;
  $("refresh").disabled = true;
  $("errorBar").hidden = true;
  const params = new URLSearchParams({ hours: app.hours, limit: 60 });
  if (app.client) params.set("client", app.client);
  if (app.tool) params.set("tool", app.tool);
  if (app.status) params.set("status", app.status);
  if (app.search) params.set("search", app.search);
  if (more && app.nextBefore) params.set("before", app.nextBefore);
  try {
    const response = await fetch(`/dashboard/api?${params}`, { cache: "no-store", credentials: "same-origin" });
    if (!response.ok) throw new Error(`HTTP ${response.status}: ${response.status === 401 ? "认证未通过" : "无法读取活动数据"}`);
    const data = await response.json();
    if (seq !== app.requestSeq) return;
    app.data = data;
    if (more) {
      app.calls.push(...data.calls);
      app.pagesLoaded++;
    } else {
      app.calls = data.calls;
      app.pagesLoaded = 1;
    }
    app.nextBefore = data.next_before;
    updateOptionCatalog(data);
    renderSummary(data); renderTrend(data); renderTopTools(data);
    renderSessions(data); renderCalls();
  } catch (err) {
    $("errorBar").textContent = `读取调用记录失败：${err.message}`;
    $("errorBar").hidden = false;
  } finally {
    app.loading = false;
    $("refresh").disabled = false;
    $("loadMore").disabled = false;
    if (app.reloadQueued) { app.reloadQueued = false; refresh(); }
  }
}

function resetFilters() {
  app.client = ""; app.tool = ""; app.status = ""; app.search = "";
  $("clientFilter").value = ""; $("toolFilter").value = "";
  $("statusFilter").value = ""; $("search").value = "";
  refresh();
}

function bind() {
  $("refresh").addEventListener("click", () => refresh());
  $("hours").addEventListener("change", (e) => {
    app.hours = Number(e.target.value); app.knownClients.clear(); app.knownTools.clear();
    refresh();
  });
  $("clientFilter").addEventListener("change", (e) => { app.client = e.target.value; refresh(); });
  $("toolFilter").addEventListener("change", (e) => { app.tool = e.target.value; refresh(); });
  $("statusFilter").addEventListener("change", (e) => { app.status = e.target.value; refresh(); });
  $("resetFilters").addEventListener("click", resetFilters);
  $("clearSession").addEventListener("click", () => { app.client = ""; $("clientFilter").value = ""; refresh(); });
  $("loadMore").addEventListener("click", () => refresh(true));
  $("closeDrawer").addEventListener("click", closeDrawer);
  $("drawerOverlay").addEventListener("click", closeDrawer);
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") closeDrawer(); });
  let searchDebounce;
  $("search").addEventListener("input", (e) => {
    clearTimeout(searchDebounce);
    searchDebounce = setTimeout(() => { app.search = e.target.value.trim(); refresh(); }, 350);
  });
  document.querySelectorAll("[data-nav]").forEach((a) => {
    a.addEventListener("click", () => {
      document.querySelectorAll("[data-nav]").forEach((v) => v.classList.remove("active"));
      a.classList.add("active");
    });
  });
  setInterval(() => {
    if ($("autoRefresh").checked && !document.hidden && !app.loading && app.pagesLoaded === 1 && $("drawerOverlay").hidden)
      refresh();
  }, 15000);
}
bind();
refresh();
