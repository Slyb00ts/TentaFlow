// =============================================================================
// Plik: sdk-runtime/data-avatar-lists-renderer.test.js
// Opis: Testy Avatar/AvatarGroup/BulletList/Timeline — chunk 3.3d-3.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  AVATAR_TAG, AVATAR_GROUP_TAG, BULLET_LIST_TAG, TIMELINE_TAG,
} from './data-avatar-lists-renderer.js';

const results = [];
function test(name, fn) {
  try { fn(); results.push({ name, ok: true }); }
  catch (err) { results.push({ name, ok: false, err }); }
}
function assertEq(a, e, m) {
  const aj = JSON.stringify(a, (_k, v) => typeof v === 'bigint' ? `${v}n` : v);
  const ej = JSON.stringify(e, (_k, v) => typeof v === 'bigint' ? `${v}n` : v);
  if (aj !== ej) throw new Error(`${m || 'assertEq'}: expected ${ej}, got ${aj}`);
}
function assert(cond, m) { if (!cond) throw new Error(m || 'assert failed'); }
function assertThrows(fn, m) {
  let t = false; try { fn(); } catch { t = true; }
  if (!t) throw new Error(m || 'expected throw');
}

const PATH = (...segs) => segs.map((s) =>
  typeof s === 'number' ? { kind: 'index', value: s } : { kind: 'key', value: s });

function makeStore() { return new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n }); }
function makeEngine(store) {
  return new ComponentRenderer({ store: store || makeStore(), eventDispatcher: { emit() {} }, locale: 'en-US' });
}
function comp(tag, fields, extra = {}) {
  return {
    tag, id: extra.id ?? 'c1', fields,
    handlers: extra.handlers ?? null,
    bind: extra.bind ?? null,
    a11y: extra.a11y ?? null,
    visibility: extra.visibility ?? null,
    test_id: extra.test_id ?? null,
  };
}
function setup() {
  _clearComponentRendererRegistry();
  bootstrapSdkRuntime();
  document.body.innerHTML = '';
}

function avatarComp(opts = {}, id = 'a1') {
  const source = opts.source || { kind: 'initials', initials: 'AB' };
  const fields = [
    [0, source],
    [1, opts.size || 'md'],
    [2, opts.shape || 'circle'],
  ];
  if (opts.status) fields.push([3, opts.status]);
  if (opts.tone) fields.push([4, opts.tone]);
  return comp(AVATAR_TAG, fields, { id });
}

// ============================================================================
// Avatar
// ============================================================================

test('Avatar initials renderuje <span> z size+shape class', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(avatarComp({ size: 'lg', shape: 'rounded' }));
  assertEq(el.tagName, 'SPAN');
  assert(el.classList.contains('tf-avatar--size-lg'));
  assert(el.classList.contains('tf-avatar--shape-rounded'));
  assertEq(el.querySelector('.tf-avatar-source__initials').textContent, 'AB');
});

test('Avatar initials bez tone dostaje deterministyczny kolor (B2)', () => {
  setup();
  const engine = makeEngine();
  const src = () => engine.render(avatarComp({ source: { kind: 'initials', initials: 'MW' } }))
    .querySelector('.tf-avatar-source');
  const first = src();
  const auto = [...first.classList].find((c) => c.startsWith('tf-avatar-source--auto-'));
  assert(auto, 'expected an auto-color class');
  // Deterministic: the same initials always map to the same bucket.
  assert(src().classList.contains(auto), 'same initials → same bucket');
  // Different initials can land in a different bucket (not asserting inequality,
  // just that the class is present and well-formed).
  const other = engine.render(avatarComp({ source: { kind: 'initials', initials: 'MK' } }))
    .querySelector('.tf-avatar-source');
  assert([...other.classList].some((c) => c.startsWith('tf-avatar-source--auto-')));
});

test('Avatar initials z tone NIE dostaje auto-koloru', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(avatarComp({ source: { kind: 'initials', initials: 'MW' }, tone: 'primary' }));
  const wrap = el.querySelector('.tf-avatar-source');
  assert(![...wrap.classList].some((c) => c.startsWith('tf-avatar-source--auto-')),
    'explicit tone must suppress the auto-color bucket');
});

test('Avatar image z https://... renderuje <img>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(avatarComp({
    source: { kind: 'image', ref: 'https://example.com/a.png' },
  }));
  const img = el.querySelector('img');
  assertEq(img.getAttribute('src'), 'https://example.com/a.png');
});

test('Avatar image z javascript: throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(avatarComp({
    source: { kind: 'image', ref: 'javascript:alert(1)' },
  })));
});

test('Avatar icon source renderuje SVG', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(avatarComp({
    source: { kind: 'icon', icon: { kind: 'named', name: 'user' } },
  }));
  assert(el.querySelector('.tf-avatar-source__icon') != null);
});

test('Avatar status=online renderuje dot indicator', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(avatarComp({ status: 'online' }));
  const dot = el.querySelector('.tf-avatar__status--online');
  assert(dot != null);
  assertEq(dot.getAttribute('aria-label'), 'online');
});

