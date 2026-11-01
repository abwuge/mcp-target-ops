"use strict";

// Diff rows and scrolling behavior follow the former file-change MCP App.
const DashboardDiff = (() => {
  const MAX_LINES = 1800;
  const el = (tag, cls, value) => {
    const item = document.createElement(tag);
    if (cls) item.className = cls;
    if (value !== undefined) item.textContent = String(value);
    return item;
  };
  function row(oldNo, newNo, text, kind) {
    const line = el("div", "diff-line" + (kind ? " " + kind : ""));
    line.append(
      el("span", "diff-gutter", oldNo ?? ""),
      el("span", "diff-gutter", newNo ?? ""),
      el("span", "diff-text", text)
    );
    return line;
  }
  function addScroll(root) {
    const viewport = el("div", "diff-viewport");
    const table = el("div", "diff-code");
    viewport.append(table);
    root.append(viewport);
    return table;
  }
  function note(root, text) {
    root.append(el("div", "diff-notice", text));
  }
  function renderContent(root, content, kind, encoding) {
    root.replaceChildren();
    if (encoding === "base64") {
      note(root, "二进制文件");
      return;
    }
    if (typeof content !== "string") return;
    const table = addScroll(root);
    const lines = content.replace(/\n$/, "").split("\n");
    if (content === "") return;
    lines.slice(0, MAX_LINES).forEach((text, i) => {
      const removed = kind === "deleted";
      table.append(row(removed ? i + 1 : null, removed ? null : i + 1,
        text, removed ? "removed" : "added"));
    });
    if (lines.length > MAX_LINES) note(root, `仅显示前 ${MAX_LINES} 行`);
  }
  function parseHeader(line) {
    const match = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(line);
    return match && {
      oldStart: Number(match[1]), oldCount: Number(match[2] ?? 1),
      newStart: Number(match[3]), newCount: Number(match[4] ?? 1)
    };
  }
  function renderDiff(root, text) {
    root.replaceChildren();
    const table = addScroll(root);
    let oldNo = 0, newNo = 0, oldLeft = 0, newLeft = 0;
    let oldPath = "", currentPath = "", count = 0, additions = 0, deletions = 0;
    const add = (element) => { if (count++ < MAX_LINES) table.append(element); };
    for (const line of String(text || "").split("\n")) {
      if (oldLeft > 0 || newLeft > 0) {
        if (line.startsWith("-")) {
          add(row(oldNo++, null, line.slice(1), "removed"));
          --oldLeft; ++deletions;
        } else if (line.startsWith("+")) {
          add(row(null, newNo++, line.slice(1), "added"));
          --newLeft; ++additions;
        } else if (line.startsWith(" ")) {
          add(row(oldNo++, newNo++, line.slice(1), ""));
          --oldLeft; --newLeft;
        } else if (line.startsWith("\\ No newline")) {
          // Metadata about an already rendered line.
        }
        continue;
      }
      if (line.startsWith("--- ")) {
        oldPath = line.slice(4).replace(/^a\//, "");
        continue;
      }
      if (line.startsWith("+++ ")) {
        const path = line.slice(4).replace(/^b\//, "");
        const shown = path === "/dev/null" ? oldPath : path;
        if (shown && shown !== "before" && shown !== "after" &&
            shown !== "/dev/null" && shown !== currentPath) {
          currentPath = shown;
          add(el("div", "diff-file", shown));
        }
        continue;
      }
      const header = parseHeader(line);
      if (header) {
        oldNo = header.oldStart; newNo = header.newStart;
        oldLeft = header.oldCount; newLeft = header.newCount;
        add(el("div", "diff-hunk", `@@ -${oldNo} +${newNo} @@`));
      }
    }
    if (!table.childNodes.length) {
      root.replaceChildren();
      note(root, "没有内容变化");
    } else if (count > MAX_LINES) {
      note(root, `仅显示前 ${MAX_LINES} 行`);
    }
    return { additions, deletions };
  }
  return {renderDiff, renderContent};
})();