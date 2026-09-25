const assert = require('node:assert/strict');
const { test } = require('node:test');

const { bootApp } = require('./app-harness.cjs');
const boot = openai => bootApp('file-read', openai);

const file = {
  resolved_target: { target: 'local' }, path: '/tmp/example.txt',
  content: 'hello <world>\nsecond line\n', encoding: 'utf-8', bytes: 26,
  sha256: 'abc', truncated: false, start_line: 7, end_line: 8,
};

function assertFile(app) {
  assert.equal(app.get('path').textContent, file.path);
  assert.match(app.get('viewer').innerHTML, /hello &lt;world&gt;/);
  assert.match(app.get('viewer').innerHTML, /class="gutter">7</);
  assert.match(app.get('viewer').innerHTML, /second line/);
  assert.match(app.get('meta').innerHTML, /abc/);
  assert.doesNotMatch(app.get('viewer').innerHTML, /is empty/);
}

for (const [name, wrap] of Object.entries({
  structured: data => ({ structuredContent: data }),
  plain: data => data,
  legacyContent: data => ({ content: data }),
  legacyJson: data => ({ content: [{ type: 'json', json: data }] }),
})) {
  test(`single file notification: ${name}`, () => {
    const app = boot();
    app.notify(wrap(file));
    assertFile(app);
  });
}

test('historical file without a result id restores on opening', () => {
  const app = boot({ toolOutput: file });
  assert.equal(app.get('card').open, false);
  app.notify({ structuredContent: file });
  assert.equal(app.get('card').open, false);
  app.open();
  assertFile(app);
});

test('historical file with a result id restores from result_read', async () => {
  const saved = { ...file, result_id: 'saved-file' };
  const app = boot({ toolOutput: saved });
  app.open();
  const request = app.sent.find(message => message.method === 'tools/call');
  assert.equal(request?.params.arguments.result_id, 'saved-file');
  app.message({ jsonrpc: '2.0', id: request.id, result: {
    structuredContent: { found: true, data: saved },
  } });
  await Promise.resolve();
  assertFile(app);
});

test('empty and binary files retain their intended displays', () => {
  const app = boot();
  app.notify({ structuredContent: { ...file, content: '', bytes: 0 } });
  assert.equal(app.get('path').textContent, file.path);
  assert.match(app.get('viewer').innerHTML, /is empty/);
  app.notify({ structuredContent: { ...file, content: 'AAE=', encoding: 'base64' } });
  assert.match(app.get('viewer').innerHTML, /Binary content shown as base64/);
  assert.match(app.get('viewer').innerHTML, /AAE=/);
});

test('batch reads still render contents and isolated errors', () => {
  const app = boot();
  app.notify({ structuredContent: {
    requested_count: 2, succeeded: 1, failed: 1, truncated: false,
    files: [{ ...file, success: true }, { path: '/missing', success: false, error: 'Missing file' }],
  } });
  assert.equal(app.get('title').textContent, '2 file reads');
  const blocks = app.get('viewer').children;
  assert.match(blocks[0].children[1].innerHTML, /hello &lt;world&gt;/);
  assert.match(blocks[1].children[1].innerHTML, /Missing file/);
});
