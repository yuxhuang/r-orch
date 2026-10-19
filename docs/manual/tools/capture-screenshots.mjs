#!/usr/bin/env node
// Regenerates the manual's dashboard screenshots. Zero dependencies (Node 22+).
//
//   node docs/manual/tools/capture-screenshots.mjs
//
// Environment:
//   ORCH_BIN     path to the orch binary (default: target/debug/orch in this repo)
//   CHROME       path to Chrome/Chromium (default: macOS Chrome, else google-chrome / chromium on PATH)
//   ONLY         comma-separated shot names to capture (default: all)
//   KEEP_DEMO=1  keep the demo sandbox and print its path
//   DUMP_TEXT=1  print the terminal text before every shot (for debugging key sequences)
import { spawn, execFileSync } from 'node:child_process';
import { existsSync, realpathSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync, statSync } from 'node:fs';
import { tmpdir, homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '../../..');
const imagesDir = resolve(here, '../images');
const orchBin = resolve(process.env.ORCH_BIN || join(repo, 'target/debug/orch'));
const MAC_CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const VIEWPORT = { width: 1280, height: 816 };
const SCALE = 1;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const redact = (text) => text.replace(/token=[0-9a-f]+/g, 'token=…');
const log = (...a) => console.error('[capture]', ...a);

function findChrome() {
  if (process.env.CHROME) return process.env.CHROME;
  if (existsSync(MAC_CHROME)) return MAC_CHROME;
  for (const name of ['google-chrome', 'chromium', 'chromium-browser']) {
    try {
      return execFileSync('which', [name], { encoding: 'utf8' }).trim();
    } catch {}
  }
  throw new Error('Chrome not found: set CHROME=/path/to/chrome');
}

// ---- process bookkeeping: everything started here is killed on exit ----
const children = new Set();
const tempDirs = [];
function start(cmd, args, opts = {}) {
  const child = spawn(cmd, args, { stdio: ['ignore', 'pipe', 'pipe'], detached: true, ...opts });
  children.add(child);
  child.on('exit', () => children.delete(child));
  return child;
}
async function killAll() {
  for (const child of children) {
    try { process.kill(-child.pid, 'SIGTERM'); } catch {}
  }
  for (let i = 0; i < 30 && children.size; i++) await sleep(100);
  for (const child of children) {
    try { process.kill(-child.pid, 'SIGKILL'); } catch {}
  }
}
function cleanTemp() {
  for (const dir of tempDirs) rmSync(dir, { recursive: true, force: true });
}
let shuttingDown = false;
async function shutdown(code) {
  if (shuttingDown) return;
  shuttingDown = true;
  await killAll();
  cleanTemp();
  process.exit(code);
}
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(sig, () => shutdown(130));
for (const ev of ['unhandledRejection', 'uncaughtException']) {
  process.on(ev, (err) => { console.error(ev, err?.message || err); shutdown(1); });
}
const WATCHDOG_MS = 180000;
setTimeout(() => { console.error('watchdog: capture took too long'); shutdown(1); }, WATCHDOG_MS).unref();

