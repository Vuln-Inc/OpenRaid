import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { bedrockModelId, messagesForSdk, run, safeError } from './sdk-bridge.mjs';

test('SDK error classification never returns request secrets', () => {
  const error = Object.assign(new Error('Bearer private-key maximum context length exceeded'), { statusCode: 400 });
  const safe = safeError(error);
  assert.equal(safe.context_overflow, true);
  assert.equal(safe.retryable, false);
  assert.ok(!safe.message.includes('private-key'));
  assert.equal(safeError({ statusCode: 503 }).retryable, true);
});

test('Bedrock region mapping preserves explicit inference profiles and ARNs', () => {
  assert.equal(bedrockModelId('anthropic.claude-sonnet-4-5', 'us-east-1'), 'us.anthropic.claude-sonnet-4-5');
  assert.equal(bedrockModelId('anthropic.claude-sonnet-4-5', 'ap-southeast-2'), 'au.anthropic.claude-sonnet-4-5');
  assert.equal(bedrockModelId('us.anthropic.claude-sonnet-4-5', 'eu-west-1'), 'us.anthropic.claude-sonnet-4-5');
  assert.equal(bedrockModelId('arn:aws:bedrock:profile', 'us-east-1'), 'arn:aws:bedrock:profile');
});

test('bundled catalog SDK packages are installed with compatible factories', async () => {
  const catalog = JSON.parse(await readFile(new URL('../data/models.json', import.meta.url), 'utf8'));
  const packages = new Set();
  for (const provider of Object.values(catalog)) {
    if (provider.npm) packages.add(provider.npm);
    for (const model of Object.values(provider.models ?? {})) if (model.provider?.npm) packages.add(model.provider.npm);
  }
  // Copilot uses its own native OAuth transport, rather than a public package.
  packages.delete('@ai-sdk/github-copilot');
  const missing = [];
  for (const name of packages) {
    try {
      const adapter = await import(name);
      if (!Object.keys(adapter).some((key) => key.startsWith('create') && typeof adapter[key] === 'function')) missing.push(`${name}: missing factory`);
    } catch (error) { missing.push(`${name}: ${error.code}`); }
  }
  assert.deepEqual(missing, []);
});

test('signed reasoning and tool identities survive SDK history conversion', () => {
  const parts = [{ type: 'reasoning', text: 'thought', providerOptions: { anthropic: { signature: 'signed' } } },
    { type: 'tool-call', toolCallId: 'call-1', toolName: 'read_file', input: { path: 'README.md' } }];
  const messages = messagesForSdk([{ role: 'assistant', content: '', _openraid_native: { sdk: parts },
    tool_calls: [{ id: 'call-1', function: { name: 'read_file', arguments: '{"path":"README.md"}' } }] },
    { role: 'tool', tool_call_id: 'call-1', content: 'file contents' }]);
  assert.deepEqual(messages[0].content, parts);
  assert.equal(messages[1].content[0].toolName, 'read_file');
  assert.deepEqual(messages[1].content[0].output, { type: 'text', value: 'file contents' });
});

test('actual SDK adapter returns native tool calls and token usage without executing tools', async () => {
  let received;
  const server = createServer(async (req, res) => {
    let body = '';
    for await (const chunk of req) body += chunk;
    received = JSON.parse(body);
    assert.equal(req.headers.authorization, 'Bearer mock-key');
    assert.equal(req.headers['x-priority'], 'explicit');
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ id: 'chat-1', object: 'chat.completion', created: 1, model: 'mock',
      choices: [{ index: 0, finish_reason: 'tool_calls', message: { role: 'assistant', content: null,
        tool_calls: [{ id: 'call-1', type: 'function', function: { name: 'read_file', arguments: '{"path":"README.md"}' } }] } }],
      usage: { prompt_tokens: 12, completion_tokens: 7, total_tokens: 19 } }));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  try {
    const completion = await run({ model: 'mock', baseURL: `http://127.0.0.1:${server.address().port}/v1`,
      apiKey: 'mock-key', maxOutputTokens: 100, headers: { 'X-Priority': 'explicit' },
      options: { temperature: 0.2, _openraid_sdk: { npm: '@ai-sdk/openai-compatible', provider: 'mock', settings: { headers: { 'x-priority': 'factory' } } } },
      messages: [{ role: 'user', content: 'Read a file' }],
      tools: [{ type: 'function', function: { name: 'read_file', description: 'read',
        parameters: { type: 'object', properties: { path: { type: 'string' } }, required: ['path'] } } }] });
    assert.equal(received.model, 'mock');
    assert.equal(received.temperature, 0.2);
    assert.equal(received.tools[0].function.name, 'read_file');
    assert.deepEqual(completion.tool_calls, [{ id: 'call-1', name: 'read_file', arguments: '{"path":"README.md"}' }]);
    assert.equal(completion.usage.input_tokens, 12);
    assert.equal(completion.usage.output_tokens, 7);
    assert.equal(completion.native_content.sdk[0].type, 'tool-call');
  } finally {
    await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
});

