// Execute the reviewed upstream loader functions with isolated dependency
// fixtures. This is deliberately independent of installing SDK factories.
import assert from 'node:assert/strict';
import os from 'node:os';
import * as nodeModule from 'node:module';
import { bedrockModelId, resolveSdkModel } from './sdk-bridge.mjs';

export const reviewedCommit = '35fc7a776cddb72d334ee60590c3cce41154be05';
export const reviewedLoaders = ['anthropic', 'opencode', 'openai', 'meta', 'xai', 'github-copilot',
  'azure', 'azure-cognitive-services', 'amazon-bedrock', 'llmgateway', 'openrouter', 'nvidia', 'vercel',
  'google-vertex', 'google-vertex-anthropic', 'sap-ai-core', 'zenmux', 'gitlab', 'cloudflare-workers-ai',
  'cloudflare-ai-gateway', 'cerebras', 'kilo', 'snowflake-cortex'];

function effect(value) {
  return { value: Promise.resolve(value), *[Symbol.iterator]() { return yield this; } };
}

export async function verifySourceContracts(source) {
  assert.equal(typeof nodeModule.stripTypeScriptTypes, 'function', 'source audit needs Node.js 22.13+ (runtime bridge needs 22.12+)');
  const helpers = source.slice(source.indexOf('function googleVertexAnthropicBaseURL('), source.indexOf('\ntype BundledSDK'));
  const customStart = source.indexOf('function selectAzureLanguageModel(');
  const customEnd = source.indexOf('\nconst ProviderApiInfo', customStart);
  assert.ok(customStart >= 0 && customEnd > customStart, 'upstream source layout changed');
  const code = nodeModule.stripTypeScriptTypes(`${helpers}\n${source.slice(customStart, customEnd)}`);
  const Effect = { succeed: effect, promise: fn => effect(fn()), fnUntraced: fn => (...args) => effect((async () => {
    const iterator = fn(...args);
    let step = iterator.next();
    while (!step.done) step = iterator.next(await step.value.value);
    return step.value;
  })()) };
  const env = { AWS_REGION: 'us-east-1', AWS_BEARER_TOKEN_BEDROCK: 'audit', GOOGLE_VERTEX_PROJECT: 'audit',
    GOOGLE_CLOUD_PROJECT: 'audit', GOOGLE_CLOUD_LOCATION: 'eu', AZURE_RESOURCE_NAME: 'resource',
    AZURE_COGNITIVE_SERVICES_RESOURCE_NAME: 'cognitive', CLOUDFLARE_ACCOUNT_ID: 'account',
    CLOUDFLARE_GATEWAY_ID: 'gateway', CLOUDFLARE_API_TOKEN: 'token', CLOUDFLARE_API_KEY: 'token',
    SNOWFLAKE_ACCOUNT: 'account', SNOWFLAKE_CORTEX_TOKEN: 'token', AICORE_SERVICE_KEY: 'audit' };
  const dep = { env: () => effect(env), get: key => effect(env[key]),
    auth: () => effect({ type: 'api', key: 'audit', metadata: {} }), config: () => effect({ provider: {} }) };
  const factory = new Function('Effect', 'iife', 'process', 'dep', 'InstanceState', 'InstallationVersion', 'os',
    `const OPENAI_HEADER_TIMEOUT_DEFAULT = 300_000;\n${code}\nreturn custom(dep);`);
  const loaders = factory(Effect, fn => fn(), { env }, dep, { directory: effect(process.cwd()) }, 'audit', os);
  assert.deepEqual(Object.keys(loaders).sort(), [...reviewedLoaders].sort(), 'unreviewed source loaders require a new audit');
  const resolved = {};
  for (const name of reviewedLoaders) {
    resolved[name] = await loaders[name]({ id: name, source: 'config', env: ['AUDIT_KEY'], options: {},
      models: { free: { cost: { input: 0 } }, paid: { cost: { input: 1 } } } }).value;
    assert.equal(typeof resolved[name].autoload, 'boolean', `${name}: loader contract`);
  }
  // Source headers are reviewed separately from OpenRaid's own brand values.
  assert.ok(resolved.anthropic.options.headers['anthropic-beta'].includes('fine-grained-tool-streaming'));
  assert.equal(resolved['google-vertex-anthropic'].options.location, 'eu');
  assert.ok(resolved['google-vertex-anthropic'].options.baseURL.includes('aiplatform.eu.rep.googleapis.com'));
  assert.equal(resolved['azure-cognitive-services'].options.baseURL, 'https://cognitive.cognitiveservices.azure.com/openai/v1');
  assert.equal(resolved['snowflake-cortex'].options.baseURL, 'https://account.snowflakecomputing.com/api/v2/cortex/v1');

  const sdk = Object.fromEntries(['chat', 'responses', 'messages', 'languageModel'].map(method => [method, id => ({ method, modelId: id })]));
  for (const name of ['openai', 'meta', 'xai']) assert.equal((await resolved[name].getModel(sdk, 'gpt-5')).method, 'responses');
  const regions = ['us-east-1', 'us-gov-west-1', 'eu-west-1', 'eu-central-2', 'ap-southeast-2', 'ap-southeast-4', 'ap-northeast-1', 'ap-south-1'];
  const models = ['anthropic.claude-sonnet-4-5', 'anthropic.claude-haiku', 'amazon.nova-pro', 'meta.llama3', 'deepseek.r1', 'global.anthropic.claude', 'arn:aws:bedrock:profile'];
  for (const region of regions) for (const id of models) {
    const sourceModel = await resolved['amazon-bedrock'].getModel(sdk, id, { region });
    assert.equal(bedrockModelId(id, region), sourceModel.modelId, `Bedrock ${region}/${id}: upstream mapping parity`);
  }
  for (const id of ['openai.gpt-oss-safeguard-20b', 'openai.gpt-oss-safeguard-120b', 'openai.gpt-oss-120b']) {
    const sourceModel = await resolved['amazon-bedrock'].getModel(sdk, id, {}, { api: { npm: '@ai-sdk/amazon-bedrock/mantle' } });
    const actual = await resolveSdkModel({ model: id, options: { _openraid_sdk: { npm: '@ai-sdk/amazon-bedrock/mantle', provider: 'amazon-bedrock', settings: { apiKey: 'audit' } } } }, { env: {} });
    assert.ok(actual.model.provider.includes(sourceModel.method), `Mantle ${id}: upstream dispatch parity`);
  }
  return { loaders: reviewedLoaders.length, bedrockCases: regions.length * models.length, selectors: 6 };
}
