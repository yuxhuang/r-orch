// Emit the shop catalogue as JSON straight from the TS source, so the Rust
// side can be byte-compared against it (V28). argv: <ts-root> <out-dir>
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const [tsRoot, outDir] = process.argv.slice(2);
const { AGENT_SHOP_TEMPLATES } = await import(join(tsRoot, 'src/domain/agent-shop.ts'));
const { ORG_TEMPLATES } = await import(join(tsRoot, 'src/domain/org-shop.ts'));
const { getDefaultAgents } = await import(join(tsRoot, 'src/domain/default-agents.ts'));

await mkdir(outDir, { recursive: true });

const write = async (name, value) => {
  await writeFile(join(outDir, name), JSON.stringify(value, null, 2) + '\n', 'utf8');
  console.log(name);
};

await write('agent-shop.json', AGENT_SHOP_TEMPLATES);
await write('org-shop.json', ORG_TEMPLATES);
await write('default-agents.json', {
  claude: getDefaultAgents('claude'),
  codex: getDefaultAgents('codex'),
  antigravity: getDefaultAgents('antigravity'),
  shell: getDefaultAgents('shell'),
});
