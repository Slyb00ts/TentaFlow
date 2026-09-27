// ===== File: modules/tentabus/message-preview.js — "Podgląd wiadomości" (T02c/T03d): one window for every preview button =====
//
// The window reads one partition at a time, from a chosen message number,
// oldest to newest, and pages on with "Wczytaj więcej". It opens on the
// busiest partition at its newest page, so the reader first sees what is
// being written now. Every read is written to the audit log by the server
// and shows what the data-hiding rules let this reader see — the banner says
// both. Nothing here consumes: the topic's consumers keep their own place.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { T, fmtCount, fmtBytes, fmtWhen } from '/js/modules/tentabus/format.js';
import { bytesToPreviewText, headerText, parseBlobRefJson } from '/js/modules/tentabus/payload.js';
import '/js/components/tf-window.js';
import '/js/components/tf-button.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-table.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-spinner.js';

const sprite = (id) => `<svg class="icon" aria-hidden="true"><use href="#i-${id}"/></svg>`;

export const PREVIEW_PAGE = 50;
const PAYLOAD_PREVIEW_BYTES = 4096;

const highWatermark = (p) => Number(p?.highWatermark ?? p?.logEndOffset) || 0;
const earliest = (p) => Number(p?.earliestOffset) || 0;

/** The partition to open on: the one holding the most messages, the lowest number on a tie; 0 when unknown. */
export function pickPartition(partitions) {
  let best = null;
  for (const p of partitions || []) {
    const held = highWatermark(p) - earliest(p);
    if (!best || held > best.held || (held === best.held && p.partition < best.partition)) best = { partition: Number(p.partition), held };
  }
  return best ? best.partition : 0;
}

/** Where the first page of a partition starts: its newest page, never before the oldest kept message. */
export function previewStart(partition, pageSize = PREVIEW_PAGE) {
  if (!partition) return null;
  return Math.max(earliest(partition), highWatermark(partition) - pageSize);
}

/** The browse request for one partition, from `fromOffset` (the oldest kept message when `null`). */
export function buildBrowseRequest(instanceId, topic, partition, fromOffset, limit = PREVIEW_PAGE) {
  const req = { instanceId, topic, partition: Number(partition), limit };
  if (fromOffset != null) req.fromOffsets = [{ partition: Number(partition), offset: Number(fromOffset) }];
  return req;
}

/** "od 30 433 233 do 72 405 459", or that the partition keeps nothing. */
export function rangeText(partition) {
  if (!partition) return '—';
  const from = earliest(partition);
  const next = highWatermark(partition);
  return next > from
    ? T('topics.preview.range', { from: fmtCount(from), to: fmtCount(next - 1) })
    : T('topics.preview.range_empty');
}

/** The line under the list: its order, and whether more follows or which number comes next. */
export function pageNote(info) {
  if (!info) return T('topics.preview.order');
  if (info.hasMore) return `${T('topics.preview.order')} ${T('topics.preview.more_after', { number: fmtCount(info.nextOffset) })}`;
  return `${T('topics.preview.order')} ${T('topics.preview.next_number', { number: fmtCount(highWatermark(info)) })}`;
}

/**
 * Opens the window. `browse(request)` sends one `MessagesBrowseRequest`;
 * `loadPartitions()` resolves the topic's partitions with their oldest kept
 * message and next number (may reject: the window then starts at the oldest
 * message of partition 0); `describeError(err)` words a failure.
 */
