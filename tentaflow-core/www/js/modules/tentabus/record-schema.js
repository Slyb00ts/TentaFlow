// ===== File: modules/tentabus/record-schema.js — the schema a record of a binary topic was encoded under, on demand =====
//
// A record of an Avro, Protobuf or Thrift topic carries `schemaRefId`: the id of
// the schema its bytes are written under. For a reader whose rule hides fields
// that is a SUB-SCHEMA, the pattern without the hidden fields (the server
// re-encodes the record to match it), so the bytes cannot be read with the
// pattern itself. "Pokaż wzór" asks the server for that schema
// (`SchemaForRecordGet`; the right to read the topic is the right to ask) and
// shows its text under the record. 0 means no schema: with an empty body the
// record was withheld, because it did not fit its pattern.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { T, fmtCount } from '/js/modules/tentabus/format.js';
import { schemaFormatLabel } from '/js/modules/tentabus/schemas.js';
import '/js/components/tf-button.js';
import '/js/components/tf-spinner.js';

/** Whether the record carries a schema id (`undefined` from a server that predates the field, 0 for none). */
export const hasSchemaRef = (record) => Number(record?.schemaRefId) > 0;

/**
 * The markup under a record: the schema line with its button, or the note for
 * a withheld record; empty for a server that sends no `schemaRefId`.
 * `empty` = the body has no bytes; `binary` = the topic is checked with a binary
 * pattern (an empty body of a JSON topic is just empty, not withheld).
 */
export function recordSchemaHtml(record, { empty = false, binary = false } = {}) {
  if (record?.schemaRefId === undefined || record?.schemaRefId === null) return '';
  if (hasSchemaRef(record)) {
    const id = Number(record.schemaRefId);
    return `
      <div class="tb-record-schema" data-role="record-schema" data-ref="${escapeAttr(String(id))}">
        <div class="tb-record-schema-head">
          <span>${escapeHtml(T('schemaref.line', { id: String(id) }))}</span>
          <tf-button variant="secondary" size="sm" icon="file-code" data-act="show-schema">${escapeHtml(T('schemaref.show'))}</tf-button>
        </div>
        <div class="muted">${escapeHtml(T(empty ? 'schemaref.no_fields' : 'schemaref.hint'))}</div>
        <div data-role="record-schema-body" aria-live="polite"></div>
      </div>`;
  }
  return empty && binary ? `<div class="muted tb-record-schema" data-role="record-schema-withheld">${escapeHtml(T('schemaref.withheld'))}</div>` : '';
}

/** The schema as the body of the block: what it is, and its text. */
export function schemaBodyHtml(schema) {
  const title = T(schema.derived ? 'schemaref.derived_title' : 'schemaref.full_title', {
    name: schema.subject, format: schemaFormatLabel(schema.schemaType), version: fmtCount(schema.version),
  });
  return `<div class="tb-record-schema-title">${escapeHtml(title)}</div><pre class="tb-payload" data-role="record-schema-text">${escapeHtml(schema.schemaText)}</pre>`;
}

/**
 * Makes the blocks under `root` work. `load(schemaRefId)` answers the schema
 * (`{ subject, schemaType, version, derived, schemaText }`) or rejects;
 * `describeError(err)` words a rejection. A second click folds the text away.
 */
export function wireRecordSchema(root, { load, describeError }) {
  root.addEventListener('click', async (e) => {
    const button = e.target.closest('[data-act="show-schema"]');
    const block = button?.closest('[data-role="record-schema"]');
    if (!block || !root.contains(block) || button.hasAttribute('disabled')) return;
    const body = block.querySelector('[data-role="record-schema-body"]');
    if (block.dataset.open === '1') {
      block.dataset.open = '';
      body.innerHTML = '';
      button.textContent = T('schemaref.show');
      return;
    }
    button.setAttribute('disabled', '');
    body.innerHTML = `<div class="tb-state"><tf-spinner size="sm"></tf-spinner>${escapeHtml(T('shell.loading'))}</div>`;
    try {
      const schema = await load(Number(block.dataset.ref));
      if (!block.isConnected) return;
      body.innerHTML = schemaBodyHtml(schema);
      block.dataset.open = '1';
      button.textContent = T('schemaref.hide');
    } catch (err) {
      if (!block.isConnected) return;
      const gone = /\bbus\.schema_not_found\b/.test(String(err?.message ?? ''));
      body.innerHTML = `<div class="tb-field-error" role="alert" data-role="record-schema-error">${escapeHtml(gone ? T('schemaref.gone') : describeError(err))}</div>`;
    } finally {
      button.removeAttribute('disabled');
    }
  });
}
