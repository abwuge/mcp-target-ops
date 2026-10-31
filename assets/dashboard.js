"use strict";

const $ = (id) => document.getElementById(id);
const GROUPS = {
  mcp_session: ["MCP Session", false],
  credential_group: ["凭证归组", true],
  anonymous_group: ["匿名归组", true],
  stdio_process: ["stdio 进程", true],
};
const fmtTime = new Intl.DateTimeFormat("zh-CN", {
  month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false,
});
const fmtShortTime = new Intl.DateTimeFormat("zh-CN", {
  month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false,
});
const nf = new Intl.NumberFormat("zh-CN");
const app = {
  hours: 24, client: "", session: "", tool: "", status: "", search: "",
  calls: [], sessions: [], nextBefore: null, data: null,
  knownClients: new Set(), knownTools: new Set(), loading: false, requestSeq: 0,
  pagesLoaded: 1, reloadQueued: false,
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
function idLabel(key) {
  if (!key) return "—";
  const raw = key.split(":").slice(-1)[0];
  return raw.length > 22 ? `${raw.slice(0, 11)}…${raw.slice(-5)}` : raw;
}
function groupName(grouping) { return (GROUPS[grouping] || ["未知来源", true])[0]; }
function groupApprox(grouping) { return (GROUPS[grouping] || ["", true])[1]; }
function empty(container, message) { container.replaceChildren(node("div", "muted", message)); }

function renderSummary(data) {
  const s = data.summary;
  $("metricCalls").textContent = nf.format(s.count);
  $("metricSessions").textContent = nf.format(s.sessions);
  $("metricClients").textContent = nf.format(s.clients);
  $("metricError").textContent = s.count ? `${(s.errors * 100 / s.count).toFixed(1)}%` : "0%";
  $("metricError").style.color = s.errors ? "var(--orange)" : "";
  $("errorDetail").textContent = `${nf.format(s.errors)} 次失败`;
  $("metricLatency").textContent = prettyDuration(s.p95_ms);
  $("avgDetail").textContent = `平均耗时 ${prettyDuration(s.avg_ms)}`;
  $("sideSessionCount").textContent = nf.format(s.sessions);
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
    head.append(node("span", "tool-label", tool.name),
      node("span", "tool-percent", `${(tool.count * 100 / max).toFixed(0)}%`));
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

function sessionClick(key) {
  app.session = app.session === key ? "" : key;
  if (app.session) app.client = "";
  $("clientFilter").value = app.client;
  refresh();
}

function sessionButton(s, sidebar) {
  const btn = node("button", sidebar ? "sidebar-session" : "session-card");
  btn.type = "button";
  if (app.session === s.key) btn.classList.add("selected");
  btn.title = `${s.client} · ${groupName(s.grouping)}\n${s.key}`;
  if (sidebar) {
    const line = node("div", "session-line");
    line.append(node("span", "session-title", s.client));
    btn.append(line, node("span", "session-id", idLabel(s.key)));
    const meta = node("div", "session-meta");
    meta.append(node("span", "", `${s.count} 次`),
      document.createTextNode(` · ${groupName(s.grouping)}`));
    btn.append(meta);
  } else {
    const head = node("div", "session-card-head");
    head.append(node("span", "session-client", s.client));
    head.lastChild.title = s.client;
    const badge = node("span", "group-label", groupName(s.grouping));
    if (groupApprox(s.grouping)) badge.classList.add("approx");
    head.append(badge);
    const bottom = node("div", "session-card-bottom");
    const count = node("span");
    count.append(node("strong", "", nf.format(s.count)), document.createTextNode(" 次调用"));
    const when = node("span", "", fmtShortTime.format(new Date(s.last_call_ms)));
    bottom.append(count, when);
    btn.append(head, node("div", "session-card-id", idLabel(s.key)), bottom);
  }
  btn.addEventListener("click", () => { sessionClick(s.key); document.getElementById("calls").scrollIntoView({behavior:"smooth"}); });
  return btn;
}

function renderSessions(data) {
  app.sessions = data.sessions;
  const sidebar = $("sessionList"), grid = $("sessionGrid");
  sidebar.replaceChildren(); grid.replaceChildren();
  if (!app.sessions.length) {
    empty(sidebar, "暂无会话");
    empty(grid, "当前范围暂无会话或归组");
    return;
  }
  app.sessions.slice(0, 60).forEach((s) => sidebar.append(sessionButton(s, true)));
  app.sessions.slice(0, 12).forEach((s) => grid.append(sessionButton(s, false)));
}

function showActiveFilters() {
  const el = $("activeFilters");
  const labels = [];
  if (app.session) labels.push(`会话: ${idLabel(app.session)}`);
  if (app.client) labels.push(`客户端: ${app.client}`);
  if (app.tool) labels.push(`工具: ${app.tool}`);
  if (app.status) labels.push(`状态: ${app.status === "error" ? "失败" : "成功"}`);
  if (app.search) labels.push(`搜索: ${app.search}`);
  el.hidden = !labels.length;
  el.textContent = labels.length ? `当前筛选 · ${labels.join(" / ")}` : "";
}

function renderCalls() {
  const body = $("callsTable"); body.replaceChildren();
  showActiveFilters();
  if (!app.calls.length) {
    const tr = node("tr");
    const td = node("td", "empty-cell", "该筛选范围内没有调用记录");
    td.colSpan = 7; tr.append(td); body.append(tr);
  }
  app.calls.forEach((call) => {
    const tr = node("tr");
    const time = node("td", "time-cell", prettyTime(call.started_ms));
    const client = node("td");
    client.append(node("div", "client-cell", call.client),
      node("span", "mini-session", idLabel(call.session_key)));
    const method = node("td", "tool-cell", call.name);
    const target = node("td");
    if (call.target) target.append(node("span", "target-pill", call.target));
    else target.append(node("span", "muted", "—"));
    const duration = node("td", "duration-cell", prettyDuration(call.duration_ms));
    const status = node("td");
    status.append(node("span", `status-pill ${call.status}`, call.status === "ok" ? "成功" : "失败"));
    tr.append(time, client, method, target, duration, status, node("td", "more-glyph", "›"));
    tr.tabIndex = 0;
    tr.setAttribute("aria-label", `查看 ${call.name} 调用详情`);
    tr.addEventListener("click", () => showCall(call));
    tr.addEventListener("keydown", (e) => { if (e.key === "Enter") showCall(call); });
    body.append(tr);
  });
  $("listCount").textContent = `已显示 ${nf.format(app.calls.length)} 条调用 · 每次最多返回 200 条`;
  $("loadMore").hidden = app.nextBefore === null;
  $("loadMore").disabled = app.loading;
}

function detailRow(parent, key, value) {
  const row = node("div", "detail-row");
  row.append(node("span", "detail-name", key), node("span", "detail-value", value ?? "—"));
  parent.append(row);
}
function detailSection(parent, name) {
  const section = node("div", "drawer-section");
  section.append(node("div", "drawer-section-title", name));
  parent.append(section);
  return section;
}
function showCall(call) {
  const container = $("drawerContent"); container.replaceChildren();
  const s = app.sessions.find((session) => session.key === call.session_key);
  let section = detailSection(container, "EXECUTION");
  detailRow(section, "事件 ID", `#${call.id}`);
  detailRow(section, "调用方法", call.name);
  detailRow(section, "类别", call.kind === "tool" ? "MCP 工具" : "MCP 协议");
  detailRow(section, "时间", prettyTime(call.started_ms));
  detailRow(section, "耗时", prettyDuration(call.duration_ms));
  detailRow(section, "结果", call.status === "ok" ? "成功" : "失败");
  detailRow(section, "JSON-RPC 错误码", call.rpc_error_code === null ? "—" : call.rpc_error_code);
  detailRow(section, "目标", call.target || "未指定");
  section = detailSection(container, "CALLER");
  detailRow(section, "客户端", call.client);
  detailRow(section, "ClientInfo 版本", call.client_version || "未知");
  detailRow(section, "会话类型", groupName(call.grouping));
  detailRow(section, "传输", call.transport.toUpperCase());
  detailRow(section, "归组键", call.session_key);
  if (s) {
    detailRow(section, "Mcp-Session-Id", s.session_id || "无");
    detailRow(section, "OAuth 客户端", s.oauth_client_name || "未知");
    detailRow(section, "User-Agent", s.user_agent || "未知");
    detailRow(section, "结束时间", s.ended_ms ? prettyTime(s.ended_ms) : "未收到结束通知");
  }
  section = detailSection(container, "ARGUMENT FIELDS");
  if (call.argument_keys.length) {
    call.argument_keys.forEach((key) => section.append(node("span", "argument-key", key)));
  } else {
    section.append(node("div", "muted small", "无参数字段"));
  }
  const note = node("div", "drawer-note",
    "隐私保护：只记录参数名称，不存储命令、文件内容、完整请求、返回值或 Bearer Token。只有带有实际 Session ID 的请求可作为可验证的协议会话归组。");
  container.append(note);
  $("drawerOverlay").hidden = false;
  $("detailDrawer").classList.add("open");
  $("detailDrawer").setAttribute("aria-hidden", "false");
  $("closeDrawer").focus();
}

function closeDrawer() {
  $("detailDrawer").classList.remove("open");
  $("detailDrawer").setAttribute("aria-hidden", "true");
  $("drawerOverlay").hidden = true;
}

async function refresh(more = false) {
  if (app.loading) { app.reloadQueued = true; return; }
  app.loading = true;
  const seq = ++app.requestSeq;
  $("refresh").disabled = true;
  $("connectionState").textContent = "正在同步";
  $("errorBar").hidden = true;
  const params = new URLSearchParams({ hours: app.hours, limit: 60 });
  if (app.session) params.set("session", app.session);
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
    $("connectionState").textContent = "已同步";
  } catch (err) {
    $("connectionState").textContent = "连接失败";
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
  app.client = ""; app.session = ""; app.tool = ""; app.status = ""; app.search = "";
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
  $("clientFilter").addEventListener("change", (e) => { app.client = e.target.value; app.session = ""; refresh(); });
  $("toolFilter").addEventListener("change", (e) => { app.tool = e.target.value; refresh(); });
  $("statusFilter").addEventListener("change", (e) => { app.status = e.target.value; refresh(); });
  $("resetFilters").addEventListener("click", resetFilters);
  $("clearSession").addEventListener("click", () => { app.session = ""; refresh(); });
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