// ---- minimal CDP client over the global WebSocket ----
class Cdp {
  constructor(url) {
    this.nextId = 1;
    this.pending = new Map();
    this.ws = new WebSocket(url);
    this.ready = new Promise((res, rej) => {
      this.ws.onopen = res;
      this.ws.onerror = () => rej(new Error('CDP websocket error'));
    });
    this.ws.onmessage = (ev) => {
      const msg = JSON.parse(ev.data);
      const p = msg.id && this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      if (msg.error) p.rej(new Error(`${p.method}: ${msg.error.message}`));
      else p.res(msg.result);
    };
  }
  send(method, params = {}) {
    const id = this.nextId++;
    return new Promise((res, rej) => {
      this.pending.set(id, { res, rej, method });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async eval(expression) {
    const r = await this.send('Runtime.evaluate', { expression, returnByValue: true });
    if (r.exceptionDetails) throw new Error(`eval failed: ${r.exceptionDetails.text} ${expression}`);
    return r.result.value;
  }
}

// ---- keys ----
const SPECIAL = {
  Enter: { key: 'Enter', code: 'Enter', vk: 13, text: '\r' },
  Escape: { key: 'Escape', code: 'Escape', vk: 27 },
  Tab: { key: 'Tab', code: 'Tab', vk: 9 },
  ArrowUp: { key: 'ArrowUp', code: 'ArrowUp', vk: 38 },
  ArrowDown: { key: 'ArrowDown', code: 'ArrowDown', vk: 40 },
  ArrowLeft: { key: 'ArrowLeft', code: 'ArrowLeft', vk: 37 },
  ArrowRight: { key: 'ArrowRight', code: 'ArrowRight', vk: 39 },
  F1: { key: 'F1', code: 'F1', vk: 112 },
};
async function press(cdp, name, { ctrl = false } = {}) {
  const modifiers = ctrl ? 2 : 0;
  if (SPECIAL[name]) {
    const s = SPECIAL[name];
    const base = { key: s.key, code: s.code, windowsVirtualKeyCode: s.vk, nativeVirtualKeyCode: s.vk, modifiers };
    await cdp.send('Input.dispatchKeyEvent', { type: s.text ? 'keyDown' : 'rawKeyDown', ...base, ...(s.text && { text: s.text }) });
    await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', ...base });
  } else if (ctrl) {
    const vk = name.toUpperCase().charCodeAt(0);
    const base = { key: name, code: `Key${name.toUpperCase()}`, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk, modifiers };
    await cdp.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', ...base });
    await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', ...base });
  } else {
    await cdp.send('Input.insertText', { text: name });
  }
  await sleep(150);
}
const type = async (cdp, text) => { for (const ch of text) await press(cdp, ch); };

// ---- terminal text ----
const screenText = (cdp) => cdp.eval(`(() => {
  const b = term.buffer.active, out = [];
  for (let i = 0; i < term.rows; i++) out.push(b.getLine(b.viewportY + i)?.translateToString(true) ?? '');
  return out.join('\\n');
})()`);
async function waitFor(cdp, what, test, timeoutMs = 15000) {
  const end = Date.now() + timeoutMs;
  let text = '';
  while (Date.now() < end) {
    text = await screenText(cdp).catch(() => '');
    if (test(text)) { await sleep(300); return await screenText(cdp); }
    await sleep(150);
  }
  throw new Error(`timed out waiting for ${what}. Last screen:\n${text}`);
}
const has = (...needles) => (t) => needles.every((n) => t.includes(n));

// ---- the shot list ----
// Each shot starts from a known screen and leaves the dashboard on its home screen again.
const shots = [
  {
    name: 'dashboard-overview',
    run: async (c) => {
      await waitFor(c, 'default view', has('Build the signup service'));
    },
  },
  {
    // Letter keys act on the selected entity in the Tasks view (A approves), so views change with the arrows.
    name: 'dashboard-agents',
    run: async (c) => {
      await press(c, 'ArrowRight');
      await waitFor(c, 'Agents view', has('Grace', 'Margaret', 'opencode'));
    },
    after: async (c) => { await press(c, 'ArrowLeft'); },
  },
  {
    name: 'dashboard-task-detail',
    run: async (c) => {
      await waitFor(c, 'Tasks view', has('Prepare the validation change'));
      await press(c, 'Enter');
      await waitFor(c, 'task detail', has('Overview', 'Activity'));
    },
    after: async (c) => { await press(c, 'Escape'); },
  },
  {
    name: 'dashboard-new-agent',
    run: async (c) => {
      await press(c, 'ArrowRight');
      await waitFor(c, 'Agents view', has('Grace'));
      await press(c, 'n');
      await waitFor(c, 'New Agent wizard', has('NEW AGENT'));
    },
  },
  {
    name: 'dashboard-agent-shop',
    run: async (c) => {
      await press(c, 's', { ctrl: true });
      await waitFor(c, 'Agent Shop picker', has('Shop'));
    },
    after: async (c) => { await press(c, 'Escape'); await press(c, 'Escape'); await press(c, 'Escape'); await press(c, 'ArrowLeft'); },
  },
  {
    name: 'dashboard-message-viewer',
    run: async (c) => {
      await press(c, 'ArrowRight');
      await press(c, 'ArrowRight');
      await waitFor(c, 'Actions view', has('ACTIONS', 'Observer mode'));
      await press(c, 'ArrowUp');
      await press(c, 'i');
      await waitFor(c, 'log inspector', has('Metadata'));
      await press(c, 'Tab');
      await sleep(300);
      await press(c, 'Enter');
      await waitFor(c, 'full-text viewer', has('Snapshot'));
    },
    after: async (c) => { await press(c, 'Escape'); await press(c, 'Escape'); await press(c, 'ArrowLeft'); await press(c, 'ArrowLeft'); },
  },
  {
    // The web page has no chrome of its own: it is the dashboard terminal filling the browser viewport.
    // The Goals view keeps this shot different from dashboard-overview.
    name: 'web-dashboard',
    run: async (c) => {
      await press(c, 'ArrowLeft');
      await waitFor(c, 'Goals view', has('GOALS', 'Ship signup validation'));
    },
    after: async (c) => { await press(c, 'ArrowRight'); },
  },
  {
    name: 'dashboard-help',
    run: async (c) => {
      await press(c, 'F1');
      await waitFor(c, 'help', has('Help'));
    },
    after: async (c) => { await press(c, 'Escape'); },
  },
];

async function main() {
  const only = (process.env.ONLY || '').split(',').filter(Boolean);
  const chromePath = findChrome();
  if (!existsSync(orchBin)) throw new Error(`orch binary not found: ${orchBin} (set ORCH_BIN)`);
  mkdirSync(imagesDir, { recursive: true });

  // 1. demo project in its own sandbox
  // Never under the real home: orch searches parent directories for .orchestry/.
  const sandbox = mkdtempSync(join(realpathSync(tmpdir()), 'orch-manual-'));
  if ((sandbox + '/').startsWith(realpathSync(homedir()) + '/')) throw new Error(`sandbox is under the real HOME: ${sandbox}`);
  tempDirs.push(sandbox);
  if (process.env.KEEP_DEMO) tempDirs.pop();
  const demoEnv = {
    PATH: `/usr/bin:/bin:${dirname(process.execPath)}`,
    HOME: homedir(),
    TMPDIR: tmpdir(),
    LANG: 'en_US.UTF-8',
  };
  const project = execFileSync(join(here, 'demo-project.sh'), [orchBin, sandbox], { encoding: 'utf8', env: demoEnv, stdio: ['ignore', 'pipe', 'inherit'] }).trim();
  log('demo project', project);
  if (!existsSync(join(project, '.orchestry'))) throw new Error(`no .orchestry/ in ${project}; refusing to start orch web`);

  // 2. orch web on loopback, ephemeral port, generated token.
  // --observe only stops watch-mode dispatch: the dashboard can still run a task on r/R or a click.
  // What keeps real AI CLIs from running is the minimal PATH and sandbox HOME below, plus a demo
  // project in which no runnable task is assigned to a non-shell agent.
  const env = {
    PATH: '/usr/bin:/bin',
    HOME: join(sandbox, 'home'),
    ORCHESTRY_GLOBAL_CONFIG_PATH: join(sandbox, 'home/global.yml'),
    TERM: 'xterm-256color',
    LANG: 'en_US.UTF-8',
  };
  const web = start(orchBin, ['web', '--bind', '127.0.0.1:0', '--observe'], { cwd: project, env });
  let webOut = '';
  web.stderr.on('data', (d) => { webOut += d; });
  const url = await new Promise((res, rej) => {
    web.stdout.on('data', (d) => {
      webOut += d;
      const m = /listening on (http:\/\/127\.0\.0\.1:\d+\/\?token=[0-9a-f]+)/.exec(webOut);
      if (m) res(m[1]);
    });
    web.on('exit', (code) => rej(new Error(`orch web exited (${code}): ${redact(webOut)}`)));
    setTimeout(() => rej(new Error(`orch web did not report an address: ${redact(webOut)}`)), 15000);
  });
  log('orch web at', url.replace(/token=.*/, 'token=…'));

  // 3. Chrome with a temp profile and a debugging port on loopback
  const profile = mkdtempSync(join(tmpdir(), 'orch-manual-chrome-'));
  tempDirs.push(profile);
  start(chromePath, [
    '--headless=new', '--remote-debugging-port=0', '--remote-debugging-address=127.0.0.1',
    `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check',
    '--disable-extensions', '--disable-sync', '--hide-scrollbars',
    `--window-size=${VIEWPORT.width},${VIEWPORT.height}`, 'about:blank',
  ], { stdio: 'ignore' });
  const portFile = join(profile, 'DevToolsActivePort');
  for (let i = 0; i < 100 && !existsSync(portFile); i++) await sleep(100);
  if (!existsSync(portFile)) throw new Error('Chrome did not start a DevTools port');
  const port = readFileSync(portFile, 'utf8').split('\n')[0];

  let page;
  for (let i = 0; i < 50 && !page; i++) {
    const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    page = targets.find((t) => t.type === 'page');
    if (!page) await sleep(100);
  }
  if (!page) throw new Error('no page target');
  const cdp = new Cdp(page.webSocketDebuggerUrl);
  await cdp.ready;

  await cdp.send('Page.enable');
  await cdp.send('Emulation.setDeviceMetricsOverride', { ...VIEWPORT, deviceScaleFactor: SCALE, mobile: false });
  await cdp.send('Page.navigate', { url });
  await waitFor(cdp, 'the dashboard', has('Build the signup service'), 30000);
  await cdp.eval('term.focus()');

  const failures = [];
  for (const shot of shots) {
    if (only.length && !only.includes(shot.name)) continue;
    try {
      await shot.run(cdp);
      if (process.env.DUMP_TEXT) console.error(`----- ${shot.name} -----\n${await screenText(cdp)}\n-----`);
      const { data } = await cdp.send('Page.captureScreenshot', { format: 'png' });
      const file = join(imagesDir, `${shot.name}.png`);
      writeFileSync(file, Buffer.from(data, 'base64'));
      log(`${shot.name}.png ${(statSync(file).size / 1024).toFixed(0)} KB`);
    } catch (err) {
      failures.push(shot.name);
      log(`FAILED ${shot.name}: ${err.message}`);
    }
    if (shot.after) await shot.after(cdp).catch(() => {});
    await sleep(300);
  }
  if (failures.length) throw new Error(`failed shots: ${failures.join(', ')}`);
}

try {
  await main();
  await shutdown(0);
} catch (err) {
  console.error(err.stack || err.message);
  await shutdown(1);
}
