// Audit every distinct adapter required by the complete offline catalog.
// Run after npm ci --prefix scripts: node scripts/check-catalog-adapters.mjs
import { readFile } from 'node:fs/promises';

const catalog = JSON.parse(await readFile(new URL('../data/models.json', import.meta.url), 'utf8'));
const adapters = new Map();
const placeholders = new Set();
let modelCount = 0;
for (const [providerId, provider] of Object.entries(catalog)) {
  for (const match of (provider.api ?? '').matchAll(/\{([^}]+)\}/g)) placeholders.add(match[1]);
  for (const model of Object.values(provider.models ?? {})) {
    modelCount++;
    const npm = model.provider?.npm ?? provider.npm ?? '@ai-sdk/openai-compatible';
    const providers = adapters.get(npm) ?? new Set();
    providers.add(providerId);
    adapters.set(npm, providers);
    for (const match of (model.provider?.api ?? '').matchAll(/\{([^}]+)\}/g)) placeholders.add(match[1]);
  }
}

let failures = 0;
for (const [npm, providers] of [...adapters.entries()].sort(([a], [b]) => a.localeCompare(b))) {
  if (npm === '@ai-sdk/github-copilot') {
    console.log(`${npm}: OpenRaid native OAuth adapter (${providers.size} provider)`);
    continue;
  }
  try {
    const adapter = await import(npm);
    const factories = Object.keys(adapter).filter((name) => name.startsWith('create') && typeof adapter[name] === 'function');
    if (!factories.length) throw new Error('no provider factory exported');
    console.log(`${npm}: ${factories.join(', ')} (${providers.size} providers)`);
  } catch (error) {
    failures++;
    console.error(`${npm}: MISSING ADAPTER (${[...providers].join(', ')}); ${error.code ?? error.message}`);
  }
}
console.log(`Audited ${adapters.size} adapters used by ${Object.keys(catalog).length} providers / ${modelCount} models.`);
console.log(`Endpoint placeholders: ${[...placeholders].sort().join(', ') || 'none'}`);
if (failures) process.exitCode = 1;