export function openMessagePreview({ instanceId, topic, partitionCount, browse, loadPartitions, describeError = (e) => String(e?.message || e) }) {
  const state = {
    partition: 0,
    from: null,
    known: new Map(),
    records: [],
    info: null,
    selected: -1,
    loading: true,
    error: '',
    generation: 0,
    // Set once the reader picks a partition or a start number: the topic's
    // partition list arriving later must not move them elsewhere.
    chosen: false,
  };
  const count = Math.max(1, Number(partitionCount) || 1);

  const win = document.createElement('tf-window');
  win.className = 'tb-window tb-preview-window';
  win.setAttribute('title', T('topics.preview.title', { topic }));
  win.setAttribute('icon', 'eye');
  win.setAttribute('buttons', 'close');
  win.setAttribute('modal', '');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '860');
  win.setAttribute('min-width', '360');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="tb-preview">
      <div class="tb-audit-banner">${sprite('shield')}<span><b>${escapeHtml(T('topics.preview.audit_title'))}</b> ${escapeHtml(T('topics.preview.audit_text'))}</span></div>
      <div class="tb-preview-controls">
        <tf-select data-role="partition" label="${escapeAttr(T('topics.preview.partition_label'))}"></tf-select>
        <tf-input data-role="from" type="number" inputmode="numeric" min="0" label="${escapeAttr(T('topics.preview.from_label'))}" placeholder="${escapeAttr(T('topics.preview.from_placeholder'))}"></tf-input>
        <div class="tb-preview-range"><div class="k">${escapeHtml(T('topics.preview.range_label'))}</div><div class="v" data-role="range">—</div></div>
      </div>
      <div data-role="list"></div>
      <div data-role="detail"></div>
    </div>
    <div slot="footer"><tf-button variant="secondary" data-action="close-preview">${escapeHtml(T('topics.preview.close'))}</tf-button></div>`;

  const partitionSelect = win.querySelector('[data-role="partition"]');
  const fromInput = win.querySelector('[data-role="from"]');

  const listHost = win.querySelector('[data-role="list"]');
  listHost.innerHTML = `
    <div class="tb-preview-list">
      <tf-table data-role="table" variant="flush">
        <tf-column key="partition" label="${escapeAttr(T('topics.preview.col_partition'))}" priority="low"></tf-column>
        <tf-column key="offset" label="${escapeAttr(T('topics.preview.col_number'))}" renderer="num"></tf-column>
        <tf-column key="when" label="${escapeAttr(T('topics.preview.col_when'))}"></tf-column>
        <tf-column key="key" label="${escapeAttr(T('topics.preview.col_key'))}" renderer="html" fill></tf-column>
      </tf-table>
    </div>
    <div class="tb-state" data-role="state"></div>
    <div class="tb-preview-more">
      <div class="muted" data-role="note"></div>
      <tf-button variant="secondary" size="sm" data-role="more" hidden>${escapeHtml(T('topics.preview.load_more'))}</tf-button>
    </div>`;
  const table = listHost.querySelector('[data-role="table"]');
  table.addEventListener('row-click', (e) => { state.selected = e.detail.index ?? state.records.findIndex((r) => r.offset === e.detail.row._offset); paint(); });

  const paintDetail = () => {
    const host = win.querySelector('[data-role="detail"]');
    const r = state.records[state.selected];
    if (!r) { host.innerHTML = ''; return; }
    const blob = r.isBlobRef ? parseBlobRefJson(r.payloadPreview) : null;
    const origin = headerText(r.headers, 'tf.origin');
    const newest = state.selected === state.records.length - 1 && !state.info?.hasMore;
    const title = T(`topics.preview.${newest ? 'record_title_newest' : 'record_title'}`, { number: fmtCount(r.offset), partition: fmtCount(r.partition), when: fmtWhen(r.timestampMs) });
    const chips = [
      r.truncated ? `<tf-chip size="sm" variant="outline" status="warn" label="${escapeAttr(T('topics.preview.truncated'))}"></tf-chip>` : '',
      origin ? `<tf-chip size="sm" variant="outline" status="neutral" label="${escapeAttr(T('topics.preview.origin', { origin }))}"></tf-chip>` : '',
    ].join('');
    host.innerHTML = `
      <div class="tb-preview-record">
        <div class="tb-preview-record-head"><span>${escapeHtml(title)}</span>${chips}</div>
        ${blob
          ? `<div class="tb-kv-grid"><div class="k">${escapeHtml(T('topics.preview.blob_title'))}</div><div class="v">${escapeHtml(T('topics.preview.blob_text', { size: fmtBytes(blob.size_bytes), mime: blob.mime }))}</div></div>`
          : `<pre class="tb-payload">${escapeHtml(bytesToPreviewText(r.payloadPreview, PAYLOAD_PREVIEW_BYTES)) || escapeHtml(T('topics.preview.payload_empty'))}</pre>`}
      </div>`;
  };

  const paint = () => {
    const known = state.known.get(state.partition) || state.info;
    win.querySelector('[data-role="range"]').textContent = rangeText(known);
    const stateEl = listHost.querySelector('[data-role="state"]');
    const more = listHost.querySelector('[data-role="more"]');
    const note = listHost.querySelector('[data-role="note"]');
    if (state.loading && !state.records.length) {
      stateEl.innerHTML = `<tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}`;
    } else if (state.error) {
      stateEl.innerHTML = `${sprite('alert')}<span>${escapeHtml(state.error)}</span>`;
    } else if (!state.records.length) {
      stateEl.textContent = state.from != null ? T('topics.preview.empty_from', { number: fmtCount(state.from) }) : T('topics.preview.empty');
    } else {
      stateEl.textContent = '';
    }
    stateEl.hidden = !stateEl.textContent && !stateEl.children.length;
    table.hidden = state.records.length === 0;
    table.rows = state.records.map((r, i) => ({
      partition: T('topics.preview.partition_n', { n: r.partition }),
      offset: fmtCount(r.offset),
      when: fmtWhen(r.timestampMs),
      key: r.key?.length ? `<span class="tf-table__cell--mono"><span class="tf-table__cell-title">${escapeHtml(bytesToPreviewText(r.key, 64))}</span></span>` : '—',
      _offset: r.offset,
      _class: i === state.selected ? 'selected' : '',
    }));
    note.textContent = state.records.length ? pageNote(state.info) : '';
    more.hidden = !(state.info?.hasMore) || state.loading;
    paintDetail();
  };

  const load = async (append) => {
    const gen = ++state.generation;
    state.loading = true;
    state.error = '';
    if (!append) { state.records = []; state.info = null; state.selected = -1; }
    paint();
    const fromOffset = append ? state.info?.nextOffset : state.from;
    try {
      const resp = await browse(buildBrowseRequest(instanceId, topic, state.partition, fromOffset));
      if (gen !== state.generation) return;
      const records = (resp?.records || []).filter((r) => Number(r.partition) === state.partition);
      // A reader still on the newest message follows it to the newest loaded one.
      const onNewest = state.selected === state.records.length - 1;
      state.records = append ? [...state.records, ...records] : records;
      state.info = (resp?.partitions || []).find((p) => Number(p.partition) === state.partition) || null;
      if (state.info) state.known.set(state.partition, state.info);
      // The newest message of the page is the one worth reading first.
      if (!append || onNewest) state.selected = state.records.length - 1;
    } catch (err) {
      if (gen !== state.generation) return;
      state.error = describeError(err);
    }
    state.loading = false;
    paint();
    // A new page opens at its newest message, the one selected for reading.
    if (!append) {
      const list = listHost.querySelector('.tb-preview-list');
      requestAnimationFrame(() => { list.scrollTop = list.scrollHeight; });
    }
  };

  const partitionOptions = () => Array.from({ length: count }, (_, i) => ({ value: String(i), label: T('topics.preview.partition_n', { n: i }) }));

  partitionSelect.setOptions(partitionOptions(), '0');
  partitionSelect.addEventListener('change', (e) => {
    state.chosen = true;
    state.partition = Number(e.detail.value) || 0;
    state.from = previewStart(state.known.get(state.partition));
    fromInput.value = state.from == null ? '' : String(state.from);
    load(false);
  });
  const applyFrom = () => {
    const raw = String(fromInput.value || '').trim();
    const n = raw === '' ? null : Math.max(0, Math.trunc(Number(raw)));
    if (raw !== '' && !Number.isFinite(n)) return;
    if (n === state.from) return;
    state.chosen = true;
    state.from = n;
    load(false);
  };
  fromInput.addEventListener('change', applyFrom);
  fromInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') applyFrom(); });
  listHost.querySelector('[data-role="more"]').addEventListener('click', () => load(true));
  win.addEventListener('action', (e) => { if (e.detail?.action === 'close-preview') win.close(true); });

  document.body.appendChild(win);
  paint();
  Promise.resolve()
    .then(() => loadPartitions?.())
    .then((parts) => {
      for (const p of parts || []) state.known.set(Number(p.partition), p);
      if (!state.chosen) state.partition = pickPartition(parts);
    }, () => {})
    .then(() => {
      // The reader's own choice already loaded its page; only the range
      // line learns what the list said about that partition.
      if (state.chosen) { paint(); return undefined; }
      partitionSelect.value = String(state.partition);
      state.from = previewStart(state.known.get(state.partition));
      fromInput.value = state.from == null ? '' : String(state.from);
      return load(false);
    });
  return win;
}
