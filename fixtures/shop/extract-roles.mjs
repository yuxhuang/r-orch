// Extract role prompt constants byte-exactly from the TS package into .md files
// so the Rust crate can include_str! them. Run with the TS package cwd on PATH
// resolution; pass the output directory as argv[2].
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const outDir = process.argv[2];
if (!outDir) {
  console.error('usage: node extract-roles.mjs <out-dir>');
  process.exit(2);
}

const tsRoot = process.argv[3];
const { AGENT_SHOP_TEMPLATES } = await import(join(tsRoot, 'src/domain/agent-shop.ts'));
const { getDefaultAgents } = await import(join(tsRoot, 'src/domain/default-agents.ts'));

await mkdir(outDir, { recursive: true });

for (const t of AGENT_SHOP_TEMPLATES) {
  await writeFile(join(outDir, `${t.key}.md`), t.role, 'utf8');
  console.log(`${t.key}.md ${Buffer.byteLength(t.role)}`);
}

const creator = getDefaultAgents('claude')[0];
await writeFile(join(outDir, 'agent-creator.md'), creator.role, 'utf8');
console.log(`agent-creator.md ${Buffer.byteLength(creator.role)}`);
