import { pathToFileURL } from 'node:url';
import { createInterface } from 'node:readline';
import { installNoDeadlineTransport } from './http-transport.mjs';

// Includes SDK fetches, workflow discovery and cloud-credential HTTP clients
// which use the shared global dispatcher; idle keepalive remains independent.
installNoDeadlineTransport();

export function messagesForSdk(messages) {
  const names = new Map();
  return messages.map((message) => {
    if (message.role === 'tool') {
      return { role: 'tool', content: [{ type: 'tool-result', toolCallId: message.tool_call_id,
        toolName: names.get(message.tool_call_id) ?? message.name ?? 'unknown',
        output: { type: 'text', value: typeof message.content === 'string' ? message.content : JSON.stringify(message.content) } }] };
    }
    if (message.role === 'assistant') {
      for (const call of message.tool_calls ?? []) names.set(call.id, call.function.name);
      if (Array.isArray(message._openraid_native?.sdk)) {
        return { role: 'assistant', content: message._openraid_native.sdk };
      }
      const content = [];
      if (message.content) content.push({ type: 'text', text: message.content });
      for (const call of message.tool_calls ?? []) {
        content.push({ type: 'tool-call', toolCallId: call.id, toolName: call.function.name,
          input: JSON.parse(call.function.arguments || '{}') });
      }
      return { role: 'assistant', content };
    }
    return { role: message.role, content: message.content ?? '' };
  });
}

const factories = {
  '@ai-sdk/anthropic': 'createAnthropic', '@ai-sdk/azure': 'createAzure',
  '@ai-sdk/google': 'createGoogleGenerativeAI', '@ai-sdk/openai': 'createOpenAI',
  '@ai-sdk/xai': 'createXai', '@ai-sdk/mistral': 'createMistral',
  '@ai-sdk/groq': 'createGroq', '@ai-sdk/deepinfra': 'createDeepInfra',
  '@ai-sdk/cerebras': 'createCerebras', '@ai-sdk/cohere': 'createCohere',
  '@ai-sdk/gateway': 'createGateway', '@ai-sdk/togetherai': 'createTogetherAI',
  '@ai-sdk/perplexity': 'createPerplexity', '@ai-sdk/vercel': 'createVercel',
  '@ai-sdk/alibaba': 'createAlibaba', 'ai-gateway-provider': 'createAiGateway',
  '@ai-sdk/amazon-bedrock': 'createAmazonBedrock',
  '@ai-sdk/amazon-bedrock/mantle': 'createBedrockMantle',
  '@ai-sdk/google-vertex': 'createVertex',
  '@ai-sdk/google-vertex/anthropic': 'createVertexAnthropic',
  '@ai-sdk/openai-compatible': 'createOpenAICompatible',
  '@openrouter/ai-sdk-provider': 'createOpenRouter',
  '@jerome-benoit/sap-ai-provider-v2': 'createSAPAIProvider',
  '@aihubmix/ai-sdk-provider': 'createAihubmix',
  '@qvac/ai-sdk-provider': 'createQvac',
  '@saladtechnologies-oss/ai-sdk-provider': 'createSaladCloud',
  'gitlab-ai-provider': 'createGitLab',
  'merge-gateway-ai-sdk-provider': 'createMergeGateway',
  'watsonx-ai-provider': 'createWatsonx',
  'venice-ai-sdk-provider': 'createVenice',
};

const namespaces = {
  '@ai-sdk/amazon-bedrock': 'bedrock', '@ai-sdk/google-vertex': 'vertex',
  '@ai-sdk/google-vertex/anthropic': 'anthropic', '@ai-sdk/azure': 'azure',
  '@openrouter/ai-sdk-provider': 'openrouter', 'gitlab-ai-provider': 'gitlab',
  'venice-ai-sdk-provider': 'venice',
  '@jerome-benoit/sap-ai-provider-v2': 'sap-ai', '@aihubmix/ai-sdk-provider': 'aihubmix',
  '@qvac/ai-sdk-provider': 'qvac', '@saladtechnologies-oss/ai-sdk-provider': 'salad-cloud',
  'merge-gateway-ai-sdk-provider': 'mergeGateway', 'watsonx-ai-provider': 'watsonx',
};

