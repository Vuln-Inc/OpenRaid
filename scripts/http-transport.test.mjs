import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { test } from 'node:test';
import { getGlobalDispatcher, setGlobalDispatcher } from 'undici';
import { createNoDeadlineDispatcher, installNoDeadlineTransport } from './http-transport.mjs';

test('no-deadline dispatcher overrides constructor and fetch-level request deadlines', async () => {
  const dispatches = [];
  let construction;
  let closed = false;
  class PoolFixture {
    constructor(options) { construction = options; }
    dispatch(options, handler) { dispatches.push({ options, handler }); return true; }
    async close() { closed = true; }
    async destroy() {}
  }
  const dispatcher = createNoDeadlineDispatcher({ AgentClass: PoolFixture });
  const handler = {};
  const signal = new AbortController().signal;
  dispatcher.dispatch({ origin: 'https://fixture.invalid', headersTimeout: 300000,
    bodyTimeout: 300000, signal, method: 'POST' }, handler);
  dispatcher.dispatch({ origin: 'https://fixture.invalid', method: 'GET' }, handler);
  assert.deepEqual(construction, { headersTimeout: 0, bodyTimeout: 0, connect: { timeout: 0 } });
  for (const { options } of dispatches) {
    assert.equal(options.headersTimeout, 0);
    assert.equal(options.bodyTimeout, 0);
  }
  assert.equal(dispatches[0].options.signal, signal, 'explicit lifecycle cancellation is preserved');
  assert.equal(dispatches[0].handler, handler);
  await dispatcher.close();
  assert.equal(closed, true);
});

test('actual global fetch uses the shared no-deadline dispatcher for headers and streamed body', async () => {
  const previous = getGlobalDispatcher();
  const requests = [];
  const effective = [];
  const server = createServer((request, response) => {
    requests.push(request.url);
    response.writeHead(200, { 'Content-Type': 'text/plain; charset=utf-8' });
    response.write('native-');
    setImmediate(() => response.end('SDK-İ'));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const installed = installNoDeadlineTransport({ onDispatch: options => effective.push(options) });
  assert.equal(installNoDeadlineTransport(), installed, 'all bridge users reuse the same pool');
  const recorder = {
    dispatch(options, handler) {
      return installed.dispatch({ ...options, headersTimeout: 1, bodyTimeout: 1 }, handler);
    },
  };
  // Observe the final options inside the policy boundary, including options
  // supplied by built-in Node fetch rather than just our constructor settings.
  setGlobalDispatcher(recorder);
  try {
    for (const route of ['/generate', '/discover']) {
      const response = await fetch(`http://127.0.0.1:${server.address().port}${route}`);
      assert.equal(await response.text(), 'native-SDK-İ');
    }
    assert.deepEqual(requests, ['/generate', '/discover']);
    assert.equal(effective.length, 2);
    assert.ok(effective.every(options => options.headersTimeout === 0 && options.bodyTimeout === 0));
  } finally {
    setGlobalDispatcher(previous);
    await installed.close();
    await new Promise(resolve => server.close(resolve));
  }
});
