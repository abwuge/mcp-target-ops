const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { runInNewContext } = require('node:vm');
const flush = () => new Promise(resolve => setImmediate(resolve));

function boot(saved) {
  const script = readFileSync(`${__dirname}/../assets/panel.html`, 'utf8').match(/<script>([\s\S]*?)<\/script>/)[1]
    .replace('__PANEL_RENDERERS__', '')
    .replace('__TARGET_OPS_RUNTIME_CONFIG__', '{jobPollIntervalMs:1000}');
  const timers = new Map(), elements = new Map(), listeners = {}, sent = [], rendered = [];
  let nextTimer = 0;
  function element() {
    return { children: [], checked: true, hidden: false, listeners: {}, attrs: {},
      addEventListener(name, fn) { this.listeners[name] = fn; },
      setAttribute(name, value) { this.attrs[name] = value; },
      append(...nodes) { this.children.push(...nodes); }, appendChild(node) { this.children.push(node); },
      replaceChildren(...nodes) { this.children = nodes; },
      attachShadow() { return element(); }, content: { cloneNode() { return element(); } },
    };
  }
  const document = {
    hidden: false, body: element(), documentElement: { style: {}, getBoundingClientRect: () => ({height:400}) },
    getElementById(id) { if (!elements.has(id)) elements.set(id, element()); return elements.get(id); },
    createElement: element, addEventListener(name, fn) { listeners[name] = fn; },
  };
  const window = {
    innerWidth: 760, parent: { postMessage(message) { sent.push(message); } },
    openai: { widgetState: saved, setWidgetState(value) { this.widgetState = value; } },
    requestAnimationFrame(fn) { fn(); }, addEventListener(name, fn) { listeners[name] = fn; },
  };
  runInNewContext(script, { document, window, console,
    targetOpsRenderers: Object.fromEntries(['exec-terminal','file-read','file-change','inventory-card'].map(kind => [kind, () => ({render: data => rendered.push({kind,data})})])),
    setTimeout(fn, ms) { const id = ++nextTimer; timers.set(id,{fn,ms}); return id; },
    clearTimeout(id) { timers.delete(id); },
  });
  const app = {
    get: id => document.getElementById(id), sent, rendered, document, window,
    message(data, source = window.parent) { listeners.message({data,source}); },
    async reply(request, result) { this.message({jsonrpc:'2.0',id:request.id,result}); await flush(); },
    async fire(ms) { for (const [id,timer] of [...timers]) if (timer.ms === ms) {timers.delete(id);timer.fn();} await flush(); },
    request() { return sent.filter(m => m.method === 'tools/call').at(-1); },
    notify(id='panel-a') { this.message({method:'ui/notifications/tool-result',params:{structuredContent:{panel_id:id,version:0}}}); },
    async start() { await this.reply(sent[0],{}); this.notify(); await this.fire(0); },
    click(id) { this.get(id).listeners.click(); },
    async select(index) { this.get('operations').children[index].listeners.click(); await this.fire(0); },
    async visibility(hidden) { document.hidden=hidden;listeners.visibilitychange();await this.fire(0); },
  };
  return app;
}
const entry = (seq, tool='exec', status='completed') => ({seq,result_id:`r${seq}`,tool,label:`operation ${seq}`,status});
const response = (entries, version=entries.length, detail=null) => ({structuredContent:{panel_id:'panel-a',version,oldest_seq:1,entries,detail}});
const detail = (id, tool='exec', data={stdout:'ok'}) => ({result_id:id,tool,found:true,data});

 test('one panel discovers later operations and follows latest without another render call', async () => {
  const app=boot(); await app.start();
  await app.reply(app.request(),response([entry(1)])); await app.fire(0);
  assert.equal(app.request().params.arguments.result_id,'r1');
  await app.reply(app.request(),response([],1,detail('r1')));
  await app.fire(5000);
  assert.equal(app.request().params.arguments.after,1);
  await app.reply(app.request(),response([entry(2,'file_read')],2)); await app.fire(0);
  await app.reply(app.request(),response([],2,detail('r2','file_read',{content:'second'})));
  assert.equal(app.rendered.at(-1).kind,'file-read');
  assert.equal(app.get('operations').children.length,2);
  assert.ok(app.sent.filter(m=>m.method==='tools/call').every(m=>m.params.name==='panel_read'));
 });

 test('selection during an in-flight response ignores stale details and fetches the chosen result', async () => {
  const app=boot(); await app.start();
  await app.reply(app.request(),response([entry(1),entry(2)])); await app.fire(0);
  const pending=app.request();
  await app.select(1); // r1 while r2 is in flight
  assert.equal(app.request(),pending);
  await app.reply(pending,response([],2,detail('r2'))); await app.fire(0);
  assert.equal(app.rendered.length,0);
  assert.equal(app.request().params.arguments.result_id,'r1');
  await app.reply(app.request(),response([],2,detail('r1')));
  assert.equal(app.rendered.length,1);
 });

 test('parallel jobs keep independent states and poll only selected output', async () => {
  const app=boot(); await app.start();
  await app.reply(app.request(),response([entry(1,'exec_start','running'),entry(2,'exec_start','running')])); await app.fire(0);
  await app.reply(app.request(),response([],2,detail('r2','exec_start',{status:'running',stdout:'two'})));
  await app.fire(1000);
  assert.equal(app.request().params.arguments.result_id,'r2');
  await app.reply(app.request(),response([entry(1,'exec_start','running'),entry(2,'exec_start','completed')],2,detail('r2','exec_start',{stdout:'done'})));
  assert.equal(app.get('status').textContent,'1 running · 2 operations');
  await app.fire(1000);
  assert.equal(app.request().params.arguments.result_id,undefined);
 });

 test('remount restores selection; duplicate host replay does not reset the cursor', async () => {
  const app=boot({panel_id:'panel-a',selected:'r1',follow:false}); await app.start();
  assert.equal(app.request().params.arguments.after,0);
  await app.reply(app.request(),response([entry(1),entry(2)],2,detail('r1')));
  app.notify(); await app.fire(5000);
  assert.equal(app.request().params.arguments.after,2);
  assert.equal(app.window.openai.widgetState.selected,'r1');
 });

 test('tool errors pause polling and manual refresh retries; hidden panels pause', async () => {
  const app=boot(); await app.start();
  await app.reply(app.request(),{isError:true,content:[{type:'text',text:'Unavailable'}]});
  const count=app.sent.length; await app.fire(5000); assert.equal(app.sent.length,count);
  app.click('refresh'); await app.fire(0); assert.notEqual(app.sent.length,count);
  await app.reply(app.request(),response([]));
  await app.visibility(true); const hiddenCount=app.sent.length;
  await app.fire(5000); assert.equal(app.sent.length,hiddenCount);
  await app.visibility(false); assert.ok(app.sent.length>hiddenCount);
 });

 test('expired results render a notice and unrelated windows cannot deliver tool results', async () => {
  const app=boot(); await app.start();
  app.message({method:'ui/notifications/tool-result',params:{panel_id:'attacker'}},{});
  await app.reply(app.request(),response([entry(1)])); await app.fire(0);
  assert.equal(app.request().params.arguments.panel_id,'panel-a');
  await app.reply(app.request(),response([],1,{result_id:'r1',found:false}));
  assert.equal(app.get('detail').children[0].textContent,'Result expired from the cache.');
 });

 test('a recreated panel resets retained client history even when its version repeats', async () => {
  const app=boot(); await app.start();
  await app.reply(app.request(),response([entry(1)]));await app.fire(0);
  await app.reply(app.request(),response([],1,detail('r1')));await app.fire(5000);
  const recreated=response([{...entry(1),result_id:'replacement'}],1);
  recreated.structuredContent.reset=true;recreated.structuredContent.generation='new';
  await app.reply(app.request(),recreated);await app.fire(0);
  assert.equal(app.get('operations').children.length,1);
  assert.equal(app.request().params.arguments.result_id,'replacement');
  assert.equal(app.request().params.arguments.generation,'new');
 });
