import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile, mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { resolveSdkModel, run, bedrockModelId, discoverGitLabModels } from './sdk-bridge.mjs';

function request(npm, provider, model = 'mock', settings = {}, options = {}) {
  return { model, headers: {}, maxOutputTokens: 100,
    messages: [{ role: 'user', content: 'hello' }], tools: [],
    options: { ...options, _openraid_sdk: { npm, provider, settings } } };
}

function fakeAdapter(methods, extra = {}) {
  const calls = [];
  let settings;
  const provider = Object.fromEntries(methods.map(method => [method, (modelId, options) => {
    const model = { method, modelId, options };
    calls.push(model);
    return model;
  }]));
  return { calls, get settings() { return settings; }, module: { ...extra,
    createTest: value => { settings = value; return provider; } } };
}

test('Azure selector handles chat override and responses/messages/chat/languageModel fallbacks', async () => {
  for (const [methods, useChat, expected] of [
    [['responses', 'messages', 'chat', 'languageModel'], false, 'responses'],
    [['responses', 'messages', 'chat', 'languageModel'], true, 'chat'],
    [['messages', 'chat', 'languageModel'], false, 'messages'],
    [['chat', 'languageModel'], false, 'chat'], [['languageModel'], true, 'languageModel'],
  ]) {
    const fixture = fakeAdapter(methods);
    fixture.module.createAzure = fixture.module.createTest;
    const { model } = await resolveSdkModel(request('@ai-sdk/azure', 'azure', 'deployment', { useCompletionUrls: useChat }),
      { load: async () => fixture.module, env: {} });
    assert.equal(model.method, expected);
    assert.equal(model.modelId, 'deployment');
  }
});

test('Azure Cognitive Services uses source-loader endpoint and deployment URL setting', async () => {
  for (const useDeploymentBasedUrls of [false, true]) {
    const resolved = await resolveSdkModel(request('@ai-sdk/azure', 'azure-cognitive-services', 'gpt-5', { useDeploymentBasedUrls }),
      { env: { AZURE_COGNITIVE_SERVICES_RESOURCE_NAME: 'example' } });
    assert.equal(resolved.settings.baseURL, `https://example.cognitiveservices.azure.com/openai${useDeploymentBasedUrls ? '' : '/v1'}`);
  }
});

test('Bedrock config beats environment, endpoint beats baseURL, bearer skips credential chain', async () => {
  const fixture = fakeAdapter(['languageModel']);
  fixture.module.createAmazonBedrock = fixture.module.createTest;
  let credentials = 0;
  const load = async name => name === '@aws-sdk/credential-providers'
    ? { fromNodeProviderChain: options => { credentials++; return options; } } : fixture.module;
  const resolved = await resolveSdkModel({ ...request('@ai-sdk/amazon-bedrock', 'amazon-bedrock', 'anthropic.claude', {
    region: 'eu-west-1', profile: 'configured', endpoint: 'https://endpoint.example', apiKey: 'bearer',
  }), baseURL: 'https://base.example' }, { load, env: { AWS_REGION: 'us-east-1', AWS_PROFILE: 'env' } });
  assert.equal(resolved.model.modelId, 'eu.anthropic.claude');
  assert.equal(resolved.settings.baseURL, 'https://endpoint.example');
  assert.equal(credentials, 0);
  await resolveSdkModel(request('@ai-sdk/amazon-bedrock', 'amazon-bedrock'), { load, env: { AWS_PROFILE: 'env' } });
  assert.equal(credentials, 1);
  assert.deepEqual(fixture.settings.credentialProvider, { profile: 'env' });
  assert.equal(bedrockModelId('anthropic.claude', 'us-gov-west-1'), 'anthropic.claude');
  assert.equal(bedrockModelId('anthropic.claude', 'ap-northeast-1'), 'jp.anthropic.claude');
});