export function bedrockModelId(model, region) {
  if (model.startsWith('arn:') || /^(global|us|eu|jp|apac|au)\./.test(model)) return model;
  if (region.startsWith('us-') && !region.startsWith('us-gov') && /nova-(micro|lite|pro|premier|2)|claude|deepseek\.r1/.test(model)) return `us.${model}`;
  if (/^eu-(west-[123]|north-1|central-1|south-[12])$/.test(region) && /claude|nova-lite|nova-micro|llama3|pixtral/.test(model)) return `eu.${model}`;
  if (['ap-southeast-2', 'ap-southeast-4'].includes(region) && /anthropic\.claude-sonnet-4-5|anthropic\.claude-haiku/.test(model)) return `au.${model}`;
  if (region.startsWith('ap-') && /claude|nova-(lite|micro|pro)/.test(model)) return `${region === 'ap-northeast-1' ? 'jp' : 'apac'}.${model}`;
  return model;
}

async function gatewayModel(settings, request, load, env) {
  const { createOpenAICompatible } = await load('@ai-sdk/openai-compatible');
  // An explicit proxy URL is already fully specified and needs no CF IDs.
  const override = settings.gatewayBaseURL ?? settings.baseURL;
  if (override && !/gateway\.ai\.cloudflare\.com|api\.cloudflare\.com/.test(override)) {
    return createOpenAICompatible({ ...settings, name: 'cloudflare-ai-gateway', baseURL: override })(request.model);
  }
  const { createAiGateway, parseAiGatewayOptions } = await load('ai-gateway-provider');
  const accountId = settings.accountId ?? env.CLOUDFLARE_ACCOUNT_ID;
  const gateway = settings.gateway ?? settings.gatewayId ?? env.CLOUDFLARE_GATEWAY_ID;
  const apiKey = settings.apiKey ?? env.CLOUDFLARE_API_TOKEN ?? env.CF_AIG_TOKEN;
  if (!accountId || !gateway || !apiKey) throw Object.assign(new Error('gateway credentials'), { code: 'CLOUDFLARE_SETTINGS' });
  let metadata = settings.metadata;
  if (!metadata && settings.headers?.['cf-aig-metadata']) {
    try { metadata = JSON.parse(settings.headers['cf-aig-metadata']); } catch { /* optional metadata */ }
  }
  const gatewayOptions = { metadata, ...Object.fromEntries(['cacheTtl', 'cacheKey', 'skipCache', 'collectLog']
    .filter(key => settings[key] !== undefined).map(key => [key, settings[key]])) };
  // Use the binding path so custom fetch and request headers apply to every
  // passthrough. The project deliberately adds no duration-based aborts.
  const gatewayProvider = createAiGateway({ binding: { run(body, init) {
    const headers = new Headers(settings.headers);
    for (const [name, value] of parseAiGatewayOptions(gatewayOptions)) headers.set(name, value);
    headers.set('Content-Type', 'application/json');
    headers.set('cf-aig-authorization', `Bearer ${apiKey}`);
    return (settings.fetch ?? fetch)(`https://gateway.ai.cloudflare.com/v1/${accountId}/${gateway}`, {
      body: JSON.stringify(body), headers, method: 'POST', signal: init?.signal,
    });
  } } });
  if (request.model.startsWith('openai/')) {
    const { createOpenAI } = await load('ai-gateway-provider/providers/openai');
    return gatewayProvider(createOpenAI()(request.model.slice(7)));
  }
  if (request.model.startsWith('anthropic/')) {
    const { createAnthropic } = await load('ai-gateway-provider/providers/anthropic');
    return gatewayProvider(createAnthropic()(request.model.slice(10).replaceAll('.', '-')));
  }
  if (request.model.startsWith('workers-ai/') || request.model.startsWith('@cf/')) {
    const { createUnified } = await load('ai-gateway-provider/providers/unified');
    return gatewayProvider(createUnified({ apiKey })(request.model));
  }
  return createOpenAICompatible({ name: 'cloudflare-ai-gateway', apiKey,
    baseURL: `https://api.cloudflare.com/client/v4/accounts/${accountId}/ai/v1`,
    fetch: settings.fetch, headers: { ...settings.headers, 'cf-aig-gateway-id': gateway } })(request.model);
}