test('one shared SDK sidecar multiplexes simultaneous requests by id', async () => {
  const server = createServer(async (req, res) => {
    let body = '';
    for await (const chunk of req) body += chunk;
    const request = JSON.parse(body);
    await new Promise((resolve) => setTimeout(resolve, request.model === 'slow' ? 60 : 1));
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ id: 'chat-1', object: 'chat.completion', created: 1, model: request.model,
      choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: request.model } }],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } }));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const child = spawn(process.execPath, [fileURLToPath(new URL('sdk-bridge.mjs', import.meta.url)), '--server'], { stdio: ['pipe', 'pipe', 'pipe'] });
  const exit = new Promise((resolve, reject) => { child.once('exit', resolve); child.once('error', reject); });
  try {
    const request = { baseURL: `http://127.0.0.1:${server.address().port}/v1`, apiKey: 'mock-key',
      maxOutputTokens: 100, headers: {}, options: { _openraid_sdk: { npm: '@ai-sdk/openai-compatible', provider: 'mock' } },
      messages: [{ role: 'user', content: 'answer' }], tools: [] };
    child.stdin.end(`${JSON.stringify({ id: 1, request: { ...request, model: 'slow' } })}\n${JSON.stringify({ id: 2, request: { ...request, model: 'fast' } })}\n`);
    const results = [];
    for await (const line of createInterface({ input: child.stdout })) results.push(JSON.parse(line));
    assert.equal(await exit, 0);
    assert.deepEqual(results.map((result) => result.id), [2, 1]);
    assert.deepEqual(results.map((result) => result.completion.content), ['fast', 'slow']);
  } finally {
    child.kill();
    await new Promise((resolve) => server.close(resolve));
  }
});

test('SDK sidecar cancellation aborts a stalled request without stopping another agent', { timeout: 15_000 }, async (t) => {
  const admitted = Promise.withResolvers();
  const siblingAdmitted = Promise.withResolvers();
  const cancelled = Promise.withResolvers();
  const healthy = Promise.withResolvers();
  const server = createServer(async (req, res) => {
    let body = '';
    for await (const chunk of req) body += chunk;
    const request = JSON.parse(body);
    if (request.model === 'stalled') {
      res.once('close', () => cancelled.resolve());
      admitted.resolve();
      return; // Only caller cancellation can end this admitted request.
    }
    siblingAdmitted.resolve();
    await cancelled.promise;
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ id: 'chat-healthy', object: 'chat.completion', created: 1, model: request.model,
      choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: 'still running' } }],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } }));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const child = spawn(process.execPath, [fileURLToPath(new URL('sdk-bridge.mjs', import.meta.url)), '--server'], { stdio: ['pipe', 'pipe', 'pipe'] });
  const exited = new Promise((resolve) => child.once('exit', resolve));
  child.once('error', (error) => { admitted.reject(error); siblingAdmitted.reject(error); healthy.reject(error); });
  const lines = createInterface({ input: child.stdout });
  lines.on('line', (line) => {
    const result = JSON.parse(line);
    if (result.id === 2) healthy.resolve(result);
  });
  // Teardown is registered before awaits so a failed cancellation cannot leak
  // the bridge process or the deliberately stalled HTTP connection.
  t.after(async () => {
    lines.close();
    child.kill();
    await exited;
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  });
  const request = { baseURL: `http://127.0.0.1:${server.address().port}/v1`, apiKey: 'mock-key',
    maxOutputTokens: 100, headers: {}, options: { _openraid_sdk: { npm: '@ai-sdk/openai-compatible', provider: 'mock' } },
    messages: [{ role: 'user', content: 'answer' }], tools: [] };
  child.stdin.write(`${JSON.stringify({ id: 1, request: { ...request, model: 'stalled' } })}\n${JSON.stringify({ id: 2, request: { ...request, model: 'healthy' } })}\n`);
  await Promise.all([admitted.promise, siblingAdmitted.promise]);
  child.stdin.write(`${JSON.stringify({ cancel: 1 })}\n`);
  await cancelled.promise;
  const result = await healthy.promise;
  assert.equal(result.error, undefined);
  assert.equal(result.completion.content, 'still running');
  assert.equal(child.exitCode, null, 'other agents keep the shared sidecar alive');
});

