const { readFileSync } = require('node:fs');
const { runInNewContext } = require('node:vm');

function bootApp(asset, openai) {
  const html = readFileSync(`${__dirname}/../assets/${asset}.html`, 'utf8');
  const script = html.match(/<script>([\s\S]*?)<\/script>/)[1]
    .replace('__TARGET_OPS_RUNTIME_CONFIG__', JSON.stringify({ successCollapseMs: 10, failureCollapseMs: 20, sleepAfterMs: 30 }));

  function element() {
    return {
      innerHTML: '', textContent: '', children: [], listeners: {},
      addEventListener(name, fn) { this.listeners[name] = fn; },
      append(...nodes) { this.children.push(...nodes); },
      appendChild(node) { this.children.push(node); },
      replaceChildren() { this.children = []; this.innerHTML = ''; },
    };
  }
  const elements = new Map();
  const listeners = {};
  const sent = [];
  const timers = new Map();
  let timerId = 0;
  runInNewContext(script, {
    document: {
      getElementById(id) {
        if (!elements.has(id)) elements.set(id, element());
        return elements.get(id);
      },
      createElement: element,
    },
    window: {
      openai,
      parent: { postMessage(message) { sent.push(message); } },
      addEventListener(name, fn) { listeners[name] = fn; },
    },
    setTimeout(fn, ms) { timers.set(++timerId, { fn, ms }); return timerId; },
    clearTimeout(id) { timers.delete(id); }, console,
  });
  return {
    get: id => elements.get(id), sent,
    close() { const card = elements.get('card'); card.open = false; card.listeners.toggle(); },
    fireTimers(ms) {
      for (const [id, timer] of [...timers]) {
        if (timer.ms === ms) { timers.delete(id); timer.fn(); }
      }
    },
    message: data => listeners.message({ data }),
    notify(params) { this.message({ method: 'ui/notifications/tool-result', params }); },
    open() { const card = elements.get('card'); card.open = true; card.listeners.toggle(); },
  };
}

module.exports = { bootApp };