export async function resolveSdkModel(request, { load = (name) => import(name), env = process.env } = {}) {
  const { _openraid_sdk: metadata = {}, ...options } = request.options ?? {};
  const npm = metadata.npm;
  if (!npm || !/^(?:@[a-z0-9._-]+\/)?[a-z0-9._-]+(?:\/[a-z0-9._-]+)*$/i.test(npm)) {
    throw Object.assign(new Error('adapter'), { code: 'UNSUPPORTED_ADAPTER' });
  }
  const adapter = await load(npm);
  const factoryName = factories[npm] ?? Object.keys(adapter).find((key) => key.startsWith('create') && typeof adapter[key] === 'function');
  if (!factoryName || typeof adapter[factoryName] !== 'function') {
    throw Object.assign(new Error('factory'), { code: 'UNSUPPORTED_ADAPTER' });
  }
  const headers = new Headers();
  for (const source of [options.headers, metadata.settings?.headers, request.headers]) {
    for (const [name, value] of Object.entries(source ?? {})) headers.set(name, value);
  }
  const settings = { ...options, ...metadata.settings,
    headers: Object.fromEntries(headers.entries()) };
  if (npm === '@ai-sdk/openai-compatible') settings.name ??= metadata.provider;
  const bearer = headers.get('authorization')?.toLowerCase().startsWith('bearer ');
  if (settings.oauthAccessToken || settings.authType === 'oauth' || (npm === '@ai-sdk/azure' && bearer)) {
    for (const name of ['api-key', 'x-api-key', 'x-goog-api-key']) headers.delete(name);
    settings.headers = Object.fromEntries(headers.entries());
  }
  if (request.apiKey) settings.apiKey = request.apiKey;
  if (request.baseURL && !/[{}]/.test(request.baseURL)) settings.baseURL = request.baseURL;
  if (npm.startsWith('@ai-sdk/amazon-bedrock')) {
    settings.region ??= env.AWS_REGION ?? env.AWS_DEFAULT_REGION ?? 'us-east-1';
    settings.profile ??= env.AWS_PROFILE;
    settings.apiKey ??= env.AWS_BEARER_TOKEN_BEDROCK;
    if (settings.endpoint) settings.baseURL = settings.endpoint;
    if (!settings.apiKey && !settings.credentialProvider) {
      const { fromNodeProviderChain } = await load('@aws-sdk/credential-providers');
      settings.credentialProvider = fromNodeProviderChain(settings.profile ? { profile: settings.profile } : {});
    }
  }
  if (npm.startsWith('@ai-sdk/google-vertex')) {
    settings.project ??= env.GOOGLE_VERTEX_PROJECT ?? env.GOOGLE_CLOUD_PROJECT ?? env.GCP_PROJECT ?? env.GCLOUD_PROJECT;
    settings.location ??= env.GOOGLE_VERTEX_LOCATION ?? env.GOOGLE_CLOUD_LOCATION ?? env.VERTEX_LOCATION ?? (npm.endsWith('/anthropic') ? 'global' : 'us-central1');
    if (npm.endsWith('/anthropic') && ['eu', 'us'].includes(settings.location) && settings.project && !settings.baseURL) {
      settings.baseURL = `https://aiplatform.${settings.location}.rep.googleapis.com/v1/projects/${settings.project}/locations/${settings.location}/publishers/anthropic/models`;
    }
  }
  if (npm === '@ai-sdk/azure') {
    settings.resourceName ??= env.AZURE_RESOURCE_NAME;
    if (metadata.provider === 'azure-cognitive-services') {
      settings.resourceName ??= env.AZURE_COGNITIVE_SERVICES_RESOURCE_NAME;
      if (settings.resourceName && (!settings.baseURL || settings.baseURL.includes('.services.ai.azure.com'))) {
        settings.baseURL = `https://${settings.resourceName}.cognitiveservices.azure.com/openai${settings.useDeploymentBasedUrls ? '' : '/v1'}`;
      }
    }
    const authorization = headers.get('authorization');
    const access = settings.oauthAccessToken ?? (authorization?.toLowerCase().startsWith('bearer ') ? authorization.slice(7) : null);
    if (access) {
      delete settings.apiKey;
      settings.tokenProvider = async () => access;
    }
  }
  if (npm === 'gitlab-ai-provider') {
    if (settings.authType === 'oauth' && settings.authToken) settings.apiKey = settings.authToken;
    settings.instanceUrl = request.baseURL || settings.instanceUrl || env.GITLAB_INSTANCE_URL || 'https://gitlab.com';
    settings.featureFlags = { duo_agent_platform_agentic_chat: true, duo_agent_platform: true, ...settings.featureFlags };
    settings.aiGatewayHeaders = { 'User-Agent': `openraid gitlab-ai-provider/${adapter.VERSION ?? 'unknown'}`,
      'anthropic-beta': 'context-1m-2025-08-07', ...settings.aiGatewayHeaders };
  }
  if (npm === '@jerome-benoit/sap-ai-provider-v2') {
    if (request.apiKey) process.env.AICORE_SERVICE_KEY = request.apiKey;
    settings.deploymentId ??= env.AICORE_DEPLOYMENT_ID;
    settings.resourceGroup ??= env.AICORE_RESOURCE_GROUP;
  }
  let model;
  if (npm === 'ai-gateway-provider' || metadata.provider === 'cloudflare-ai-gateway') {
    model = await gatewayModel(settings, request, load, env);
  } else {
    const provider = await adapter[factoryName](settings);
    let modelId = request.model;
    if (npm === '@ai-sdk/amazon-bedrock') modelId = bedrockModelId(modelId, settings.region);
    if (npm.startsWith('@ai-sdk/google-vertex')) modelId = String(modelId).trim();
    if (npm === 'gitlab-ai-provider') {
      if (modelId.startsWith('duo-workflow-')) {
        const workflowId = adapter.isWorkflowModel?.(modelId) ? modelId : 'duo-workflow';
        model = provider.workflowChat(workflowId, { featureFlags: settings.featureFlags,
          workflowDefinition: typeof options.workflowDefinition === 'string' ? options.workflowDefinition : undefined });
        if (typeof options.workflowRef === 'string') model.selectedModelRef = options.workflowRef;
      } else {
        model = provider.agenticChat(modelId, { aiGatewayHeaders: settings.aiGatewayHeaders, featureFlags: settings.featureFlags });
      }
    } else if (npm === '@ai-sdk/azure') {
      const method = settings.useCompletionUrls && provider.chat ? 'chat'
        : ['responses', 'messages', 'chat', 'languageModel'].find(name => typeof provider[name] === 'function');
      model = provider[method](modelId);
    } else if (npm === '@ai-sdk/amazon-bedrock/mantle') {
      const safeguard = ['openai.gpt-oss-safeguard-20b', 'openai.gpt-oss-safeguard-120b'].includes(modelId);
      const method = safeguard && provider.chat ? 'chat' : !safeguard && provider.responses ? 'responses' : 'languageModel';
      model = provider[method](modelId);
    } else if (['openai', 'meta', 'xai'].includes(metadata.provider) && provider.responses) {
      model = provider.responses(modelId);
    } else {
      model = typeof provider.languageModel === 'function' ? provider.languageModel(modelId) : provider(modelId);
    }
  }
  // Salad's pinned adapter already emits the V3-compatible content/usage/finish
  // shape but advertises V4 (AI SDK 7). Keep AI SDK 6's resolver compatible with
  // this specific adapter rather than accepting unknown protocol versions.
  if (npm === '@saladtechnologies-oss/ai-sdk-provider' && model.specificationVersion === 'v4') {
    const native = model;
    model = { specificationVersion: 'v3', provider: native.provider, modelId: native.modelId,
      supportedUrls: native.supportedUrls, doGenerate: options => native.doGenerate(options),
      doStream: options => native.doStream(options) };
  }
  const gateway = npm === 'ai-gateway-provider' || metadata.provider === 'cloudflare-ai-gateway';
  const namespace = gateway ? (request.model.startsWith('openai/') ? 'openai'
    : request.model.startsWith('anthropic/') ? 'anthropic' : 'cloudflare-ai-gateway')
    : npm === '@ai-sdk/openai-compatible' ? settings.name
      : npm === '@jerome-benoit/sap-ai-provider-v2' && settings.name ? settings.name.split('.')[0]
        : namespaces[npm] ?? npm.split('/').at(-1);
  return { model, options, settings, namespace };
}

