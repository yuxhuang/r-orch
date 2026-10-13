/**
 * V7/V8 oracle side: read and write `.orchestry/` with the **real TypeScript
 * store classes**, not a reimplementation of them.
 *
 * Run through the vendored package's own `tsx` so the stores, `js-yaml` and
 * every option they pass (`indent: 2, lineWidth: 120, noRefs: true,
 * sortKeys: false`) are exactly the ones the reference binary uses. A
 * hand-written mimic here would test our reading of the TS source rather than
 * the TS source.
 *
 *   tsx cross_compat.mts <mode> <ts-package-root> <project-root>
 *
 * `read`  — load every entity and report it, with the JS *type* of each
 *           timestamp field, so a value that came back as a `Date` is visible
 *           (V8). JSON.stringify alone would hide it by re-serializing a Date
 *           as an ISO string.
 * `write` — write one of each entity through the TS stores, so the Rust side
 *           can read what js-yaml actually emits — including the folded `>-`
 *           block scalars that `serde_yaml_ng` never produces.
 */
import { join } from 'node:path';

const [mode, tsRoot, projectRoot] = process.argv.slice(2);

const load = (relative) => import(join(tsRoot, relative));
const { Paths } = await load('src/infrastructure/storage/paths.ts');
const { TaskStore } = await load('src/infrastructure/storage/task-store.ts');
const { AgentStore } = await load('src/infrastructure/storage/agent-store.ts');
const { GoalStore } = await load('src/infrastructure/storage/goal-store.ts');
const { TeamStore } = await load('src/infrastructure/storage/team-store.ts');
const { MessageStore } = await load('src/infrastructure/storage/message-store.ts');
const { RunStore } = await load('src/infrastructure/storage/run-store.ts');
const { ContextStore } = await load('src/infrastructure/storage/context-store.ts');
const { StateStore } = await load('src/infrastructure/storage/state-store.ts');
const { ConfigStore } = await load('src/infrastructure/storage/config-store.ts');

const paths = new Paths(projectRoot);
const stores = {
  task: new TaskStore(paths),
  agent: new AgentStore(paths),
  goal: new GoalStore(paths),
  team: new TeamStore(paths),
  message: new MessageStore(paths),
  run: new RunStore(paths),
  context: new ContextStore(paths),
  state: new StateStore(paths),
  config: new ConfigStore(paths),
};

/**
 * Tag values whose JS type would otherwise be erased by JSON.stringify. A
 * `Date` here means js-yaml resolved an unquoted timestamp — the bug V8
 * exists to catch.
 */
function tag(value) {
  if (value === null) return null;
  if (value === undefined) return { __js: 'undefined' };
  if (value instanceof Date) return { __js: 'Date', iso: value.toISOString() };
  if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
    return { __js: 'Buffer', base64: Buffer.from(value).toString('base64') };
  }
  if (value instanceof Set) return { __js: 'Set', values: [...value].map(tag) };
  if (value instanceof Map) return { __js: 'Map' };
  if (Array.isArray(value)) return value.map(tag);
  const kind = typeof value;
  if (kind === 'string' || kind === 'number' || kind === 'boolean') return value;
  if (kind !== 'object') return { __js: kind, text: String(value) };
  const proto = Object.getPrototypeOf(value);
  if (proto !== Object.prototype && proto !== null) {
    return { __js: value.constructor ? value.constructor.name : 'unknown' };
  }
  const out = {};
  for (const [key, child] of Object.entries(value)) out[key] = tag(child);
  return out;
}

if (mode === 'read') {
  const runs = await stores.run.listAll();
  const out = {
    tasks: (await stores.task.list()).map(tag),
    agents: (await stores.agent.list()).map(tag),
    goals: (await stores.goal.list()).map(tag),
    teams: (await stores.team.list()).map(tag),
    // `MessageStore.list()` sorts with `created_at.localeCompare(...)`, which
    // **throws** if the field came back as a Date. Calling it is the test.
    messages: (await stores.message.list()).map(tag),
    runs: runs.map(tag),
    runEvents: runs.length ? (await stores.run.readEvents(runs[0].id)).map(tag) : [],
    context: (await stores.context.list()).map(tag),
    state: tag(await stores.state.read()),
    config: tag(await stores.config.read()),
  };
  process.stdout.write(JSON.stringify(out));
} else if (mode === 'write') {
  // Written through the real stores, so the bytes are js-yaml's — including
  // the folded `>-` style it picks for a long single-line scalar at
  // lineWidth 120, which serde_yaml_ng never emits.
  const stamp = '2026-10-13T16:37:58.959Z';
  const longLine = `ts-written ${'word '.repeat(60)}end`;

  await stores.task.save({
    id: 'tsk_tsWR1TE',
    title: 'Written by ts',
    description: longLine,
    status: 'todo',
    priority: 2,
    labels: ['from-ts'],
    depends_on: [],
    created_at: stamp,
    updated_at: stamp,
    attempts: 0,
    max_attempts: 3,
    goalId: 'goal_tsWR1TE',
  });

  await stores.agent.save({
    id: 'agt_tsWR1TE',
    name: 'ts-agent',
    adapter: 'shell',
    role: 'Line one\n\nLine two with trailing spaces   \n\tand a tab',
    config: { command: 'echo ts', approval_policy: 'auto', max_turns: 50 },
    status: 'idle',
    stats: { tasks_completed: 0, tasks_failed: 0, total_runs: 0, total_runtime_ms: 0 },
  });

  await stores.goal.save({
    id: 'goal_tsWR1TE',
    title: 'Ship it',
    description: longLine,
    status: 'active',
    created_at: stamp,
    updated_at: stamp,
  });

  await stores.team.save({
    id: 'team_tsWR1TE',
    name: 'ts-squad',
    status: 'active',
    members: [{ agent_id: 'agt_tsWR1TE', role: 'lead', joined_at: stamp }],
    task_pool: [],
    lead_agent_id: 'agt_tsWR1TE',
    created_at: stamp,
    updated_at: stamp,
    config: { auto_claim: true, message_ttl_ms: 86400000 },
  });

  await stores.message.save({
    id: 'msg_tsWR1TE',
    channel: 'direct',
    from_agent_id: 'cli',
    to_agent_id: 'agt_tsWR1TE',
    subject: 'Hi',
    body: longLine,
    created_at: stamp,
    status: 'pending',
  });

  await stores.run.save({
    id: 'run_tsWR1TE',
    task_id: 'tsk_tsWR1TE',
    agent_id: 'agt_tsWR1TE',
    attempt: 1,
    status: 'succeeded',
    started_at: stamp,
    finished_at: stamp,
    workspace_path: '/tmp/ws',
    prompt: longLine,
    pid: 4242,
  });
  await stores.run.appendEvent('run_tsWR1TE', {
    timestamp: stamp,
    type: 'agent_output',
    data: 'written by ts',
  });

  await stores.context.set('ts-key', longLine);
  process.stdout.write('ok');
} else {
  process.stderr.write(`unknown mode: ${mode}\n`);
  process.exit(2);
}
