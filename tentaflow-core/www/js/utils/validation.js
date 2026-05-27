// =============================================================================
// Plik: utils/validation.js
// Opis: Walidacja pol formularza dla SDK Form (Input/Textarea/Select).
// Obsluguje built-in rules (required, min_length, max_length, pattern, range)
// oraz asynchroniczna regule Custom { action_id, debounce_ms } ktora wola
// addon przez addonUiActionRequest.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

// Mapa "key" -> { timer, generation }. Pozwala invalidate stare debounce calle
// gdy user napisze cos nowego - tylko ostatnie wywolanie wygra.
const DEBOUNCE_STATE = new Map();

// Czyta `kind` reguly - backend serializuje to jako lowercase string lub
// PascalCase w zaleznosci od adapter. Normalizujemy.
function ruleKind(rule) {
  if (!rule || typeof rule !== 'object') return '';
  const k = rule.kind || rule.type || '';
  return String(k).toLowerCase();
}

/**
 * Waliduje pojedyncze pole synchronously przez built-in rules; jesli wszystko
 * przeszlo i jest regula `custom`, wola addona asynchronicznie i czeka na
 * odpowiedz.
 *
 * Zwraca: { valid: bool, error: string | null, pending: false }
 */
export async function validateField(addonId, fieldId, value, validations) {
  if (!Array.isArray(validations) || validations.length === 0) {
    return { valid: true, error: null, pending: false };
  }

  for (const rule of validations) {
    const kind = ruleKind(rule);
    if (kind === 'required') {
      const empty =
        value == null ||
        value === '' ||
        (Array.isArray(value) && value.length === 0);
      if (empty) return { valid: false, error: 'To pole jest wymagane.', pending: false };
    } else if (kind === 'min_length' || kind === 'minlength') {
      const min = Number(rule.value ?? rule.min ?? 0);
      if (typeof value === 'string' && value.length < min) {
        return { valid: false, error: `Minimum ${min} znaków.`, pending: false };
      }
    } else if (kind === 'max_length' || kind === 'maxlength') {
      const max = Number(rule.value ?? rule.max ?? 0);
      if (typeof value === 'string' && value.length > max) {
        return { valid: false, error: `Maksimum ${max} znaków.`, pending: false };
      }
    } else if (kind === 'pattern') {
      if (typeof value === 'string' && value !== '') {
        const src = rule.regex ?? rule.pattern ?? '';
        try {
          if (!new RegExp(src).test(value)) {
            return { valid: false, error: rule.message || 'Nieprawidłowy format.', pending: false };
          }
        } catch {
          console.warn('[validation] invalid pattern:', src);
        }
      }
    } else if (kind === 'range') {
      const num = typeof value === 'number' ? value : Number(value);
      if (!Number.isNaN(num)) {
        const min = Number(rule.min);
        const max = Number(rule.max);
        if (num < min || num > max) {
          return { valid: false, error: `Wartość musi być w zakresie ${min}–${max}.`, pending: false };
        }
      }
    }
  }

  // Custom validation - wywolujemy addona dopiero gdy built-in rules przeszly.
  const customRule = validations.find((r) => ruleKind(r) === 'custom');
  if (customRule) {
    const actionId = customRule.action_id || customRule.actionId;
    if (!actionId) {
      return { valid: true, error: null, pending: false };
    }
    try {
      const result = await ApiBinary.one('addonUiActionRequest', {
        addonId,
        actionId,
        params: { field_id: fieldId, value },
      });
      if (result && result.valid === false) {
        return { valid: false, error: result.error || 'Nieprawidłowa wartość.', pending: false };
      }
    } catch (e) {
      console.error('[validation] custom rule failed:', e);
      return { valid: false, error: 'Błąd walidacji.', pending: false };
    }
  }

  return { valid: true, error: null, pending: false };
}

/**
 * Wersja debounced - uzywana gdy user pisze w input. Kazde wywolanie
 * anuluje poprzednie timery dla tego samego (addonId, fieldId). Resolve
 * z ostatnim wynikiem; wczesniejsze obietnice resolvuja jako { valid: true,
 * pending: true } sygnalizujac, ze trzeba czekac na ostateczna odpowiedz.
 */
export function debouncedValidate(addonId, fieldId, value, validations, debounceMs) {
  const key = `${addonId}:${fieldId}`;
  const delay = Number.isFinite(debounceMs) && debounceMs > 0 ? debounceMs : 300;

  const prev = DEBOUNCE_STATE.get(key);
  if (prev) {
    clearTimeout(prev.timer);
    if (prev.resolve) {
      // Stary call dostaje "pending" zeby caller wiedzial, ze przyszla nowsza prosba.
      prev.resolve({ valid: true, error: null, pending: true });
    }
  }

  return new Promise((resolve) => {
    const entry = { timer: null, resolve, generation: (prev?.generation ?? 0) + 1 };
    entry.timer = setTimeout(async () => {
      const result = await validateField(addonId, fieldId, value, validations);
      // Jesli inny call nadszedl w trakcie await - ten wynik jest stary, ignorujemy.
      const current = DEBOUNCE_STATE.get(key);
      if (current && current.generation !== entry.generation) {
        resolve({ valid: true, error: null, pending: true });
        return;
      }
      DEBOUNCE_STATE.delete(key);
      resolve(result);
    }, delay);
    DEBOUNCE_STATE.set(key, entry);
  });
}

/**
 * Zwraca true gdy lista validations ma regule async (custom).
 */
export function hasAsyncRule(validations) {
  return Array.isArray(validations) && validations.some((r) => ruleKind(r) === 'custom');
}

/**
 * Zwraca debounce_ms z reguly custom (lub default 300).
 */
export function customDebounceMs(validations) {
  if (!Array.isArray(validations)) return 300;
  const c = validations.find((r) => ruleKind(r) === 'custom');
  if (!c) return 300;
  const ms = Number(c.debounce_ms ?? c.debounceMs);
  return Number.isFinite(ms) && ms > 0 ? ms : 300;
}
