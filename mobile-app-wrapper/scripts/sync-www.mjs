// =============================================================================
// File: scripts/sync-www.mjs
// Description: Kopiuje www/ z tentaflow-core do capacitor www/ przed syncem.
//              Capacitor wymaga webDir — my mamy primary w tentaflow-core/www,
//              wiec to nasz source of truth.
// =============================================================================

import { cp, rm, mkdir } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(__dirname, '../../tentaflow-core/www');
const DST = resolve(__dirname, '../www');

console.log(`[sync-www] ${SRC} -> ${DST}`);
await rm(DST, { recursive: true, force: true });
await mkdir(DST, { recursive: true });
await cp(SRC, DST, { recursive: true });

// Patch index.html — gdy app jest natywny (Capacitor), konfiguracja serwera
// ma byc ustawiona recznie przez user-a (adres daemona TentaFlow na LANie
// albo relay URL). Dodajemy link do ustawien.
// Na teraz trzymamy index.html jak jest — user po instalacji appki dostaje
// ekran login ktory pyta o host (domyslnie localhost — trzeba zmienic).
console.log('[sync-www] done');
