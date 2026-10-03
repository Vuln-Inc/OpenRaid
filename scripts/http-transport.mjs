// All SDK/discovery fetches share one pool without operation deadlines. Idle
// keep-alive eviction remains independent of a request's lifetime.
import { Agent, setGlobalDispatcher } from 'undici';

let installed;

export function createNoDeadlineDispatcher({ AgentClass = Agent, onDispatch } = {}) {
  const pool = new AgentClass({
    headersTimeout: 0,
    bodyTimeout: 0,
    connect: { timeout: 0 },
  });
  return {
    dispatch(options, handler) {
      // Some Node versions provide fetch-level defaults that would otherwise
      // override Agent settings. Enforce the policy at the dispatch boundary.
      const effective = { ...options, headersTimeout: 0, bodyTimeout: 0 };
      onDispatch?.(effective);
      return pool.dispatch(effective, handler);
    },
    close: (...args) => pool.close(...args),
    destroy: (...args) => pool.destroy(...args),
  };
}

export function installNoDeadlineTransport({ onDispatch } = {}) {
  if (!installed) {
    installed = createNoDeadlineDispatcher({ onDispatch });
    setGlobalDispatcher(installed);
  }
  return installed;
}
