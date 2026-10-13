// Golden prompt generator. Drives the real TS engine over the branch matrix.
//   cd parity/orch-ts && ./node_modules/.bin/tsx ../../fixtures/prompts/gen/generate.mts "$PWD" ../../fixtures/prompts
// Contexts are authored in the Rust `PromptContext` shape (explicit nulls,
// shared_context as [k, v] pairs) and converted to the TS shape before rendering.
import { mkdirSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [, , tsRoot, outDir] = process.argv;
if (!tsRoot || !outDir) throw new Error('usage: generate.mts <orch-ts root> <out dir>');

const mod = await import(pathToFileURL(join(tsRoot, 'src/infrastructure/template/template-engine.ts')).href);
const { LiquidTemplateEngine, DEFAULT_SYSTEM_TEMPLATE, DEFAULT_USER_TEMPLATE, DEFAULT_PROMPT_TEMPLATE, filterRelevantContext } = mod;

type Json = any;

function toTs(ctx: Json): Json {
  const out: Json = {};
  for (const [k, v] of Object.entries(ctx)) strip(out, k, v);
  if (ctx.shared_context) out.shared_context = Object.fromEntries(ctx.shared_context);
  return out;
}
function strip(target: Json, key: string, v: Json) {
  if (v === null) {
    if (key === 'attempt') target[key] = null;
    return;
  }
  if (Array.isArray(v)) target[key] = v.map((x) => (x && typeof x === 'object' && !Array.isArray(x) ? clean(x) : x));
  else if (typeof v === 'object') target[key] = clean(v);
  else target[key] = v;
}
function clean(o: Json): Json {
  const r: Json = {};
  for (const [k, v] of Object.entries(o)) if (v !== null) r[k] = Array.isArray(v) ? v.map((x) => (x && typeof x === 'object' ? clean(x) : x)) : v && typeof v === 'object' ? clean(v) : v;
  return r;
}

const base = (): Json => ({
  project: { name: 'demo-project', description: null },
  task: { id: 'tsk_abc1234', title: 'Fix login bug', description: 'Fix the login bug\nin auth.', priority: 3, labels: [], scope: null, is_autonomous: false, goal_id: null },
  agent: { id: 'agt_aaa1111', name: 'Backend A', role: 'Senior Dev' },
  agents: [
    { id: 'agt_aaa1111', name: 'Backend A', role: null, adapter: 'claude' },
    { id: 'agt_bbb2222', name: 'QA B', role: 'QA Engineer\n## WORKFLOW\nrun tests', adapter: 'codex' },
    { id: 'agt_ccc3333', name: 'Shelly', role: null, adapter: 'shell' },
  ],
  attempt: null,
  workspace_path: '/tmp/work/tsk_abc1234',
  retry: null,
  feedback: null,
  shared_context: null,
  messages: null,
  goal: null,
});
const with_ = (patch: (c: Json) => void): Json => { const c = base(); patch(c); return c; };

const msg = (i: number, over: Json = {}): Json => ({ id: `msg_${i}`, from: `Agent ${i}`, subject: `Subject ${i}`, body: `Body line ${i}\nsecond line`, sent_at: '2026-01-02T03:04:05.678Z', reply_to: null, ...over });
const goal = (over: Json = {}): Json => ({ id: 'goal_123', title: 'Ship v2', description: 'Make it fast.\nAnd safe.', status: 'active', task_names: ['[todo] A', '[done] B'], progress: null, ...over });

type Case = { name: string; template: string; context: Json; description: string };
const cases: Case[] = [];
const T = (kind: 'system' | 'user' | 'legacy') => kind;
function both(name: string, context: Json) {
  for (const k of ['system', 'user', 'legacy'] as const) cases.push({ name: `${name}.${k}`, template: T(k), context, description: name });
}

both('plain', base());
both('no-role', with_((c) => { c.agent.role = null; }));
both('empty-agents', with_((c) => { c.agents = []; }));
both('attempt-2-retry', with_((c) => { c.attempt = 2; c.retry = { previous_error: 'exit code 1', previous_output: 'Some output\nline 2' }; }));
both('attempt-3-retry-empty-output', with_((c) => { c.attempt = 3; c.retry = { previous_error: 'timeout', previous_output: '' }; }));
both('attempt-2-no-retry', with_((c) => { c.attempt = 2; }));
both('feedback', with_((c) => { c.feedback = 'Please add tests.\nAlso docs.'; }));
both('autonomous-goal', with_((c) => { c.task.labels = ['autonomous']; c.task.is_autonomous = true; c.task.goal_id = 'goal_123'; }));
both('autonomous-no-goal', with_((c) => { c.task.labels = ['autonomous']; c.task.is_autonomous = true; }));
both('autonomous-empty-goal-id', with_((c) => { c.task.labels = ['autonomous']; c.task.is_autonomous = true; c.task.goal_id = ''; }));
both('goal-full', with_((c) => { c.goal = goal({ progress: 'Halfway there.\nNext: tests.' }); }));
both('goal-no-progress', with_((c) => { c.goal = goal(); }));
both('goal-empty-desc-no-tasks', with_((c) => { c.goal = goal({ description: '', task_names: [] }); }));
both('goal-with-autonomous', with_((c) => { c.task.is_autonomous = true; c.task.goal_id = 'goal_123'; c.task.labels = ['autonomous']; c.goal = goal({ progress: 'p' }); }));
both('messages-1', with_((c) => { c.messages = [msg(1)]; }));
both('messages-1-reply', with_((c) => { c.messages = [msg(1, { reply_to: 'msg_0' })]; }));
both('messages-3', with_((c) => { c.messages = [msg(1), msg(2, { subject: '', reply_to: 'msg_1' }), msg(3)]; }));
both('shared-context-1', with_((c) => { c.shared_context = [['api-design', 'REST over gRPC']]; }));
both('shared-context-ordered', with_((c) => { c.shared_context = [['zeta', 'last?'], ['goal_123-progress', 'half'], ['alpha', 'a\nmulti-line'], ['bug-17', 'crash']]; }));
both('team-multiline-role', with_((c) => { c.agents.push({ id: 'agt_ddd4444', name: 'Long', role: 'A'.repeat(100), adapter: 'claude' }); }));
both('everything', with_((c) => {
  c.project.description = 'A demo.';
  c.task.scope = ['src/api/**']; c.task.labels = ['autonomous', 'x']; c.task.is_autonomous = true; c.task.goal_id = 'goal_123';
  c.attempt = 2; c.retry = { previous_error: 'e', previous_output: 'o' }; c.feedback = 'fb';
  c.shared_context = [['goal_123-progress', 'half'], ['k', 'v']]; c.messages = [msg(1), msg(2)]; c.goal = goal({ progress: 'prog' });
}));
both('special-chars', with_((c) => { c.task.title = '<b>"quoted" & {{ not }} {% raw %}'; c.task.description = '{{ agent.name }} {% if x %}'; c.feedback = '{{ project.name }}'; }));
both('unicode', with_((c) => { c.task.title = 'Исправить 🚀 баг'; c.agent.name = 'Агент'; c.agents[0].name = 'Агент'; }));
both('priority-zero-labels', with_((c) => { c.task.priority = 0; c.task.labels = ['a', 'b']; }));

const custom = (name: string, template: string, ctx: Json = base()) => cases.push({ name: `custom.${name}`, template: `inline:${template}`, context: ctx, description: name });
custom('hello', 'Hello {{ agent.name }}');
custom('missing-var', 'a{{ nope.deep }}b{{ task.nothing }}c{{ feedback }}d');
custom('truthy-zero-empty', '{% if task.priority %}p{% endif %}|{% if task.description != "" %}d{% endif %}|{% if agents %}arr{% endif %}', with_((c) => { c.task.priority = 0; }));
custom('default-filter', '{{ task.goal_id | default: "<g>" }}|{{ feedback | default: "none" }}', with_((c) => { c.task.goal_id = ''; }));
custom('default-filter-set', '{{ task.goal_id | default: "<g>" }}', with_((c) => { c.task.goal_id = 'goal_9'; }));
custom('size', '{{ agents.size }}|{{ task.labels.size }}|{{ agent.name.size }}', with_((c) => { c.task.labels = ['x']; }));
custom('for-pairs', '{% for e in shared_context %}[{{ e[0] }}={{ e[1] }}]{% endfor %}', with_((c) => { c.shared_context = [['b', '2'], ['a', '1'], ['c', '3']]; }));
custom('block-newlines', 'x\n{% if agent.role %}\nyes\n{% endif %}\n{% for a in agents %}\n{{ a.name }}\n{% endfor %}\nend\n');
custom('attempt-null-if', '{% if attempt %}A{{ attempt }}{% endif %}|{% if retry %}R{% endif %}');
custom('compare-gt', '{% if agents.size > 2 %}many{% endif %}{% if agents.size > 5 %}lots{% endif %}');
custom('empty-template', '');
custom('crlf', 'a\r\n{{ agent.name }}\r\n');
cases.push({
  name: 'legacy.default-prompt-template', template: 'inline:' + DEFAULT_PROMPT_TEMPLATE,
  context: with_((c) => { c.attempt = 2; c.retry = { previous_error: 'boom', previous_output: 'out' }; c.feedback = 'f'; }), description: 'DEFAULT_PROMPT_TEMPLATE',
});

const engine = new LiquidTemplateEngine();
const resolve = (t: string) => t === 'system' ? DEFAULT_SYSTEM_TEMPLATE : t === 'user' ? DEFAULT_USER_TEMPLATE : t === 'legacy' ? DEFAULT_PROMPT_TEMPLATE : t.slice('inline:'.length);

mkdirSync(outDir, { recursive: true });
for (const f of readdirSync(outDir)) if (f.endsWith('.json')) rmSync(join(outDir, f));

for (const c of cases) {
  const template = resolve(c.template);
  const expected = await engine.render(template, toTs(c.context));
  const record = { name: c.name, template: c.template.startsWith('inline:') ? { inline: template } : c.template, context: c.context, expected };
  writeFileSync(join(outDir, `${c.name}.json`), JSON.stringify(record, null, 2) + '\n');
}

// Default templates, for the byte-exact include_str! check.
writeFileSync(join(outDir, '_templates.json'), JSON.stringify({ system: DEFAULT_SYSTEM_TEMPLATE, user: DEFAULT_USER_TEMPLATE, legacy: DEFAULT_PROMPT_TEMPLATE }, null, 2) + '\n');

// filterRelevantContext vectors (insertion order of the output is the assertion).
const rec = (pairs: [string, string][]) => pairs;
const filterCases: { name: string; entries: [string, string][]; filter: Json }[] = [];
const F = (name: string, entries: [string, string][], filter: Json) => filterCases.push({ name, entries, filter });
F('empty', [], { agentName: 'Backend A' });
F('goal-prefix', [['other', 'x'], ['Goal_1-progress', 'y'], ['zzz', 'z']], { agentName: 'Backend A', goalId: 'goal_1' });
F('agent-name-in-key-or-value', [['a', 'nothing'], ['b', 'mentions BACKEND A here'], ['backend a-notes', 'v']], { agentName: 'Backend A' });
F('scope', [['x', 'y'], ['api-refactor', 'done'], ['docs', 'src/api/ is hot']], { agentName: 'Zed', taskScope: ['src/api/**'] });
F('scope-wildcard-only', [['x', 'y'], ['z', 'w']], { agentName: 'Zed', taskScope: ['**', '*', '/'] });
F('scope-first-match-breaks', [['k', 'src/a and lib/b']], { agentName: 'Zed', taskScope: ['src/a', 'lib/b'] });
F('role-prefix', [['frontend-x', 'v'], ['backend-x', 'v'], ['backend_y', 'v'], ['backend', 'v']], { agentName: 'Backend A' });
F('front-end-keywords', [['backend-1', 'v'], ['tui-1', 'v'], ['frontend-1', 'v'], ['front-end-1', 'v']], { agentName: 'Front-End' });
F('marketing-keywords', [['marketer-1', 'v'], ['cmo_1', 'v'], ['marketing-1', 'v'], ['x', 'v']], { agentName: 'Marketing Lead' });
F('role-first-word', [['senior-x', 'v'], ['x', 'v']], { agentName: 'Zed', agentRole: 'Senior Dev' });
F('role-first-word-short', [['qa-x', 'v'], ['x', 'v']], { agentName: 'Zed', agentRole: 'qa engineer' });
F('role-dup-keyword', [['backend-x', 'v'], ['y', 'v']], { agentName: 'Backend A', agentRole: 'Backend dev' });
F('generic-prefixes', [['unrelated', 'v'], ['bug-1', 'v'], ['PERF-2', 'v'], ['stability-3', 'v'], ['docs-4', 'v'], ['arch-5', 'v'], ['spec-6', 'v'], ['bugs', 'v']], { agentName: 'Zed' });
F('stable-ties', [['c', '1'], ['a', '2'], ['b', '3'], ['bug-z', '4'], ['bug-a', '5']], { agentName: 'Zed' });
F('limit-15', Array.from({ length: 30 }, (_, i) => [`k${String(i).padStart(2, '0')}`, `v${i}`] as [string, string]), { agentName: 'Zed' });
F('limit-15-mixed', Array.from({ length: 30 }, (_, i) => [i % 3 === 0 ? `bug-${i}` : `k${i}`, `v${i}`] as [string, string]), { agentName: 'Zed' });
F('long-value', [['k', 'x'.repeat(600)]], { agentName: 'Zed' });
F('unicode-case', [['ÄGENT-notes', 'v'], ['x', 'ÄGENT here']], { agentName: 'Ägent' });
F('empty-agent-name', [['a', 'b']], { agentName: '' });
F('short-first-word', [['a-x', 'v']], { agentName: 'A B' });
const filterOut = filterCases.map((c) => ({ ...c, expected: Object.entries(filterRelevantContext(Object.fromEntries(c.entries), c.filter)) }));
writeFileSync(join(outDir, '_filter.json'), JSON.stringify(filterOut, null, 2) + '\n');
// Skill block (V17): the real TS SkillLoader against the shipped library.
const { SkillLoader } = await import(pathToFileURL(join(tsRoot, 'src/infrastructure/skills/skill-loader.ts')).href);
const skillCases = [
  ['careful'],
  ['review', 'feature-dev:code-explorer', 'qa-only', 'nonexistent', '../etc/passwd', 'Review'],
  ['unfreeze', 'freeze', 'unfreeze'],
  ['only:mcp'],
];
const skillOut = [];
for (const names of skillCases) skillOut.push({ names, expected: await new SkillLoader(join(tsRoot, 'skills', 'library')).loadSkills(names) });
writeFileSync(join(outDir, '_skills.json'), JSON.stringify(skillOut, null, 2) + '\n');
console.log(`wrote ${cases.length} render cases, ${filterOut.length} filter cases`);