test('Bedrock Mantle routes exact safeguards through chat and other models through responses', async () => {
  for (const [id, method] of [['openai.gpt-oss-safeguard-20b', 'chat'], ['openai.gpt-oss-safeguard-120b', 'chat'],
    ['openai.gpt-oss-safeguard-other', 'responses'], ['openai.gpt-oss-120b', 'responses']]) {
    const result = await resolveSdkModel(request('@ai-sdk/amazon-bedrock/mantle', 'amazon-bedrock', id, { apiKey: 'bearer' }), { env: {} });
    assert.ok(result.model.provider.includes(method));
    assert.equal(result.model.modelId, id);
  }
});

test('Vertex defaults, continental endpoints, trimmed IDs and explicit URL precedence match loaders', async () => {
  const google = await resolveSdkModel(request('@ai-sdk/google-vertex', 'google-vertex', ' gemini-2.5-pro ', { project: 'configured' }), { env: { GOOGLE_VERTEX_PROJECT: 'env' } });
  assert.equal(google.settings.project, 'configured');
  assert.equal(google.settings.location, 'us-central1');
  assert.equal(google.model.modelId, 'gemini-2.5-pro');
  const anthropic = await resolveSdkModel(request('@ai-sdk/google-vertex/anthropic', 'google-vertex-anthropic'), { env: { GOOGLE_CLOUD_PROJECT: 'project' } });
  assert.equal(anthropic.settings.location, 'global');
  for (const location of ['eu', 'us']) {
    const result = await resolveSdkModel(request('@ai-sdk/google-vertex/anthropic', 'google-vertex-anthropic', ' claude ', { project: 'project', location }), { env: {} });
    assert.equal(result.settings.baseURL, `https://aiplatform.${location}.rep.googleapis.com/v1/projects/project/locations/${location}/publishers/anthropic/models`);
    assert.equal(result.model.modelId, 'claude');
    const proxy = await resolveSdkModel({ ...request('@ai-sdk/google-vertex/anthropic', 'google-vertex-anthropic', 'claude', { project: 'project', location }), baseURL: 'https://proxy.example' }, { env: {} });
    assert.equal(proxy.settings.baseURL, 'https://proxy.example');
  }
});

test('GitLab custom workflow refs use generic workflow and preserve flags/definition', async () => {
  const fixture = fakeAdapter(['agenticChat', 'workflowChat'], { isWorkflowModel: id => id === 'duo-workflow-static' });
  fixture.module.createGitLab = fixture.module.createTest;
  const options = { workflowRef: 'ref/custom', workflowDefinition: 'definition' };
  const custom = await resolveSdkModel(request('gitlab-ai-provider', 'gitlab', 'duo-workflow-custom', { featureFlags: { duo_agent_platform: false } }, options), { load: async () => fixture.module, env: {} });
  assert.equal(custom.model.modelId, 'duo-workflow');
  assert.equal(custom.model.selectedModelRef, 'ref/custom');
  assert.equal(custom.model.options.workflowDefinition, 'definition');
  assert.equal(custom.model.options.featureFlags.duo_agent_platform, false);
  const fixed = await resolveSdkModel(request('gitlab-ai-provider', 'gitlab', 'duo-workflow-static'), { load: async () => fixture.module, env: {} });
  assert.equal(fixed.model.modelId, 'duo-workflow-static');
  const agentic = await resolveSdkModel(request('gitlab-ai-provider', 'gitlab', 'duo-chat', { aiGatewayHeaders: { 'custom-header': 'value' } }), { load: async () => fixture.module, env: {} });
  assert.equal(agentic.model.options.aiGatewayHeaders['anthropic-beta'], 'context-1m-2025-08-07');
  assert.equal(agentic.model.options.aiGatewayHeaders['custom-header'], 'value');
});

