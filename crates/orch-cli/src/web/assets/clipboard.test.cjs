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
  // An injected clock: tests fire timers explicitly instead of sleeping.
  const timers = new Map();
  let nextTimer = 1;
  const context = {
    TextEncoder, TextDecoder, Uint8Array, ArrayBuffer, URLSearchParams,
    atob, btoa, isSecureContext: secure,
    setTimeout(fn, ms) { timers.set(nextTimer, { fn, ms }); return nextTimer++; },
    clearTimeout(id) { timers.delete(id); },
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
  const fireTimers = () => {
    const due = [...timers.values()];
    timers.clear();
    for (const { fn } of due) fn();
    return due.length;
  };
  return { request: osc, nodes, socket, timers, fireTimers };
}
const wire = (text) => 'c;' + Buffer.from(text).toString('base64');
// A browser write the test settles by hand; `never` leaves it pending forever.
function deferredWrites() {
  const calls = [];
  const write = text => new Promise((resolve, reject) => calls.push({ text, resolve, reject }));
  return { calls, write };
}
const never = () => new Promise(() => {});
const view = p => ({
  disabled: p.nodes.get('copy-button').disabled,
  status: p.nodes.get('copy-status').textContent,
  fallback: p.nodes.get('copy-fallback').hidden ? null : p.nodes.get('copy-fallback').value,
  panelHidden: p.nodes.get('copy-panel').hidden,
});

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

test('a browser write that never settles times out within 3 s with the exact selectable text', () => {
  const p = page({ write: never });
  const text = 'stalled 界 👩‍💻 "quoted"\n';
  p.request(wire(text));
  p.nodes.get('copy-button').onclick();
  const waits = [...p.timers.values()].map(t => t.ms);
  assert.equal(waits.length, 1, 'one bounded UI wait per write');
  assert.ok(waits[0] > 0 && waits[0] <= 3000, `wait ${waits[0]} ms`);
  p.fireTimers();
  const shown = view(p);
  assert.equal(shown.fallback, text);
  assert.match(shown.status, /may or may not/i);
  assert.doesNotMatch(shown.status, /click|copied/i);
  assert.equal(shown.disabled, true, 'the unresolved write still owns the clipboard');
});

test('only one browser write is in flight, even after the wait times out and the request is replaced', async () => {
  const sink = deferredWrites();
  const p = page({ write: sink.write });
  p.request(wire('first'));
  const first = p.nodes.get('copy-button').onclick();
  p.fireTimers();
  // Clicks are not awaited: a wrongly started write would never settle.
  p.nodes.get('copy-button').onclick();
  p.request(wire('second'));
  assert.equal(view(p).fallback, 'second');
  p.nodes.get('copy-button').onclick();
  p.nodes.get('copy-button').disabled = false; // even a forced click must not overlap
  p.nodes.get('copy-button').onclick();
  assert.deepEqual(sink.calls.map(c => c.text), ['first']);
  sink.calls[0].resolve(); await first;
  p.nodes.get('copy-button').onclick(); // left pending: only its start matters
  assert.deepEqual(sink.calls.map(c => c.text), ['first', 'second'], 'writes stay sequential and in order');
});

test('dismissing or replacing an unresolved write recovers manual copy for the replacement', () => {
  const p = page({ write: never });
  p.request(wire('first'));
  p.nodes.get('copy-button').onclick();
  p.nodes.get('copy-dismiss').onclick();
  assert.equal(view(p).panelHidden, true);
  p.request(wire('replacement'));
  const shown = view(p);
  assert.equal(shown.panelHidden, false);
  assert.equal(shown.fallback, 'replacement');
  assert.equal(shown.disabled, true);
  assert.match(shown.status, /not finished/i);
  assert.doesNotMatch(shown.status, /click Copy/i, 'never instruct a click on a disabled button');
});

test('a late success or failure of an older write never relabels or clears the newer request', async () => {
  for (const settle of ['resolve', 'reject']) {
    const sink = deferredWrites();
    const p = page({ write: sink.write });
    p.request(wire('older'));
    const older = p.nodes.get('copy-button').onclick();
    p.request(wire('newer'));
    p.fireTimers(); // the older request's wait must not speak for the newer one
    assert.match(view(p).status, /not finished/i, settle);
    sink.calls[0][settle](new Error('late')); await older;
    const shown = view(p);
    assert.equal(shown.fallback, 'newer', settle);
    assert.doesNotMatch(shown.status, /copied|denied|may or may not/i, settle);
    assert.equal(shown.disabled, false, `${settle}: the newer request can now be written`);
  }
});

test('disconnect clears a timed-out request and a late completion cannot bring it back', async () => {
  const sink = deferredWrites();
  const p = page({ write: sink.write });
  p.request(wire('secret'));
  const writing = p.nodes.get('copy-button').onclick();
  p.fireTimers();
  assert.equal(view(p).fallback, 'secret');
  p.socket.onclose();
  sink.calls[0].resolve(); await writing;
  assert.deepEqual(view(p), { disabled: true, status: '', fallback: null, panelHidden: true });
  assert.equal(p.nodes.get('copy-fallback').value, '');
  assert.equal(p.fireTimers(), 0, 'no timer survives disconnect');
});
