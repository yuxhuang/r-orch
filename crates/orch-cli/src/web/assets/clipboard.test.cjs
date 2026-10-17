// First-party OSC52 glue; all clipboard writes terminate in a fake sink.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function page({ secure = true, write = async () => {} } = {}) {
  const nodes = new Map();
  const element = () => ({ hidden: false, disabled: false, textContent: '', value: '',
    addEventListener(type, fn) { this['on' + type] = fn; }, focus() {}, select() {} });
  let osc, socket;
  const context = {
    TextEncoder, TextDecoder, Uint8Array, ArrayBuffer, URLSearchParams,
    atob, btoa, isSecureContext: secure,
    location: { protocol: 'http:', host: 'fixture.invalid', search: '' },
    navigator: { clipboard: { writeText: write } },
    document: { getElementById(id) {
      if (!nodes.has(id)) nodes.set(id, element());
      return nodes.get(id);
    } },
    window: { addEventListener() {} },
    FitAddon: { FitAddon: class { fit() {} } },
    Terminal: class {
      cols = 80; rows = 24;
      parser = { registerOscHandler(code, fn) { assert.equal(code, 52); osc = fn; } };
      loadAddon() {} open() {} focus() {} write() {} onData() {} onBinary() {} onResize() {}
    },
    WebSocket: class {
      static OPEN = 1;
      readyState = 1;
      constructor() { socket = this; }
      send() {}
    },
  };
  const html = fs.readFileSync(__dirname + '/index.html', 'utf8');
  vm.runInNewContext(html.match(/<script>([\s\S]*?)<\/script>/)[1], context);
  assert.equal(typeof osc, 'function', 'OSC52 must have a first-party consumer');
  return { request: osc, nodes, socket };
}
const wire = (text) => 'c;' + Buffer.from(text).toString('base64');

test('a valid UTF-8 request waits for a click, writes exact bytes and clears content', async () => {
  const writes = [];
  const p = page({ write: async text => writes.push(text) });
  const text = '\ufeffquotes " commands $()\n界 e\u0301 👩‍💻\x1b\x07';
  assert.equal(p.request(wire(text)), true);
  assert.deepEqual(writes, []);
  assert.equal(p.nodes.get('copy-button').disabled, false);
  await p.nodes.get('copy-button').onclick();
  assert.deepEqual(writes, [text]);
  assert.match(p.nodes.get('copy-status').textContent, /Copied in browser/);
  assert.equal(p.nodes.get('copy-fallback').value, '');
  assert.equal(p.nodes.get('copy-button').disabled, true);
});

test('queries, wrong selections, noncanonical base64, invalid UTF-8 and oversize fail closed', () => {
  const p = page();
  for (const bad of ['c;?', 'p;YQ==', 'c;YQ', 'c;YR==', 'c;Y Q==', 'c;YQ==;x', 'c;/w==', wire('x'.repeat(65537))]) {
    p.request(wire('old'));
    assert.equal(p.request(bad), true, bad);
    assert.equal(p.nodes.get('copy-button').disabled, true, bad);
    assert.equal(p.nodes.get('copy-fallback').value, '', bad);
    assert.match(p.nodes.get('copy-status').textContent, /rejected/i);
  }
  p.request(wire('x'.repeat(65536)));
  assert.equal(p.nodes.get('copy-button').disabled, false);
});

test('insecure context and permission denial expose selectable exact text without success', async () => {
  const writes = [];
  const insecure = page({ secure: false, write: async text => writes.push(text) });
  insecure.request(wire('<script>not markup</script>'));
  await insecure.nodes.get('copy-button').onclick();
  assert.deepEqual(writes, []);
  assert.equal(insecure.nodes.get('copy-fallback').value, '<script>not markup</script>');
  assert.equal(insecure.nodes.get('copy-fallback').hidden, false);
  assert.match(insecure.nodes.get('copy-status').textContent, /insecure/i);
  const denied = page({ write: async () => { throw new Error('untrusted <error>'); } });
  denied.request(wire('retained after denial'));
  await denied.nodes.get('copy-button').onclick();
  assert.equal(denied.nodes.get('copy-fallback').value, 'retained after denial');
  assert.match(denied.nodes.get('copy-status').textContent, /denied|unavailable/i);
  assert.doesNotMatch(denied.nodes.get('copy-status').textContent, /untrusted/);
});

test('a replacement and disconnect retire old requests even across asynchronous completion', async () => {
  let finish;
  const p = page({ write: () => new Promise(resolve => { finish = resolve; }) });
  p.request(wire('first'));
  const writing = p.nodes.get('copy-button').onclick();
  p.request(wire('second'));
  assert.equal(p.nodes.get('copy-button').disabled, true);
  finish(); await writing;
  assert.equal(p.nodes.get('copy-button').disabled, false);
  assert.doesNotMatch(p.nodes.get('copy-status').textContent, /Copied/);
  p.socket.onclose();
  assert.equal(p.nodes.get('copy-button').disabled, true);
  assert.equal(p.nodes.get('copy-fallback').value, '');
  assert.equal(p.nodes.get('copy-panel').hidden, true);
});