test('GitLab discovery forwards API/OAuth authentication and workspace and retains workflow capabilities', async () => {
  for (const authType of ['api', 'oauth']) {
    let captured;
    const models = await discoverGitLabModels({ ...request('gitlab-ai-provider', 'gitlab', '', { authType, authToken: 'oauth-token' }),
      apiKey: 'api-token', baseURL: 'https://gitlab.example', options: {
        _openraid_sdk: { workspace: '/workspace', settings: { authType, authToken: 'oauth-token' } },
      } }, { load: async () => ({ discoverWorkflowModels: async (config, options) => {
        captured = { config, options };
        return { models: [{ id: 'duo-workflow-custom', ref: 'custom/model', name: 'Custom', context: 99999, output: 4096 }] };
      } }), env: {} });
    assert.deepEqual(captured.config.getHeaders(), authType === 'oauth' ? { Authorization: 'Bearer oauth-token' } : { 'PRIVATE-TOKEN': 'api-token' });
    assert.equal(captured.options.workingDirectory, '/workspace');
    assert.equal(captured.options.cacheKey, authType === 'oauth' ? 'oauth-token' : 'api-token');
    assert.equal(models[0].options.workflowRef, 'custom/model');
    assert.equal(models[0].limit.context, 99999);
    assert.equal(models[0].tool_call, true);
    assert.equal(models[0].provider.npm, 'gitlab-ai-provider');
  }
  assert.deepEqual(await discoverGitLabModels(request('gitlab-ai-provider', 'gitlab'), { env: {}, load: async () => { throw new Error('must not import without credentials'); } }), []);
});

