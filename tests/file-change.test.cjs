const assert = require('node:assert/strict');
const { test } = require('node:test');
const { bootApp } = require('./app-harness.cjs');
const boot = openai => bootApp('file-change', openai);

const added = {
  path: '/tmp/new.txt', resolved_target: { target: 'local' },
  created: true, written: true, content: 'hello <world>\n',
  encoding: 'utf-8', bytes: 14, new_sha256: 'new', diff: '',
};
const deleted = { ...added, created: undefined, deleted: true, old_sha256: 'old' };
const modified = {
  path: added.path, changed: true, written: true,
  diff: '--- before\n+++ after\n@@ -1 +1 @@\n-old\n+new\n',
};

for (const [kind, data] of Object.entries({ added, deleted })) {
  for (const [shape, wrap] of Object.entries({
    structured: data => ({ structuredContent: data }),
    plain: data => data,
    legacyContent: data => ({ content: data }),
    legacyJson: data => ({ content: [{ type: 'json', json: data }] }),
  })) {
    test(`${kind} file via ${shape} preserves full content`, () => {
      const app = boot();
      app.notify(wrap(data));
      assert.equal(app.get('path').textContent, data.path);
      assert.match(app.get('viewer').innerHTML, /hello &lt;world&gt;/);
      assert.match(app.get('viewer').innerHTML, kind === 'added' ? /code-line added/ : /code-line removed/);
    });
  }
  test(`historical ${kind} file without cache id`, () => {
    const app = boot({ toolOutput: data });
    app.notify({ structuredContent: data });
    assert.equal(app.get('card').open, false);
    app.open();
    assert.match(app.get('viewer').innerHTML, /hello &lt;world&gt;/);
  });
  test(`historical ${kind} file restores from cache`, async () => {
    const saved = { ...data, result_id: 'saved' };
    const app = boot({ toolResponseMetadata: { mcp_tool_result: { structuredContent: saved } } });
    app.open();
    const request = app.sent.find(msg => msg.method === 'tools/call');
    assert.equal(request?.params.arguments.result_id, 'saved');
    app.message({ jsonrpc: '2.0', id: request.id, result: {
      structuredContent: { found: true, data: saved },
    } });
    await Promise.resolve();
    assert.match(app.get('viewer').innerHTML, /hello &lt;world&gt;/);
  });
}

test('diff body lines resembling file headers are not discarded', () => {
  const app = boot();
  app.notify({ structuredContent: { ...modified,
    diff: '--- before\n+++ after\n@@ -3,2 +3,2 @@\n--- old heading\n+++ new heading\n context\n--- a/second.txt\n+++ b/second.txt\n@@ -1 +1 @@\n-before\n+after\n',
  } });
  const view = app.get('viewer').innerHTML;
  assert.match(view, /class="code">-- old heading</);
  assert.match(view, /class="code">\+\+ new heading</);
  assert.match(view, /class="gutter">4<\/span><span class="gutter">4/);
  assert.match(view, /class="file-label">second.txt</);
  assert.doesNotMatch(view, /class="file-label">\+ new heading/);
});

for (const completed_jobs of [undefined, [{ job_id: 'unrelated' }]]) {
  test(`tool failure displays error, including completed_jobs=${!!completed_jobs}`, () => {
    const app = boot();
    app.notify({ isError: true, content: [{ type: 'text', text: 'file changed before write: <conflict>' }],
      ...(completed_jobs ? { structuredContent: { completed_jobs } } : {}),
    });
    assert.equal(app.get('status').textContent, 'Failed');
    assert.match(app.get('viewer').innerHTML, /file changed before write: &lt;conflict&gt;/);
    assert.doesNotMatch(app.get('viewer').innerHTML, /No textual differences/);
  });
}

test('live result restores after sleeping', async () => {
  const app = boot();
  const data = { ...modified, result_id: 'live' };
  app.notify({ structuredContent: data });
  app.close();
  app.fireTimers(30);
  assert.equal(app.get('viewer').innerHTML, '');
  app.open();
  const request = app.sent.find(msg => msg.method === 'tools/call');
  assert.equal(request.params.arguments.result_id, 'live');
  app.message({ jsonrpc: '2.0', id: request.id, result: {
    structuredContent: { found: true, data },
  } });
  await Promise.resolve();
  assert.match(app.get('viewer').innerHTML, /class="code">new</);
});

test('delete preview, binary addition, empty addition and rename render', () => {
  const app = boot();
  app.notify({ structuredContent: { ...deleted, deleted: false, written: false } });
  assert.equal(app.get('status').textContent, 'Preview');
  assert.match(app.get('viewer').innerHTML, /hello &lt;world&gt;/);
  app.notify({ structuredContent: { ...added, encoding: 'base64', content: 'AAE=' } });
  assert.match(app.get('viewer').innerHTML, /AAE=/);
  app.notify({ structuredContent: { ...added, content: '', bytes: 0 } });
  assert.equal(app.get('kind').textContent, 'Added');
  app.notify({ structuredContent: { moved: true, source: '/old', destination: '/new' } });
  assert.match(app.get('viewer').innerHTML, /Renamed <strong>\/old<\/strong> to <strong>\/new/);
});

test('no-op edits are not mislabeled as previews', () => {
  const app = boot();
  app.notify({ structuredContent: { ...modified, changed: false, written: false, diff: '' } });
  assert.equal(app.get('status').textContent, 'No change');
});

test('failed results survive sleep and do not restore an earlier success', () => {
  const app = boot({ toolOutput: added });
  app.notify({ isError: true, content: [{ type: 'text', text: 'Permission denied' }] });
  app.close();
  app.fireTimers(30);
  app.open();
  assert.equal(app.get('status').textContent, 'Failed');
  assert.match(app.get('viewer').innerHTML, /Permission denied/);
  assert.doesNotMatch(app.get('viewer').innerHTML, /hello/);
});

test('fresh cached results supersede the historical in-memory fallback', async () => {
  const app = boot({ toolOutput: added });
  const data = { ...modified, result_id: 'new-result' };
  app.notify({ structuredContent: data });
  app.close();
  app.fireTimers(30);
  app.open();
  const request = app.sent.find(msg => msg.method === 'tools/call');
  assert.equal(request?.params.arguments.result_id, 'new-result');
  app.message({ jsonrpc: '2.0', id: request.id, result: {
    structuredContent: { found: true, data },
  } });
  await Promise.resolve();
  assert.match(app.get('viewer').innerHTML, /class="code">new</);
  assert.doesNotMatch(app.get('viewer').innerHTML, /hello/);
});

test('diff handles zero-length sides, omitted counts and missing final newlines', () => {
  const app = boot();
  app.notify({ structuredContent: { ...modified,
    diff: '--- before\n+++ after\n@@ -0,0 +1 @@\n+++ inserted\n\\ No newline at end of file\n@@ -5 +6,0 @@\n--- removed\n\\ No newline at end of file\n@@ -10 +10 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n',
  } });
  const view = app.get('viewer').innerHTML;
  assert.match(view, /class="gutter">1<\/span><span class="code">\+\+ inserted/);
  assert.match(view, /class="gutter">5<\/span><span class="gutter"><\/span><span class="code">-- removed/);
  assert.match(view, /class="gutter">10<\/span><span class="code">new/);
});