export async function discoverGitLabModels(request, { load = name => import(name), env = process.env } = {}) {
  const metadata = request.options?._openraid_sdk ?? {};
  const settings = metadata.settings ?? {};
  const token = settings.authType === 'oauth' ? settings.authToken : request.apiKey ?? settings.apiKey ?? env.GITLAB_TOKEN;
  if (!token) return [];
  const instanceUrl = request.baseURL || settings.instanceUrl || env.GITLAB_INSTANCE_URL || 'https://gitlab.com';
  const { discoverWorkflowModels } = await load('gitlab-ai-provider');
  const getHeaders = () => settings.authType === 'oauth' ? { Authorization: `Bearer ${token}` } : { 'PRIVATE-TOKEN': token };
  const result = await discoverWorkflowModels({ instanceUrl, getHeaders, fetch: settings.fetch },
    { workingDirectory: metadata.workspace ?? process.cwd(), cacheKey: token });
  return result.models.map(model => ({ id: model.id, name: `Agent Platform (${model.name})`,
    reasoning: true, tool_call: true, attachment: true, temperature: false,
    modalities: { input: ['text', 'image', 'pdf'], output: ['text'] },
    limit: { context: model.context, output: model.output },
    cost: { input: 0, output: 0, cache_read: 0, cache_write: 0 },
    provider: { api: instanceUrl, npm: 'gitlab-ai-provider' }, options: { workflowRef: model.ref }, variants: {} }));
}