test('Avatar invalid size throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(avatarComp({ size: 'huge' })));
});

test('Avatar invalid shape throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(avatarComp({ shape: 'triangle' })));
});

test('Avatar unknown source kind throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(avatarComp({ source: { kind: 'video' } })));
});

test('Avatar unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AVATAR_TAG, [
    [0, { kind: 'initials', initials: 'A' }], [1, 'md'], [2, 'circle'], [99, 'x'],
  ])));
});

// ============================================================================
// AvatarGroup
// ============================================================================

test('AvatarGroup renderuje N avatarów + overflow +X', () => {
  setup();
  const engine = makeEngine();
  const avs = [
    avatarComp({}, 'a1'), avatarComp({}, 'a2'),
    avatarComp({}, 'a3'), avatarComp({}, 'a4'),
    avatarComp({}, 'a5'),
  ];
  const el = engine.render(comp(AVATAR_GROUP_TAG, [
    [0, avs], [1, 3], [2, 'tight'], [3, 'md'],
  ]));
  assertEq(el.querySelectorAll('.tf-avatar-group__item').length, 3);
  const more = el.querySelector('.tf-avatar-group__more');
  assertEq(more.textContent, '+2');
});

test('AvatarGroup ≤ max_visible bez overflow', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(AVATAR_GROUP_TAG, [
    [0, [avatarComp({}, 'a1'), avatarComp({}, 'a2')]],
    [1, 5], [2, 'default'], [3, 'md'],
  ]));
  assertEq(el.querySelector('.tf-avatar-group__more'), null);
});

test('AvatarGroup avatar z innym tagiem throws', () => {
  setup();
  const engine = makeEngine();
  const badChild = { tag: 0x0201, id: 'bad', fields: [], handlers: null, bind: null, a11y: null, visibility: null, test_id: null };
  assertThrows(() => engine.render(comp(AVATAR_GROUP_TAG, [
    [0, [badChild]], [1, 3], [2, 'default'], [3, 'md'],
  ])));
});

test('AvatarGroup malformed overflow entry (beyond max_visible) throws', () => {
  setup();
  const engine = makeEngine();
  // Wpisy poza max_visible też muszą być valid Avatar Components.
  const badOverflow = { tag: 0x020D };  // brak id, fields
  const avs = [avatarComp({}, 'a1'), badOverflow];
  assertThrows(() => engine.render(comp(AVATAR_GROUP_TAG, [
    [0, avs], [1, 1], [2, 'default'], [3, 'md'],
  ])));
});

test('Timeline ts_ms fractional number throws (must be i64 integer)', () => {
  setup();
  const engine = makeEngine();
  const items = [timelineItem('t1', 1717000000000.5, 'event')];
  assertThrows(() => engine.render(comp(TIMELINE_TAG, [
    [0, items], [1, 'vertical'], [2, 'default'], [3, false], [4, false],
  ])));
});

test('AvatarGroup max_visible=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AVATAR_GROUP_TAG, [
    [0, []], [1, 0], [2, 'default'], [3, 'md'],
  ])));
});

test('AvatarGroup invalid overlap throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AVATAR_GROUP_TAG, [
    [0, []], [1, 3], [2, 'mega'], [3, 'md'],
  ])));
});

// ============================================================================
// BulletList
// ============================================================================

test('BulletList variant=bullet renderuje <ul>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BULLET_LIST_TAG, [
    [0, [{ kind: 'literal', value: 'a' }, { kind: 'literal', value: 'b' }]],
    [1, 'bullet'], [3, 'default'],
  ]));
  assertEq(el.tagName, 'UL');
  assertEq(el.querySelectorAll('li').length, 2);
});

test('BulletList variant=numbered renderuje <ol>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BULLET_LIST_TAG, [
    [0, [{ kind: 'literal', value: 'a' }]],
    [1, 'numbered'], [3, 'default'],
  ]));
  assertEq(el.tagName, 'OL');
});

test('BulletList variant=check renderuje ✓ markery', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BULLET_LIST_TAG, [
    [0, [{ kind: 'literal', value: 'done' }]],
    [1, 'check'], [3, 'default'],
  ]));
  const mark = el.querySelector('.tf-bullet-list__check');
  assertEq(mark.textContent, '✓');
});

test('BulletList density i tone klasy', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BULLET_LIST_TAG, [
    [0, []], [1, 'bullet'], [2, 'muted'], [3, 'compact'],
  ]));
  assert(el.classList.contains('tf-bullet-list--density-compact'));
  assert(el.classList.contains('tf-bullet-list--tone-muted'));
});

test('BulletList unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BULLET_LIST_TAG, [
    [0, []], [1, 'bullet'], [3, 'default'], [99, 'x'],
  ])));
});

test('BulletList invalid variant throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BULLET_LIST_TAG, [
    [0, []], [1, 'fancy'], [3, 'default'],
  ])));
});

// ============================================================================
// Timeline
// ============================================================================

