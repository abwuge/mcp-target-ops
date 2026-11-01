"use strict";

const DashboardDetail = (() => {
  const labels = {
    exec: "执行命令", exec_start: "执行命令", exec_batch: "批量命令",
    job: "后台任务", job_wait: "后台任务", job_output: "后台任务",
    job_poll: "后台任务", job_cancel: "取消任务",
    read: "读取文件", file_read: "读取文件", file_find: "搜索文件",
    write: "文件修改", file_edit: "文件修改", file_patch: "文件修改",
    file_write: "写入文件", file_import: "导入文件", file_export: "导出文件",
    file_delete: "删除文件", file_move: "移动文件",
    file_backup: "备份文件", file_restore: "恢复文件",
    file_backup_list: "备份记录", file_backup_delete: "删除备份",
    file_transfer: "传输文件", file_list: "目录内容",
    directory_create: "创建目录", file_chmod: "修改权限",
    mcp_tool_call: "MCP 工具调用", mcp_tools_list: "MCP 工具列表",
    target_select: "切换目标", target_connect: "连接目标",
    target_disconnect: "断开连接", server_info: "服务状态",
  };
  const create = (tag, cls, text) => {
    const item = document.createElement(tag);
    if (cls) item.className = cls;
    if (text !== undefined && text !== null) item.textContent = String(text);
    return item;
  };
  function readableName(name) { return labels[name] || name; }
  function smallPath(value) {
    return String(value || "").replace(/\/$/, "").split("/").pop() || String(value || "");
  }
  function pathFrom(args, result) {
    return result?.path || result?.destination_path || args?.path ||
      result?.source_path || args?.destination || args?.source_path || "";
  }
  function title(call, details) {
    const args = details?.arguments || {}, result = details?.result || {};
    const path = pathFrom(args, result);
    if (path && /^(read|file_|write|directory_)/.test(call.name)) {
      return `${readableName(call.name)} · ${smallPath(path)}`;
    }
    if (call.name === "mcp_tool_call" && args.tool) return args.tool;
    return readableName(call.name);
  }
  function codeBox(parent, label, value, className = "") {
    const frame = create("section", "detail-code-frame " + className);
    const header = create("div", "detail-code-head");
    header.append(create("strong", "", label));
    const copy = create("button", "log-copy", "复制");
    copy.type = "button";
    copy.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(String(value));
        copy.textContent = "已复制";
      } catch (_) { copy.textContent = "复制失败"; }
    });
    header.append(copy);
    frame.append(header, create("pre", "detail-code-body", String(value)));
    parent.append(frame);
    return frame;
  }
  function infoLine(parent, text, className="") {
    if (text === undefined || text === null || String(text) === "") return;
    parent.append(create("div", "detail-info " + className, text));
  }
  function filePath(parent, path) {
    if (path) parent.append(create("div", "detail-file-path", path));
  }
  function responseStatus(parent, result) {
    if (!result || typeof result !== "object") return;
    if (result.timed_out === true) infoLine(parent, "执行超时", "detail-error-text");
    else if (typeof result.exit_code === "number" && result.exit_code !== 0) {
      infoLine(parent, `退出码 ${result.exit_code}`, "detail-error-text");
    }
    if (result.cancel_requested === true) infoLine(parent, "已请求取消");
    else if (result.status === "running") infoLine(parent, "执行中");
    if (typeof result.job_id === "string") infoLine(parent, `任务 ${result.job_id}`);
    if (result.error) {
      const message = typeof result.error === "string" ? result.error :
        result.error.message || "";
      if (message) infoLine(parent, message, "detail-error-text");
    }
  }
  function resultStreams(parent, result) {
    if (!result || typeof result !== "object") return false;
    responseStatus(parent, result);
    let shown = false;
    if (typeof result.stdout === "string" && result.stdout) {
      codeBox(parent, "输出", result.stdout);
      shown = true;
    }
    if (typeof result.stderr === "string" && result.stderr) {
      codeBox(parent, "错误输出", result.stderr, "stderr");
      shown = true;
    }
    return shown;
  }
  function batch(parent, args, result) {
    const commands = Array.isArray(args?.commands) ? args.commands : [];
    const outputs = Array.isArray(result?.results) ? result.results : [];
    if (!outputs.length && !commands.length) return false;
    if (typeof result.succeeded === "number" && typeof result.failed === "number") {
      const parts = [`${result.succeeded} 成功`];
      if (result.failed) parts.push(`${result.failed} 失败`);
      const total = Number(result.requested_count);
      const executed = Number(result.executed_count);
      if (Number.isFinite(total) && Number.isFinite(executed) && total > executed) {
        parts.push(`${total - executed} 未执行`);
      }
      infoLine(parent, parts.join(" · "), "batch-summary");
    }
    const byIndex = new Map(outputs.map((x, i) => [Number.isInteger(x.index) ? x.index : i, x]));
    for (let i = 0; i < Math.max(commands.length, outputs.length); i++) {
      const cmd = commands[i];
      const command = typeof cmd === "string" ? cmd : cmd?.command;
      const item = byIndex.get(i);
      const card = create("div", "detail-batch-item");
      const heading = create("div", "detail-batch-head");
      heading.append(create("strong", "", `命令 ${i + 1}`));
      if (item) {
        const state = item.status === "running" ? "执行中" :
          item.success === false ? "失败" : item.success === true ? "成功" : "";
        if (state) heading.append(create("span",
          "detail-mini-status " + (state === "失败" ? "failed" : ""), state));
      } else if (result.done) {
        heading.append(create("span", "detail-mini-status", "未执行"));
      }
      card.append(heading);
      if (command) codeBox(card, "命令", command, "command");
      if (item) {
        resultStreams(card, item);
        if (item.error) infoLine(card, item.error, "detail-error-text");
      }
      parent.append(card);
    }
    return true;
  }
  function command(parent, args, result, details) {
    if (Array.isArray(args?.commands) || Array.isArray(result?.results)) {
      return batch(parent, args, result);
    }
    const commandText = details?.command || args?.command;
    if (typeof commandText === "string" && commandText) {
      codeBox(parent, "命令", commandText, "command");
    }
    if (details?.cwd || args?.cwd) filePath(parent, details.cwd || args.cwd);
    const shown = resultStreams(parent, result);
    return Boolean(commandText || shown || result?.job_id);
  }
  function diffView(parent, diff, label="文件差异") {
    if (!diff) return false;
    const section = create("section", "detail-diff-section");
    section.append(create("div", "detail-section-heading", label));
    const surface = create("div", "detail-diff");
    DashboardDiff.renderDiff(surface, diff);
    section.append(surface);
    parent.append(section);
    return true;
  }
  function fullFile(parent, content, kind, encoding, label) {
    if (typeof content !== "string") return false;
    const section = create("section", "detail-diff-section");
    section.append(create("div", "detail-section-heading", label));
    const surface = create("div", "detail-diff");
    DashboardDiff.renderContent(surface, content, kind, encoding);
    section.append(surface);
    parent.append(section);
    return true;
  }
  function matchedLines(parent, entries) {
    if (!Array.isArray(entries)) return false;
    const matches = entries.filter(x => x?.kind === "match" || x?.kind === "context");
    if (!matches.length) return false;
    const box = create("div", "search-results");
    for (const item of matches.slice(0, 160)) {
      const path = item.path || "";
      const row = create("div", item.kind === "context" ? "search-line context" : "search-line");
      row.append(create("div", "search-location", `${smallPath(path)}:${item.line ?? ""}`),
        create("pre", "search-source", item.text || ""));
      row.title = path;
      box.append(row);
    }
    parent.append(box);
    return true;
  }
  function readFile(parent, args, result) {
    if (Array.isArray(result.matches)) {
      infoLine(parent, `${result.matches.length} 处匹配`, "detail-result-count");
      return matchedLines(parent, result.matches.map(x => ({kind:"match", path:args.path,
        line:x.line, text:x.text}))) || true;
    }
    if (Array.isArray(result.results) && result.match_count !== undefined) {
      infoLine(parent, `${result.match_count} 处匹配`, "detail-result-count");
      return matchedLines(parent, result.results) || true;
    }
    if (Array.isArray(result.files)) {
      let shown = false;
      for (const file of result.files) {
        filePath(parent, file.path);
        if (!file.success) {
          infoLine(parent, file.error || "读取失败", "detail-error-text");
          continue;
        }
        if (file.encoding === "base64") {
          infoLine(parent, "二进制文件"); shown = true;
        } else if (typeof file.content === "string") {
          codeBox(parent, "文件内容", file.content); shown = true;
        }
      }
      return shown;
    }
    if (result.encoding === "base64") {
      infoLine(parent, "二进制文件");
      return true;
    }
    if (typeof result.content === "string") {
      codeBox(parent, "文件内容", result.content);
      return true;
    }
    return false;
  }
  function fileChange(parent, call, args, result) {
    const path = pathFrom(args, result);
    filePath(parent, path);
    if (result?.written === false) {
      infoLine(parent, result?.changed === false ? "内容未变化" : "预览 · 未写入");
    } else if (result?.restored || result?.deleted || result?.created || result?.written) {
      const verb = result.deleted ? "已删除" : result.restored ? "已恢复" :
        result.created ? "已创建" : "已保存";
      infoLine(parent, verb, "detail-result-count");
    }
    if (call.status === "error") return Boolean(path);
    if (call.name === "file_delete" && result.encoding === "base64") {
      infoLine(parent, "二进制文件");
      return true;
    }
    if ((call.name === "file_delete") && result.encoding !== "base64") {
      return fullFile(parent, result.content, "deleted", result.encoding, "删除的内容");
    }
    if (result.created === true && (args.encoding !== "base64") &&
        (result.encoding !== "base64") && typeof args.content === "string") {
      return fullFile(parent, args.content, "added", "utf-8", "新增内容");
    }
    if (result.created && (args.encoding === "base64" || result.encoding === "base64")) {
      infoLine(parent, "二进制文件");
      return true;
    }
    const diff = typeof result.diff === "string" && result.diff ?
      result.diff : typeof args.patch === "string" && result.changed !== false ?
        args.patch : "";
    if (diff) return diffView(parent, diff);
    if (Array.isArray(result.files) && result.files.length) {
      result.files.forEach(item => infoLine(parent,
        `${item.changed ? "已修改" : "无变化"} · ${item.path}`));
      return true;
    }
    return Boolean(path);
  }
  function backup(parent, args, result) {
    if (result.backup) {
      const b = result.backup;
      filePath(parent, b.path);
      infoLine(parent, "备份已创建");
      if (b.expires_at_unix_secs) {
        infoLine(parent, "有效期至 " + new Date(b.expires_at_unix_secs * 1000).toLocaleString("zh-CN"));
      }
      return true;
    }
    if (Array.isArray(result.backups)) {
      if (!result.backups.length) { infoLine(parent, "没有备份"); return true; }
      for (const b of result.backups.slice(0, 25)) {
        const card = create("div", "backup-item");
        card.append(create("strong", "", b.path || "备份"),
          create("span", "", b.note || new Date(b.created_at_unix_secs * 1000).toLocaleString("zh-CN")));
        parent.append(card);
      }
      if (result.backups.length > 25) infoLine(parent, "显示前 25 条", "detail-truncated");
      return true;
    }
    return fileChange(parent, {name:"file_restore"}, args, result);
  }
  function generic(parent, args, result) {
    if (!result || typeof result !== "object") {
      if (typeof result === "string") { codeBox(parent, "结果", result); return true; }
      return false;
    }
    let content = result;
    if (result.result && typeof result.result === "object") {
      content = result.result.structuredContent || result.result;
    }
    if (Array.isArray(content?.content)) {
      const lines = content.content.filter(x=>x.type === "text" && typeof x.text === "string");
      if (lines.length) {
        codeBox(parent, "结果", lines.map(x=>x.text).join("\n"));
        return true;
      }
    }
    if (typeof content.stdout === "string" || typeof content.stderr === "string") {
      return resultStreams(parent, content);
    }
    const preferred = [
      ["path","文件"],["source_path","来源"],["destination_path","目标文件"],
      ["source","来源"],["destination","目标"],["mode","权限"],
      ["target","目标"],["active_target","当前目标"],
      ["connected","已连接"],["status","状态"],["job_id","任务"],
      ["name","名称"],["message","信息"],["bytes","大小（字节）"],
      ["written","已写入"],["deleted","已删除"],["restored","已恢复"],
      ["created","已创建"],["moved","已移动"],
      ["count","数量"],["total_matches","数量"],["transferred","已传输"],
    ];
    let shown = false;
    const labels = new Set();
    for (const [key, name] of preferred) {
      const value = content[key];
      if (value === null || value === undefined || typeof value === "object") continue;
      labels.add(key);
      infoLine(parent, `${name}：${typeof value === "boolean" ? (value ? "是" : "否") : value}`);
      shown = true;
    }
    if (shown) return true;
    for (const [key, value] of Object.entries(content).slice(0, 8)) {
      if (labels.has(key) || value === null || value === undefined) continue;
      if (typeof value === "string" || typeof value === "number") {
        infoLine(parent, `${key}：${value}`);
        shown = true;
      }
    }
    return shown;
  }
  function rawSection(parent, args, result) {
    const details = create("details", "raw-details");
    details.append(create("summary", "", "原始数据"));
    const content = create("div", "raw-content");
    if (args !== undefined && args !== null) codeBox(content, "调用参数", JSON.stringify(args, null, 2));
    if (result !== undefined && result !== null) codeBox(content, "返回数据", JSON.stringify(result, null, 2));
    details.append(content);
    parent.append(details);
  }
  function render(parent, call, details) {
    parent.replaceChildren();
    if (!details?.available) {
      infoLine(parent, "这条记录没有保存详细内容");
      return;
    }
    const args = details.arguments || {};
    const result = details.result || {};
    const name = call.name;
    const shell = /^(exec|exec_start|exec_batch|job|job_wait|job_poll|job_output|job_cancel)$/.test(name);
    const isRead = /^(read|file_read|file_find)$/.test(name);
    const isBackup = /^(file_backup|file_restore|file_backup_list|file_backup_delete)$/.test(name);
    const isChange = /^(write|file_write|file_edit|file_patch|file_delete|file_import)$/.test(name);
    let rendered = false;
    if (shell) rendered = command(parent, args, result, details);
    else if (isRead) {
      filePath(parent, pathFrom(args, result));
      rendered = readFile(parent, args, result);
    } else if (isBackup) rendered = backup(parent, args, result);
    else if (isChange) rendered = fileChange(parent, call, args, result);
    else rendered = generic(parent, args, result);

    if (call.status === "error" && !shell) {
      if (typeof result.error === "string") infoLine(parent, result.error, "detail-error-text");
      else if (result.error?.message) infoLine(parent, result.error.message, "detail-error-text");
      else if (call.rpc_error_code != null) infoLine(parent,
        `调用失败（${call.rpc_error_code}）`, "detail-error-text");
    }
    if (details.request_truncated || details.response_truncated) {
      infoLine(parent, "部分内容已截断", "detail-truncated");
    }
    if (!rendered && !parent.childNodes.length) infoLine(parent, "没有附加内容");
    rawSection(parent, args, result);
  }
  return {render, title, readableName};
})();