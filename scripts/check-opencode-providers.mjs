// Compare the actual OpenCode provider source, not just the models.dev catalog.
import { readFile } from 'node:fs/promises';
import assert from 'node:assert/strict';
import { reviewedCommit, reviewedLoaders, verifySourceContracts } from './provider-source-contracts.mjs';
import { resolveSdkModel } from './sdk-bridge.mjs';

const url = 'https://raw.githubusercontent.com/anomalyco/opencode/dev/packages/opencode/src/provider/provider.ts';
const response = await fetch(url);
assert(response.ok, `OpenCode source fetch failed: ${response.status}`);
const source = await response.text();
const pinnedUrl = `https://raw.githubusercontent.com/anomalyco/opencode/${reviewedCommit}/packages/opencode/src/provider/provider.ts`;
const pinnedResponse = await fetch(pinnedUrl);
assert(pinnedResponse.ok, `Pinned source fetch failed: ${pinnedResponse.status}`);
const pinned = await pinnedResponse.text();
assert.equal(source, pinned, 'Current source differs from reviewed commit; review all loader changes before accepting parity');
const start = source.indexOf('const BUNDLED_PROVIDERS:');
const end = source.indexOf('\ntype CustomModelLoader', start);
assert(start >= 0 && end > start, 'OpenCode provider source layout changed; review the audit parser');
const bundled = [...source.slice(start, end).matchAll(/^\s*"([^"]+)":/gm)].map(match => match[1]);
const customStart = source.indexOf('function custom(');
const customEnd = source.indexOf('\nconst ProviderApiInfo', customStart);
const loaders = [...source.slice(customStart, customEnd).matchAll(/^    (?:"([^"]+)"|([\w-]+)): (?:\(|Effect)/gm)].map(match => match[1] ?? match[2]);
const catalog = JSON.parse(await readFile(new URL('../data/models.json', import.meta.url), 'utf8'));
assert.deepEqual(loaders.sort(), [...reviewedLoaders].sort(), 'source loaders lack behavioral review');
const contracts = await verifySourceContracts(source);
console.log(`Executed upstream contracts: ${contracts.loaders} loader fixtures, ${contracts.bedrockCases} Bedrock mappings, ${contracts.selectors} routing selectors.`);
let failures = 0;
for (const npm of bundled) {
  if (npm === '@ai-sdk/github-copilot') {
    console.log(`${npm}: native Copilot model routing + shared account authentication`);
    continue;
  }
  try {
    const adapter = await import(npm);
    assert(Object.entries(adapter).some(([name, value]) => name.startsWith('create') && typeof value === 'function'));
    const candidate = Object.values(catalog).flatMap(provider => Object.entries(provider.models ?? {})
      .filter(([, entry]) => (entry.provider?.npm ?? provider.npm) === npm).map(([id]) => id))[0] ?? 'audit-model';
    const { model } = await resolveSdkModel({ model: candidate, apiKey: 'audit-only', options: {
      _openraid_sdk: { npm, provider: npm.split('/').at(-1), settings: { project: 'audit', location: 'us-central1', resourceName: 'audit' } },
    } }, { env: {} });
    assert(['v2', 'v3'].includes(model.specificationVersion) && typeof model.doGenerate === 'function');
    console.log(`${npm}: actual bridge model construction`);
  } catch { failures++; console.error(`${npm}: MISSING`); }
}
for (const provider of loaders) {
  if (!catalog[provider]) { failures++; console.error(`${provider}: missing source-loader catalog entry`); }
}
console.log(`OpenCode source audit: ${bundled.length} bundled SDKs, ${loaders.length} custom loaders, ${Object.keys(catalog).length} catalog providers.`);
console.log(`Source: ${url}`);
console.log(`Reviewed commit: ${reviewedCommit}; behavioral evidence: docs/SOURCE_PROVIDER_AUDIT.md and npm test --prefix scripts`);
if (failures) process.exitCode = 1;