test('Azure OAuth uses bearer tokenProvider and resolved endpoint instead of stale factory URL', async () => {
  let received;
  const server = createServer(async (req, res) => {
    assert.ok(req.url.startsWith('/resolved/responses'));
    assert.equal(req.headers.authorization, 'Bearer azure-access');
    assert.equal(req.headers['api-key'], undefined);
    assert.equal(req.headers['x-api-key'], undefined);
    assert.equal(req.headers['x-goog-api-key'], undefined);
    let body = '';
    for await (const chunk of req) body += chunk;
    received = JSON.parse(body);
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ id: 'resp-azure', object: 'response', created_at: 1, status: 'completed', model: 'mock',
      output: [{ type: 'message', id: 'msg-azure', role: 'assistant', status: 'completed',
        content: [{ type: 'output_text', text: 'Azure bearer ready', annotations: [] }] }],
      usage: { input_tokens: 3, output_tokens: 2, total_tokens: 5, input_tokens_details: { cached_tokens: 0 }, output_tokens_details: { reasoning_tokens: 0 } } }));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  try {
    const completion = await run({ model: 'gpt-5-mini', baseURL: `http://127.0.0.1:${server.address().port}/resolved`,
      apiKey: 'must-not-be-api-key', headers: { Authorization: 'Bearer azure-access', 'x-api-key': 'stale-request-key', 'x-goog-api-key': 'stale-google-key' }, maxOutputTokens: 100,
      options: { reasoningEffort: 'low', _openraid_sdk: { npm: '@ai-sdk/azure', provider: 'azure', settings: { baseURL: 'http://127.0.0.1:1/stale', headers: { 'api-key': 'stale-factory-key' } } } },
      messages: [{ role: 'user', content: 'test bearer' }], tools: [] });
    assert.equal(completion.content, 'Azure bearer ready');
    assert.equal(received.reasoning.effort, 'low');
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test('GitLab OAuth metadata reaches real agentic SDK as bearer instead of stale API credential', async () => {
  let authenticated = false;
  const server = createServer(async (req, res) => {
    let body = '';
    for await (const chunk of req) body += chunk;
    res.writeHead(200, { 'Content-Type': 'application/json' });
    if (req.url.endsWith('/direct_access')) {
      assert.equal(req.headers.authorization, 'Bearer gitlab-oauth-access');
      authenticated = true;
      res.end(JSON.stringify({ token: 'gateway-access', headers: {} }));
    } else if (req.url.endsWith('/responses')) {
      res.end(JSON.stringify({ id: 'resp-gitlab', object: 'response', created_at: 1, status: 'completed', model: 'gpt-5.4',
        output: [{ type: 'message', id: 'msg-gitlab', role: 'assistant', status: 'completed', content: [{ type: 'output_text', text: 'GitLab OAuth ready', annotations: [] }] }],
        usage: { input_tokens: 3, output_tokens: 2, total_tokens: 5 } }));
    } else {
      assert.ok(req.url.endsWith('/chat/completions'));
      res.end(JSON.stringify({ id: 'chat-gitlab', object: 'chat.completion', created: 1, model: 'gpt-5.4',
        choices: [{ index: 0, finish_reason: 'stop', message: { role: 'assistant', content: 'GitLab OAuth ready' } }],
        usage: { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 } }));
    }
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const baseURL = `http://127.0.0.1:${server.address().port}`;
  try {
    const completion = await run({ model: 'duo-chat-gpt-5-4', baseURL, apiKey: 'stale-api-key', headers: {}, maxOutputTokens: 100,
      options: { _openraid_sdk: { npm: 'gitlab-ai-provider', provider: 'gitlab',
        settings: { authType: 'oauth', authToken: 'gitlab-oauth-access', instanceUrl: 'http://127.0.0.1:1/stale', aiGatewayUrl: baseURL } } },
      messages: [{ role: 'user', content: 'test account' }], tools: [] });
    assert.ok(authenticated);
    assert.equal(completion.content, 'GitLab OAuth ready');
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});
