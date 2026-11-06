// ============ File: protocol/follow-client.js — runs a callback for the current socket and again for every socket that replaces it ============

import { ApiBinary } from './api-binary-shim.js';

// A login closes the anonymous socket and opens one with the session, and the
// server pushes personal events only to the second, so a listener added once at
// start-up would stay on a socket that is gone. Listeners survive a reconnect
// of the same client, so the callback runs once per client object.
export function followClient(onClient) {
  let attached = null;
  const attach = () => {
    ApiBinary.client()
      .then((client) => {
        if (client === attached) return;
        attached = client;
        onClient(client);
      })
      .catch((error) => console.warn('[follow-client] client not ready:', error?.message));
  };
  attach();
  ApiBinary.onLifecycle((event) => {
    if (event.type === 'open') attach();
  });
}