test('actual GitLab SDK discovers local project workflows over REST/GraphQL with account-isolated auth', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'openraid-gitlab-discovery-'));
  const previousCache = process.env.XDG_CACHE_HOME;
  process.env.XDG_CACHE_HOME = join(directory, 'cache');
  const requests = [];
  const server = createServer(async (req, res) => {
    let body = '';
    for await (const chunk of req) body += chunk;
    requests.push({ url: req.url, api: req.headers['private-token'], bearer: req.headers.authorization, body: body ? JSON.parse(body) : null });
    res.writeHead(200, { 'Content-Type': 'application/json' });
    if (req.url.startsWith('/api/v4/projects/')) res.end(JSON.stringify({ id: 42, path: 'project', path_with_namespace: 'group/project', name: 'Project', namespace: { id: 5 } }));
    else if (req.url === '/api/graphql') res.end(JSON.stringify({ data: { aiChatAvailableModels: { defaultModel: null, pinnedModel: null,
      selectableModels: [{ name: 'Custom', ref: 'audit/custom' }] }, metadata: { featureFlags: [], version: '19' } } }));
    else res.end('{}');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const endpoint = `http://127.0.0.1:${server.address().port}`;
    execFileSync('git', ['init', '--quiet', directory]);
    execFileSync('git', ['-C', directory, 'remote', 'add', 'origin', `${endpoint}/group/project.git`]);
    await mkdir(join(directory, 'cache', 'opencode'), { recursive: true });
    await writeFile(join(directory, 'cache', 'opencode', 'gitlab-model-configs.json'), JSON.stringify({
      updatedAt: new Date().toISOString(), configs: { 'audit/custom': { context: 99999, output: 4096 } },
    }));
    for (const authType of ['api', 'oauth']) {
      const models = await discoverGitLabModels({ ...request('gitlab-ai-provider', 'gitlab'), baseURL: endpoint,
        apiKey: 'api-live', options: { _openraid_sdk: { workspace: directory, settings: { authType, authToken: 'oauth-live' } } } });
      assert.equal(models[0].id, 'duo-workflow-audit-custom');
      assert.equal(models[0].options.workflowRef, 'audit/custom');
      assert.equal(models[0].limit.context, 99999);
      assert.equal(models[0].limit.output, 4096);
    }
    assert.equal(requests.length, 4, 'each account must discover its own project/models');
    assert.equal(requests[0].api, 'api-live');
    assert.equal(requests[0].bearer, undefined);
    assert.equal(requests[2].bearer, 'Bearer oauth-live');
    assert.equal(requests[2].api, undefined);
    assert.equal(requests[1].body.variables.projectId, 'gid://gitlab/Project/42');
  } finally {
    if (previousCache === undefined) delete process.env.XDG_CACHE_HOME; else process.env.XDG_CACHE_HOME = previousCache;
    await new Promise(resolve => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('Cloudflare explicit proxy avoids account/gateway validation', async () => {
  const resolved = await resolveSdkModel({ ...request('ai-gateway-provider', 'cloudflare-ai-gateway', 'google/gemini'), baseURL: 'https://proxy.example/v1' }, { env: {} });
  assert.equal(resolved.model.modelId, 'google/gemini');
  assert.equal(resolved.model.provider, 'cloudflare-ai-gateway.chat');
});

test('Cloudflare real passthrough preserves OpenAI slug, cache controls and gateway-only authentication', async () => {
  let captured;
  const completion = await run(request('ai-gateway-provider', 'cloudflare-ai-gateway', 'openai/gpt-5.4', {
    accountId: 'account', gatewayId: 'gateway', apiKey: 'cf-private', cacheTtl: 42, cacheKey: 'key',
    skipCache: true, collectLog: false, metadata: { test: 'audit' }, headers: { 'x-custom': 'value' },
    fetch: async (url, init) => {
      captured = { url, headers: new Headers(init.headers), body: JSON.parse(init.body), signal: init.signal };
      return Response.json({ id: 'resp', object: 'response', created_at: 1, status: 'completed', model: 'gpt-5.4',
        output: [{ type: 'message', id: 'msg', role: 'assistant', status: 'completed', content: [{ type: 'output_text', text: 'gateway ready', annotations: [] }] }],
        usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } });
    },
  }, { reasoningEffort: 'high' }));
  assert.equal(completion.content, 'gateway ready');
  assert.equal(captured.url, 'https://gateway.ai.cloudflare.com/v1/account/gateway');
  assert.equal(captured.headers.get('cf-aig-authorization'), 'Bearer cf-private');
  assert.equal(captured.headers.get('cf-aig-cache-ttl'), '42');
  assert.equal(captured.headers.get('cf-aig-cache-key'), 'key');
  assert.equal(captured.headers.get('cf-aig-skip-cache'), 'true');
  assert.equal(captured.headers.get('cf-aig-collect-log'), 'false');
  assert.equal(captured.headers.get('x-custom'), 'value');
  assert.equal(captured.body[0].query.model, 'gpt-5.4');
  assert.equal(captured.body[0].query.reasoning.effort, 'high');
  assert.equal(JSON.stringify(captured.body).includes('cf-private'), false);
  assert.equal(captured.signal, undefined);
});

test('Cloudflare Anthropic slugs normalize dots; workers alone receive upstream CF token', async () => {
  const anthropic = await resolveSdkModel(request('ai-gateway-provider', 'cloudflare-ai-gateway', 'anthropic/claude-haiku-4.5', { accountId: 'a', gateway: 'g', apiKey: 'token' }));
  assert.equal(anthropic.model.models[0].modelId, 'claude-haiku-4-5');
  for (const id of ['workers-ai/@cf/model', '@cf/model', 'google/gemini', 'xai/grok']) {
    const result = await resolveSdkModel(request('ai-gateway-provider', 'cloudflare-ai-gateway', id, { accountId: 'a', gateway: 'g', apiKey: 'token' }));
    assert.equal(result.model.modelId, id);
    if (id.startsWith('google/') || id.startsWith('xai/')) assert.equal(result.model.provider, 'cloudflare-ai-gateway.chat');
  }
});

test('every bundled catalog SDK constructs a usable model through the actual bridge resolver', async () => {
  const catalog = JSON.parse(await readFile(new URL('../data/models.json', import.meta.url), 'utf8'));
  const representatives = new Map();
  for (const [providerId, provider] of Object.entries(catalog)) {
    for (const [id, model] of Object.entries(provider.models ?? {})) {
      const npm = model.provider?.npm ?? provider.npm;
      if (npm && npm !== '@ai-sdk/github-copilot' && !representatives.has(npm)) representatives.set(npm, { providerId, id });
    }
  }
  const failures = [];
  for (const [npm, { providerId, id }] of representatives) {
    try {
      const resolved = await resolveSdkModel({ ...request(npm, providerId, id, {
        region: 'us-east-1', project: 'audit-project', location: 'us-central1', resourceName: 'audit-resource',
        accountId: 'audit-account', gatewayId: 'audit-gateway', apiKey: 'audit-only',
      }), apiKey: 'audit-only' }, { env: {} });
      assert.ok(['v2', 'v3'].includes(resolved.model.specificationVersion), `${npm}: language-model protocol`);
      assert.equal(typeof resolved.model.doGenerate, 'function', `${npm}: generation method`);
      assert.ok(resolved.model.modelId, `${npm}: model identity`);
    } catch (error) { failures.push(`${npm}: ${error.message}`); }
  }
  assert.deepEqual(failures, []);
  assert.ok(representatives.size >= 29);
});

test('actual SAP factory keeps its SDK namespace and configured compatible names are not overwritten', async () => {
  const sap = await resolveSdkModel(request('@jerome-benoit/sap-ai-provider-v2', 'sap-ai-core', 'gpt-4.1'));
  assert.ok(sap.model.provider.startsWith('sap-ai.'));
  assert.equal(sap.namespace, 'sap-ai');
  const customSap = await resolveSdkModel(request('@jerome-benoit/sap-ai-provider-v2', 'sap-ai-core', 'gpt-4.1', { name: 'custom-sap' }));
  assert.ok(customSap.model.provider.startsWith('custom-sap.'));
  assert.equal(customSap.namespace, 'custom-sap');
  const compatible = await resolveSdkModel(request('@ai-sdk/openai-compatible', 'catalog-id', 'model', { name: 'configured-name' }));
  assert.equal(compatible.model.provider, 'configured-name.chat');
  assert.equal(compatible.namespace, 'configured-name');
});

test('Salad V4 adapter compatibility actually generates reasoning/tool calls/usage through AI SDK 6', async () => {
  let captured;
  const server = createServer(async (req, res) => {
    let body = '';
    for await (const chunk of req) body += chunk;
    captured = JSON.parse(body);
    assert.equal(req.headers.authorization, 'Bearer salad-audit');
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ id: 'salad', model: 'model', choices: [{ index: 0, finish_reason: 'tool_calls',
      message: { role: 'assistant', content: 'ready', reasoning_content: 'reasoned', tool_calls: [
        { id: 'salad-call', type: 'function', function: { name: 'read_file', arguments: '{"path":"README.md"}' } },
      ] } }], usage: { prompt_tokens: 4, completion_tokens: 3, total_tokens: 7 } }));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const completion = await run({ ...request('@saladtechnologies-oss/ai-sdk-provider', 'salad-cloud', 'model', {}, { reasoningEffort: 'high' }),
      apiKey: 'salad-audit', baseURL: `http://127.0.0.1:${server.address().port}/v1`,
      tools: [{ type: 'function', function: { name: 'read_file', description: 'read', parameters: { type: 'object', properties: { path: { type: 'string' } } } } }] });
    assert.equal(captured.reasoning_effort, 'high');
    assert.equal(completion.content, 'ready');
    assert.equal(completion.tool_calls[0].id, 'salad-call');
    assert.equal(completion.usage.input_tokens, 4);
    assert.equal(completion.usage.output_tokens, 3);
    assert.ok(completion.native_content.sdk.some(part => part.type === 'reasoning' && part.text === 'reasoned'));
  } finally { await new Promise(resolve => server.close(resolve)); }
});
