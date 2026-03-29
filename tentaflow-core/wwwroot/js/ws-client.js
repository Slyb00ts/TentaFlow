// =============================================================================
// Plik: ws-client.js
// Opis: WebSocket client do /ws/metrics z automatycznym reconnectem
//       i wzorcem event emitter.
// Przyklad: WsClient.on('metrics', data => console.log(data));
// =============================================================================

const WsClient = (() => {
  'use strict';

  let ws = null;
  let reconnectTimer = null;
  let isConnected = false;
  let reconnectAttempts = 0;
  const listeners = {};
  const RECONNECT_DELAY_BASE = 2000;
  const RECONNECT_DELAY_MAX = 30000;
  const MAX_RECONNECT_ATTEMPTS = 999; // Nieskonczone reconnecty — nigdy nie rezygnuj

  // Rejestracja listenera
  function on(event, callback) {
    if (!listeners[event]) {
      listeners[event] = [];
    }
    listeners[event].push(callback);
  }

  // Wyrejestrowanie listenera
  function off(event, callback) {
    if (!listeners[event]) return;
    listeners[event] = listeners[event].filter(cb => cb !== callback);
  }

  // Emitowanie zdarzenia do listenerow
  function emit(event, data) {
    if (!listeners[event]) return;
    listeners[event].forEach(cb => {
      try {
        cb(data);
      } catch (err) {
        console.error('Blad w listenerze WS:', err);
      }
    });
  }

  // Budowanie URL WebSocket
  function buildUrl() {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const host = window.location.host;
    return `${protocol}//${host}/ws/metrics`;
  }

  // Polaczenie WebSocket z tokenem w Sec-WebSocket-Protocol
  function connect() {
    if (ws && (ws.readyState === WebSocket.CONNECTING || ws.readyState === WebSocket.OPEN)) {
      return;
    }

    clearTimeout(reconnectTimer);

    try {
      const token = ApiClient.getToken();
      const protocols = token ? [`bearer.${token}`] : [];
      ws = new WebSocket(buildUrl(), protocols);
    } catch (err) {
      console.error('Blad tworzenia WebSocket:', err);
      scheduleReconnect();
      return;
    }

    ws.onopen = () => {
      isConnected = true;
      reconnectAttempts = 0;
      emit('status', 'connected');
    };

    ws.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data);
        emit('metrics', data);
      } catch {
        // Ignoruj nieprawidlowe wiadomosci
      }
    };

    ws.onclose = () => {
      isConnected = false;
      emit('status', 'disconnected');
      scheduleReconnect();
    };

    ws.onerror = () => {
      // onclose zostanie wywolane po onerror
    };
  }

  // Rozlaczenie
  function disconnect() {
    clearTimeout(reconnectTimer);
    if (ws) {
      ws.onclose = null;
      ws.close();
      ws = null;
    }
    isConnected = false;
    emit('status', 'disconnected');
  }

  // Zaplanowanie reconnecta z exponential backoff
  function scheduleReconnect() {
    clearTimeout(reconnectTimer);
    if (reconnectAttempts >= MAX_RECONNECT_ATTEMPTS) {
      console.warn('WS: osiagnieto limit prob reconnecta (' + MAX_RECONNECT_ATTEMPTS + ')');
      return;
    }
    const delay = Math.min(RECONNECT_DELAY_BASE * Math.pow(2, reconnectAttempts), RECONNECT_DELAY_MAX);
    reconnectAttempts++;
    reconnectTimer = setTimeout(() => {
      if (!isConnected) {
        connect();
      }
    }, delay);
  }

  // Sprawdzenie statusu
  function connected() {
    return isConnected;
  }

  return {
    connect,
    disconnect,
    connected,
    on,
    off,
  };
})();
