// =============================================================================
// Plik: api-client.js
// Opis: Fetch wrapper z obsluga JWT tokenow, auto-refresh, metody CRUD.
// Przyklad: await apiClient.get('/api/services');
// =============================================================================

const ApiClient = (() => {
  'use strict';

  const TOKEN_KEY = 'tentaflow_jwt';
  const BASE_URL = '';

  // Pobranie tokenu z localStorage
  function getToken() {
    return localStorage.getItem(TOKEN_KEY);
  }

  // Zapis tokenu do localStorage
  function setToken(token) {
    localStorage.setItem(TOKEN_KEY, token);
  }

  // Usuniecie tokenu
  function removeToken() {
    localStorage.removeItem(TOKEN_KEY);
  }

  // Sprawdzenie czy token istnieje
  function hasToken() {
    return !!getToken();
  }

  // Dekodowanie payload JWT (bez weryfikacji - to robi serwer)
  function decodeToken(token) {
    try {
      const parts = token.split('.');
      if (parts.length !== 3) return null;
      const payload = JSON.parse(atob(parts[1]));
      return payload;
    } catch {
      return null;
    }
  }

  // Sprawdzenie czy token wygasl
  function isTokenExpired() {
    const token = getToken();
    if (!token) return true;
    const payload = decodeToken(token);
    if (!payload || !payload.exp) return true;
    return Date.now() >= payload.exp * 1000;
  }

  // Pobranie nazwy uzytkownika z tokenu
  function getUsername() {
    const token = getToken();
    if (!token) return null;
    const payload = decodeToken(token);
    return payload ? payload.sub || payload.username || 'admin' : null;
  }

  // Logowanie - wysyla credentials, zapisuje JWT
  async function login(username, password) {
    const response = await fetch(`${BASE_URL}/api/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password }),
    });

    if (!response.ok) {
      const error = await response.json().catch(() => ({}));
      throw new Error(error.error || error.message || 'Nieprawidlowe dane logowania');
    }

    const data = await response.json();
    setToken(data.token);
    return data;
  }

  // Wylogowanie - usun token
  function logout() {
    removeToken();
  }

  // Naglowki autoryzacji
  function authHeaders() {
    const token = getToken();
    const headers = { 'Content-Type': 'application/json' };
    if (token) {
      headers['Authorization'] = `Bearer ${token}`;
    }
    return headers;
  }

  // Obsluga odpowiedzi - parsowanie JSON, obsluga bledow
  async function handleResponse(response) {
    if (response.status === 401) {
      removeToken();
      window.dispatchEvent(new CustomEvent('auth:expired'));
      throw new Error('Sesja wygasla - zaloguj sie ponownie');
    }

    if (!response.ok) {
      const error = await response.json().catch(() => ({}));
      throw new Error(error.error || error.message || `Blad HTTP: ${response.status}`);
    }

    // Pusta odpowiedz (204 No Content)
    if (response.status === 204) return null;

    return response.json();
  }

  // GET
  async function get(url, options = {}) {
    const response = await fetch(`${BASE_URL}${url}`, {
      method: 'GET',
      headers: authHeaders(),
      ...options,
    });
    return handleResponse(response);
  }

  // POST
  async function post(url, data, options = {}) {
    const response = await fetch(`${BASE_URL}${url}`, {
      method: 'POST',
      headers: authHeaders(),
      body: JSON.stringify(data),
      ...options,
    });
    return handleResponse(response);
  }

  // PUT
  async function put(url, data, options = {}) {
    const response = await fetch(`${BASE_URL}${url}`, {
      method: 'PUT',
      headers: authHeaders(),
      body: JSON.stringify(data),
      ...options,
    });
    return handleResponse(response);
  }

  // DELETE
  async function del(url, options = {}) {
    const response = await fetch(`${BASE_URL}${url}`, {
      method: 'DELETE',
      headers: authHeaders(),
      ...options,
    });
    return handleResponse(response);
  }

  return {
    getToken,
    setToken,
    removeToken,
    hasToken,
    isTokenExpired,
    getUsername,
    login,
    logout,
    get,
    post,
    put,
    delete: del,
  };
})();