export async function run(request, abortSignal) {
  abortSignal?.throwIfAborted();
  if (request.options?._openraid_sdk?.action === 'discover-models') {
    return { content: '', tool_calls: [], usage: { input_tokens: 0, output_tokens: 0, cached_tokens: 0 },
      finish_reason: 'stop', response_items: await discoverGitLabModels(request), native_content: null };
  }
  const { generateText, jsonSchema } = await import('ai');
  const { model, options, namespace } = await resolveSdkModel(request);
  abortSignal?.throwIfAborted();
  const tools = Object.fromEntries((request.tools ?? []).map((entry) => [entry.function.name,
    { description: entry.function.description, inputSchema: jsonSchema(entry.function.parameters) }]));
  const generation = Object.fromEntries(['temperature', 'topP', 'topK', 'presencePenalty', 'frequencyPenalty', 'stopSequences', 'seed']
    .filter((key) => options[key] !== undefined).map((key) => [key, options[key]]));
  const result = await generateText({ ...generation, model, messages: messagesForSdk(request.messages),
    maxOutputTokens: request.maxOutputTokens, tools: Object.keys(tools).length ? tools : undefined,
    providerOptions: { [namespace]: options }, maxRetries: 2, abortSignal });
  const usage = result.totalUsage ?? result.usage;
  const assistant = result.response.messages.find((message) => message.role === 'assistant');
  return { content: result.text ?? '', tool_calls: result.toolCalls.map((call) => ({ id: call.toolCallId,
    name: call.toolName, arguments: JSON.stringify(call.input) })),
    usage: { input_tokens: usage.inputTokens ?? 0, output_tokens: usage.outputTokens ?? 0,
      cached_tokens: usage.cachedInputTokens ?? usage.inputTokenDetails?.cacheReadTokens ?? 0 },
    finish_reason: result.finishReason,
    response_items: [], native_content: assistant ? { sdk: assistant.content } : null };
}

export function safeError(error) {
  const status = Number.isInteger(error?.statusCode) ? ` (HTTP ${error.statusCode})` : '';
  const message = error?.code === 'ERR_MODULE_NOT_FOUND' ? 'dependencies missing; run npm ci --prefix scripts'
    : error?.code === 'UNSUPPORTED_ADAPTER' ? 'adapter has no compatible factory; install/configure its AI SDK package'
    : error?.code === 'CLOUDFLARE_SETTINGS' ? 'set CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_GATEWAY_ID and CLOUDFLARE_API_TOKEN (or provider-options equivalents)'
    : error?.name === 'CredentialsProviderError' ? 'cloud credentials unavailable; configure the provider credential chain'
    : `provider request failed${status}; check provider settings and credentials`;
  const context = /context.{0,20}(length|window)|prompt.{0,20}(too long|tokens)|maximum.{0,20}tokens/i.test(String(error?.message ?? ''));
  return { message, retryable: error?.isRetryable === true || error?.statusCode === 429 || error?.statusCode >= 500,
    context_overflow: context };
}

async function serve() {
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  const pending = new Set();
  const controllers = new Map();
  for await (const line of lines) {
    if (!line.trim()) continue;
    let envelope;
    try { envelope = JSON.parse(line); } catch { process.exitCode = 1; break; }
    if (envelope.cancel !== undefined) {
      controllers.get(envelope.cancel)?.abort();
      continue;
    }
    const controller = new AbortController();
    controllers.set(envelope.id, controller);
    const task = (async () => {
      try { process.stdout.write(`${JSON.stringify({ id: envelope.id, completion: await run(envelope.request, controller.signal) })}\n`); }
      catch (error) { process.stdout.write(`${JSON.stringify({ id: envelope.id, error: safeError(error) })}\n`); }
      finally { controllers.delete(envelope.id); }
    })();
    pending.add(task);
    task.finally(() => pending.delete(task));
  }
  await Promise.all(pending);
}

async function main() {
  try {
    const chunks = [];
    for await (const chunk of process.stdin) chunks.push(chunk);
    const result = await run(JSON.parse(Buffer.concat(chunks).toString('utf8')));
    process.stdout.write(JSON.stringify(result));
  } catch (error) {
    // Error messages can contain raw upstream headers or request bodies. Emit
    // only a safe category/status, so credentials cannot reach the board.
    process.stderr.write(safeError(error).message);
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv.includes('--server')) await serve();
  else await main();
}