function timelineItem(id, ts_ms, title, opts = {}) {
  const f = [
    [0, id], [1, ts_ms],
    [2, { kind: 'literal', value: title }],
  ];
  if (opts.description) f.push([3, opts.description]);
  if (opts.icon) f.push([4, opts.icon]);
  if (opts.tone) f.push([5, opts.tone]);
  if (opts.action_id) f.push([6, opts.action_id]);
  return f;
}

test('Timeline renderuje <ol> z N items', () => {
  setup();
  const engine = makeEngine();
  const items = [
    timelineItem('t1', 1717000000000, 'A'),
    timelineItem('t2', 1717100000000, 'B'),
  ];
  const el = engine.render(comp(TIMELINE_TAG, [
    [0, items], [1, 'vertical'], [2, 'default'], [3, false], [4, false],
  ]));
  assertEq(el.tagName, 'OL');
  assertEq(el.querySelectorAll('.tf-timeline__item').length, 2);
});

test('Timeline item with tone class', () => {
  setup();
  const engine = makeEngine();
  const items = [timelineItem('t1', 1717000000000, 'OK', { tone: 'success' })];
  const el = engine.render(comp(TIMELINE_TAG, [
    [0, items], [1, 'vertical'], [2, 'default'], [3, false], [4, false],
  ]));
  assert(el.querySelector('.tf-timeline__item--tone-success') != null);
});

test('Timeline show_dates renderuje <time> z ISO datetime', () => {
  setup();
  const engine = makeEngine();
  const items = [timelineItem('t1', 1717000000000, 'event')];
  const el = engine.render(comp(TIMELINE_TAG, [
    [0, items], [1, 'vertical'], [2, 'default'], [3, true], [4, false],
  ]));
  const time = el.querySelector('time');
  assertEq(time.getAttribute('datetime'), new Date(1717000000000).toISOString());
});

test('Timeline group_by_day renderuje day-header', () => {
  setup();
  const engine = makeEngine();
  const items = [
    timelineItem('t1', Date.UTC(2024, 5, 1, 10), 'morning'),
    timelineItem('t2', Date.UTC(2024, 5, 2, 14), 'next day'),
  ];
  const el = engine.render(comp(TIMELINE_TAG, [
    [0, items], [1, 'vertical'], [2, 'default'], [3, false], [4, true],
  ]));
  assertEq(el.querySelectorAll('.tf-timeline__day-header').length, 2);
});

test('Timeline item z action_id click emituje item_click', () => {
  setup();
  const engine = makeEngine();
  const items = [timelineItem('t1', 1717000000000, 'go', { action_id: 'open_detail' })];
  const el = engine.render(comp(TIMELINE_TAG, [
    [0, items], [1, 'vertical'], [2, 'default'], [3, false], [4, false],
  ]));
  let got = null;
  el.addEventListener('item_click', (e) => { got = e.detail; });
  el.querySelector('.tf-timeline__item--clickable').click();
  assertEq(got, { item_id: 't1', action_id: 'open_detail', ts_ms: 1717000000000 });
});

test('Timeline duplicate item id throws', () => {
  setup();
  const engine = makeEngine();
  const items = [
    timelineItem('dup', 1, 'a'), timelineItem('dup', 2, 'b'),
  ];
  assertThrows(() => engine.render(comp(TIMELINE_TAG, [
    [0, items], [1, 'vertical'], [2, 'default'], [3, false], [4, false],
  ])));
});

test('Timeline invalid item id grammar throws', () => {
  setup();
  const engine = makeEngine();
  const items = [timelineItem('Bad ID!', 1, 'a')];
  assertThrows(() => engine.render(comp(TIMELINE_TAG, [
    [0, items], [1, 'vertical'], [2, 'default'], [3, false], [4, false],
  ])));
});

test('Timeline ts_ms out of TimeClip throws', () => {
  setup();
  const engine = makeEngine();
  const items = [timelineItem('t1', 9e15, 'far future')];
  assertThrows(() => engine.render(comp(TIMELINE_TAG, [
    [0, items], [1, 'vertical'], [2, 'default'], [3, false], [4, false],
  ])));
});

test('Timeline invalid orientation throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TIMELINE_TAG, [
    [0, []], [1, 'diagonal'], [2, 'default'], [3, false], [4, false],
  ])));
});

test('Timeline unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TIMELINE_TAG, [
    [0, []], [1, 'vertical'], [2, 'default'], [3, false], [4, false], [99, 'x'],
  ])));
});

// ---- report ----
function reportResults() {
  let pass = 0, fail = 0;
  const lines = [];
  for (const r of results) {
    if (r.ok) { pass++; lines.push(`✓ ${r.name}`); }
    else { fail++; lines.push(`✗ ${r.name}\n    ${r.err && r.err.stack ? r.err.stack : r.err}`); }
  }
  lines.push('');
  lines.push(`${pass}/${pass + fail} tests passed${fail ? ` — ${fail} FAILED` : ''}`);
  return { pass, fail, text: lines.join('\n') };
}
if (typeof process !== 'undefined') {
  const r = reportResults();
  console.log(r.text);
  if (r.fail > 0) process.exit(1);
}
export { reportResults };
