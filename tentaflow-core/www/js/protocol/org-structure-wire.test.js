// =============================================================================
// File: protocol/org-structure-wire.test.js
// Description: The organizational-structure wire through the REAL glue
//   (www/js/protocol/wasm_glue*):
//   - every `codec.encode.org*Request` builds the exact `OrgStructureBody`
//     CBOR the server decodes: snake_case fields, an absent optional as null,
//     dates as strings, `clear` lists, `confirm_backdated`;
//   - the answers (structure snapshot, chain, manager, write result with a
//     warning, typed error, recompute) decode to the objects the screen reads,
//     including an answer without the `#[serde(default)]` fields.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';

const artifact = new URL('./wasm_glue_bg.wasm', import.meta.url);
const skip = existsSync(artifact) ? false : 'wasm_glue_bg.wasm is absent — build tentaflow-core once to generate the glue';
const wasm = skip ? null : await import('./wasm_glue.js');
let codec;
if (!skip) {
  await wasm.default({ module_or_path: readFileSync(artifact) });
  codec = await import('./codec.js');
  await codec.codecReady;
}

// ----- a minimal CBOR writer and reader: what serde + ciborium exchange -------

function head(major, value) {
  const n = BigInt(value);
  const base = major << 5;
  if (n < 24n) return [base | Number(n)];
  if (n < 0x100n) return [base | 24, Number(n)];
  if (n < 0x10000n) return [base | 25, Number(n >> 8n), Number(n & 0xffn)];
  const width = n < 0x100000000n ? 4 : 8;
  const out = [base | (width === 4 ? 26 : 27)];
  for (let i = width - 1; i >= 0; i -= 1) out.push(Number((n >> BigInt(8 * i)) & 0xffn));
  return out;
}

/** A number the server writes as a CBOR float even when it is whole (`f64` fields). */
class F64 {
  constructor(value) { this.value = value; }
}

function cbor(value) {
  if (value instanceof F64) {
    const view = new DataView(new ArrayBuffer(8));
    view.setFloat64(0, value.value);
    return [0xfb, ...new Uint8Array(view.buffer)];
  }
  if (value === null || value === undefined) return [0xf6];
  if (value === true) return [0xf5];
  if (value === false) return [0xf4];
  if (value instanceof Uint8Array) return [...head(2, value.length), ...value];
  if (Array.isArray(value)) return value.reduce((acc, v) => acc.concat(cbor(v)), head(4, value.length));
  if (typeof value === 'number' && !Number.isInteger(value)) {
    const view = new DataView(new ArrayBuffer(8));
    view.setFloat64(0, value);
    return [0xfb, ...new Uint8Array(view.buffer)];
  }
  if (typeof value === 'number') return head(0, value);
  if (typeof value === 'string') {
    const bytes = new TextEncoder().encode(value);
    return [...head(3, bytes.length), ...bytes];
  }
  const entries = Object.entries(value);
  return entries.reduce((acc, [k, v]) => acc.concat(cbor(k), cbor(v)), head(5, entries.length));
}

/** Reads the definite-length subset serde + ciborium write. */
function readCbor(bytes) {
  let pos = 0;
  const arg = (info) => {
    if (info < 24) return info;
    const width = { 24: 1, 25: 2, 26: 4, 27: 8 }[info];
    let n = 0n;
    for (let i = 0; i < width; i += 1) n = (n << 8n) | BigInt(bytes[pos + i]);
    pos += width;
    return n <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(n) : n;
  };
  const item = () => {
    const first = bytes[pos];
    pos += 1;
    const major = first >> 5;
    const info = first & 0x1f;
    if (major === 7 && info >= 25) {
      const width = { 25: 2, 26: 4, 27: 8 }[info];
      const view = new DataView(bytes.buffer, bytes.byteOffset + pos, width);
      pos += width;
      if (width === 8) return view.getFloat64(0);
      if (width === 4) return view.getFloat32(0);
      const half = view.getUint16(0);
      const exp = (half >> 10) & 0x1f;
      const frac = half & 0x3ff;
      const magnitude = exp === 0 ? frac * 2 ** -24 : (1 + frac / 1024) * 2 ** (exp - 15);
      return half & 0x8000 ? -magnitude : magnitude;
    }
    if (major === 7) return { 20: false, 21: true, 22: null }[info];
    const n = arg(info);
    if (major === 0) return n;
    if (major === 1) return -1 - n;
    if (major === 2 || major === 3) {
      const slice = bytes.slice(pos, pos + n);
      pos += n;
      return major === 2 ? slice : new TextDecoder().decode(slice);
    }
    if (major === 4) return Array.from({ length: n }, item);
    const out = {};
    for (let i = 0; i < n; i += 1) {
      const key = item();
      out[key] = item();
    }
    return out;
  };
  return item();
}

const decodeBody = (body) => wasm.decodeMessageBody(new Uint8Array(cbor(body)));

/** The `BusEnvelope` a request builder puts on the wire, as plain CBOR data. */
function sentEnvelope(kind, payload) {
  const envelope = wasm.decodeEnvelope(codec.encode[kind](7, payload, 1));
  try {
    return readCbor(envelope.body).BusBody;
  } finally {
    envelope.free();
  }
}


/** The `OrgStructurePayload` a request builder puts on the wire, as plain CBOR data. */
function sent(kind, payload) {
  const envelope = wasm.decodeEnvelope(codec.encode[kind](7, payload, 1));
  try {
    return readCbor(envelope.body).OrgStructureBody;
  } finally {
    envelope.free();
  }
}

const DAY = '2026-10-01';

// ----- requests -------------------------------------------------------------

test('the read requests carry their target, direction and optional day', { skip }, () => {
  assert.deepEqual(sent('orgStructureRequest', {}), { StructureRequest: { at: null } });
  assert.deepEqual(sent('orgStructureRequest', { at: DAY }), { StructureRequest: { at: DAY } });
  assert.deepEqual(
    sent('orgReportsChainRequest', { target: { kind: 'user', id: 'u-1' }, direction: 'up' }),
    { ReportsChainRequest: { target: { kind: 'user', id: 'u-1' }, direction: 'up', seat_scope: 'primary', at: null } },
  );
  assert.deepEqual(
    sent('orgSubordinatesRequest', { target: { kind: 'position', id: 'p-1' }, transitive: true, seatScope: 'all', at: DAY }),
    { SubordinatesRequest: { target: { kind: 'position', id: 'p-1' }, transitive: true, seat_scope: 'all', at: DAY } },
  );
  assert.deepEqual(sent('orgManagerRequest', { userId: 'u-1' }), { ManagerRequest: { user_id: 'u-1', at: null } });
  assert.deepEqual(sent('orgAssignmentRequest', { user_id: 'u-1', at: DAY }), { AssignmentRequest: { user_id: 'u-1', at: DAY } });
  assert.deepEqual(sent('orgIntegrityReportRequest', {}), { IntegrityReportRequest: { at: null } });
  assert.deepEqual(sent('orgRecomputeRequest', {}), { RecomputeRequest: {} });
});

test('a unit is created, patched, moved and ended with dates and the backdating confirmation', { skip }, () => {
  assert.deepEqual(sent('orgUnitCreateRequest', { name: 'IT', parentUnitId: 'u-0', validFrom: DAY }), {
    UnitCreateRequest: {
      name: 'IT', code: null, type_id: null, parent_unit_id: 'u-0', color: null,
      valid_from: DAY, valid_to: null, confirm_backdated: false,
    },
  });
  assert.deepEqual(
    sent('orgUnitUpdateRequest', { unitId: 'u-1', name: 'IT 2', clear: ['code'], from: DAY, confirmBackdated: true }),
    {
      UnitUpdateRequest: {
        unit_id: 'u-1', name: 'IT 2', code: null, type_id: null, color: null,
        clear: ['code'], from: DAY, confirm_backdated: true,
      },
    },
  );
  assert.deepEqual(sent('orgUnitMoveRequest', { unitId: 'u-1', from: DAY }), {
    UnitMoveRequest: { unit_id: 'u-1', new_parent_unit_id: null, from: DAY, confirm_backdated: false },
  });
  assert.deepEqual(sent('orgUnitEndRequest', { unitId: 'u-1', from: DAY }), {
    UnitEndRequest: { unit_id: 'u-1', from: DAY, confirm_backdated: false },
  });
  assert.deepEqual(sent('orgHeadSetRequest', { unitId: 'u-1', headPositionId: 'p-1', from: DAY }), {
    HeadSetRequest: { unit_id: 'u-1', head_position_id: 'p-1', from: DAY, confirm_backdated: false },
  });
  assert.deepEqual(sent('orgDeputyHeadsSetRequest', { unitId: 'u-1', positionIds: ['p-2', 'p-3'], from: DAY }), {
    DeputyHeadsSetRequest: { unit_id: 'u-1', position_ids: ['p-2', 'p-3'], from: DAY, confirm_backdated: false },
  });
});

test('unit types are created, patched with a clear list and deleted', { skip }, () => {
  assert.deepEqual(sent('orgUnitTypeCreateRequest', { name: 'Dział', icon: 'building' }), {
    UnitTypeCreateRequest: { name: 'Dział', color: null, icon: 'building' },
  });
  assert.deepEqual(sent('orgUnitTypeUpdateRequest', { id: 't-1', color: '#fff', clear: ['icon'] }), {
    UnitTypeUpdateRequest: { id: 't-1', name: null, color: '#fff', icon: null, clear: ['icon'] },
  });
  assert.deepEqual(sent('orgUnitTypeDeleteRequest', { id: 't-1' }), { UnitTypeDeleteRequest: { id: 't-1' } });
});

test('positions carry the tri-state manager flag, their line and their clears', { skip }, () => {
  assert.deepEqual(
    sent('orgPositionCreateRequest', { unitId: 'u-1', name: 'Analityk', isManager: false, isStaff: true, parentPositionId: 'p-1', validFrom: DAY }),
    {
      PositionCreateRequest: {
        unit_id: 'u-1', name: 'Analityk', code: null, role_id: null, is_manager: false, is_staff: true,
        parent_position_id: 'p-1', valid_from: DAY, valid_to: null, confirm_backdated: false,
      },
    },
  );
  const patch = sent('orgPositionUpdateRequest', { positionId: 'p-1', code: 'IT-2', clear: ['role_id', 'is_manager'], from: DAY });
  assert.deepEqual(patch.PositionUpdateRequest.clear, ['role_id', 'is_manager']);
  assert.equal(patch.PositionUpdateRequest.code, 'IT-2', 'the key the file import matches on');
  assert.equal(patch.PositionUpdateRequest.is_manager, null, 'an unset flag is not false');
  assert.equal(patch.PositionUpdateRequest.is_staff, null);
  assert.deepEqual(sent('orgPositionMoveRequest', { positionId: 'p-1', newParentPositionId: 'p-0', from: DAY }), {
    PositionMoveRequest: { position_id: 'p-1', new_parent_position_id: 'p-0', from: DAY, confirm_backdated: false },
  });
  assert.deepEqual(sent('orgPositionEndRequest', { positionId: 'p-1', from: DAY, confirmBackdated: true }), {
    PositionEndRequest: { position_id: 'p-1', from: DAY, confirm_backdated: true },
  });
  assert.deepEqual(
    sent('orgReportingLineSetRequest', { positionId: 'p-1', parentPositionId: 'p-0', kind: 'functional', priority: 2, validFrom: DAY }),
    {
      ReportingLineSetRequest: {
        position_id: 'p-1', parent_position_id: 'p-0', kind: 'functional', priority: 2,
        valid_from: DAY, valid_to: null, confirm_backdated: false,
      },
    },
  );
});

test('assignments name the person by kind and id and keep the share', { skip }, () => {
  assert.deepEqual(
    sent('orgAssignRequest', { positionId: 'p-1', subject: { kind: 'external', id: 'x-1' }, assignmentType: 'contractor', share: 0.4, validFrom: DAY }),
    {
      AssignRequest: {
        position_id: 'p-1', subject: { kind: 'external', id: 'x-1' }, assignment_type: 'contractor',
        share: 0.4, is_primary: null, valid_from: DAY, valid_to: null, confirm_backdated: false,
      },
    },
  );
  assert.deepEqual(sent('orgAssignmentUpdateRequest', { assignmentId: 'a-1', share: 1, isPrimary: true, from: DAY }), {
    AssignmentUpdateRequest: {
      assignment_id: 'a-1', assignment_type: null, share: 1, is_primary: true, from: DAY, confirm_backdated: false,
    },
  });
  assert.deepEqual(sent('orgAssignmentEndRequest', { assignmentId: 'a-1', from: DAY }), {
    AssignmentEndRequest: { assignment_id: 'a-1', from: DAY, confirm_backdated: false },
  });
  assert.deepEqual(sent('orgExternalPersonCreateRequest', { displayName: 'Jan Kowalski' }), {
    ExternalPersonCreateRequest: { display_name: 'Jan Kowalski', email: null, note: null },
  });
  assert.deepEqual(sent('orgTimezoneSetRequest', { timezone: 'Europe/Berlin' }), {
    TimezoneSetRequest: { timezone: 'Europe/Berlin' },
  });
});

test('a request the server would refuse is not encoded', { skip }, () => {
  assert.throws(() => codec.encode.orgAssignRequest(1, { positionId: 'p-1', share: 1, validFrom: DAY }), /Assign/);
});

// ----- responses ------------------------------------------------------------

const decode = (payload) => decodeBody({ OrgStructureBody: payload });

test('the structure answer decodes units, positions, holders, vacancies and warnings', { skip }, () => {
  const body = decode({
    StructureResponse: {
      view: {
        at: DAY, timezone: 'Europe/Warsaw',
        units: [{
          id: 'row-1', unit_id: 'u-1', name: 'IT', code: 'IT', type_id: null, parent_unit_id: null, color: null,
          head_position_id: 'p-1', deputy_head_position_ids: ['p-2'], valid_from: '2026-01-01', valid_to: null,
        }],
        positions: [{
          id: 'prow-1', position_id: 'p-1', unit_id: 'u-1', name: 'Kierownik', role_id: null, is_manager: true,
          is_staff: false, valid_from: '2026-01-01', valid_to: null, primary_parent_position_id: null,
          functional_parent_position_ids: [], is_head: true, is_vacant: false,
        }],
        assignments: [{
          id: 'a-1', position_id: 'p-1', subject: { kind: 'user', id: 'u-anna' }, assignment_type: 'permanent',
          share: new F64(1), is_primary: true, valid_from: '2026-01-01', valid_to: null, display_name: 'Anna Nowak',
        }],
        vacancies: ['p-9'],
        warnings: [{ kind: 'share_overbooked', subject: { kind: 'user', id: 'u-anna' }, from: DAY, total: new F64(1.5) }],
      },
      unit_types: [{ id: 't-1', name: 'Dział', color: null, icon: null }],
      my_permissions: ['org.admin'],
    },
  });
  assert.equal(body.variant, 'OrgStructureStructureResponse');
  assert.equal(body.view.units[0].deputyHeadPositionIds[0], 'p-2');
  assert.equal(body.view.units[0].unit_id, 'u-1');
  assert.equal(body.view.positions[0].isHead, true);
  assert.equal(body.view.assignments[0].subject.id, 'u-anna');
  assert.equal(body.view.assignments[0].displayName, 'Anna Nowak');
  assert.equal(body.view.warnings[0].kind, 'share_overbooked');
  assert.equal(body.view.warnings[0].total, 1.5);
  assert.deepEqual(body.view.vacancies, ['p-9']);
  assert.deepEqual(body.myPermissions, ['org.admin']);
  assert.equal(body.unitTypes[0].name, 'Dział');
});

test('a structure answer without the defaulted fields decodes as empty', { skip }, () => {
  const body = decode({ StructureResponse: { view: { at: DAY, timezone: 'Europe/Warsaw' } } });
  assert.deepEqual(body.view.units, []);
  assert.deepEqual(body.view.warnings, []);
  assert.deepEqual(body.unitTypes, []);
  assert.deepEqual(body.myPermissions, []);
});

test('chain, subordinates, manager, assignment and integrity answers decode', { skip }, () => {
  const link = { position_id: 'p-0', unit_id: 'u-0', name: 'Dyrektor', depth: 1, holders: [{ kind: 'external', id: 'x-1' }] };
  const chain = decode({ ReportsChainResponse: { links: [link] } });
  assert.equal(chain.variant, 'OrgStructureReportsChainResponse');
  assert.equal(chain.links[0].positionId, 'p-0');
  assert.equal(chain.links[0].holders[0].kind, 'external');
  assert.equal(decode({ SubordinatesResponse: { links: [] } }).variant, 'OrgStructureSubordinatesResponse');

  const manager = decode({ ManagerResponse: { manager: { user_id: 'u-9', position_id: 'p-0' } } });
  assert.equal(manager.manager.userId, 'u-9');
  assert.equal(decode({ ManagerResponse: { manager: null } }).manager, null);

  const held = decode({ AssignmentResponse: { primary: null, others: [] } });
  assert.equal(held.primary, null);
  assert.deepEqual(held.others, []);

  const report = decode({ IntegrityReportResponse: { violations: [{ kind: 'unit_cycle', unit_id: 'u-1', from: DAY }] } });
  assert.equal(report.violations[0].kind, 'unit_cycle');
  assert.equal(report.violations[0].unitId, 'u-1');
});

test('a write answer carries the result, the warnings and a typed error', { skip }, () => {
  const ok = decode({
    WriteResponse: {
      ok: true,
      warnings: [{ kind: 'unit_without_head', unit_id: 'u-1', from: DAY }],
      result: { kind: 'ended', value: { reporting_lines: ['l-1'], deputy_heads: [], assignments: ['a-1'] } },
    },
  });
  assert.equal(ok.variant, 'OrgStructureWriteResponse');
  assert.equal(ok.ok, true);
  assert.equal(ok.error, null);
  assert.equal(ok.warnings[0].unitId, 'u-1');
  assert.equal(ok.result.kind, 'ended');
  assert.deepEqual(ok.result.value.reportingLines, ['l-1']);
  assert.deepEqual(ok.result.value.assignments, ['a-1']);

  const refused = decode({
    WriteResponse: {
      ok: false,
      error: { code: 'backdated_confirmation_required', message: 'before today', date: '2026-01-01' },
    },
  });
  assert.equal(refused.ok, false);
  assert.equal(refused.error.code, 'backdated_confirmation_required');
  assert.equal(refused.error.date, '2026-01-01');
  assert.equal(refused.error.id, null);
  assert.deepEqual(refused.warnings, []);
  assert.equal(refused.result, null);
});

test('the recompute answer decodes its counters', { skip }, () => {
  const body = decode({ RecomputeResponse: { written: 3, removed: 1, unchanged: 40, cycles_broken: ['u-3'] } });
  assert.equal(body.variant, 'OrgStructureRecomputeResponse');
  assert.equal(body.written, 3);
  assert.deepEqual(body.cyclesBroken, ['u-3']);
});

// ----- file import and export -----------------------------------------------

const FILE = new TextEncoder().encode('kod jednostki;nazwa jednostki\nIT;Dział IT\n');

test('an import request carries the file as a CBOR byte string and the defaults of a plain upsert', { skip }, () => {
  const dry = sent('orgImportDryRunRequest', { format: 'csv', bytes: FILE }).ImportDryRunRequest;
  assert.ok(dry.bytes instanceof Uint8Array, 'a byte string, not an array of integers');
  assert.deepEqual(dry.bytes, FILE);
  assert.deepEqual(
    { ...dry, bytes: undefined },
    { format: 'csv', bytes: undefined, mode: 'upsert', as_of: null, confirm_backdated: false, confirm_ended: false, resolutions: [] },
  );
});

test('an apply carries the mode, the day, the confirmation and the decisions taken on the dry run', { skip }, () => {
  const apply = sent('orgImportApplyRequest', {
    format: 'xlsx',
    bytes: FILE,
    mode: 'replace',
    asOf: DAY,
    confirmBackdated: true,
    confirmEnded: true,
    resolutions: [{ row: 47, action: 'use_suggested_login', login: 'j.kowalski' }, { row: 3, action: 'leave_vacant' }, { row: 4, action: 'skip_row' }],
  }).ImportApplyRequest;
  assert.equal(apply.format, 'xlsx');
  assert.equal(apply.mode, 'replace');
  assert.equal(apply.as_of, DAY);
  assert.equal(apply.confirm_backdated, true);
  assert.equal(apply.confirm_ended, true);
  assert.deepEqual(apply.resolutions, [
    { row: 47, action: 'use_suggested_login', login: 'j.kowalski' },
    { row: 3, action: 'leave_vacant', login: null },
    { row: 4, action: 'skip_row', login: null },
  ]);
  assert.deepEqual(apply.bytes, FILE);
});

test('the error report request repeats the file and the run, and a plain array is a file too', { skip }, () => {
  const errors = sent('orgExportErrorsRequest', { format: 'csv', bytes: Array.from(FILE), as_of: DAY }).ExportErrorsRequest;
  assert.deepEqual(errors.bytes, FILE);
  assert.equal(errors.as_of, DAY);
  assert.equal(errors.mode, 'upsert');
});

test('a file over the socket frame limit is refused before it is encoded', { skip }, () => {
  const big = new Uint8Array(codec.ORG_IMPORT_MAX_FILE_BYTES + 1);
  assert.throws(() => codec.encode.orgImportApplyRequest(1, { format: 'csv', bytes: big }), (e) => e.code === 'file_too_large');
  const edge = new Uint8Array(codec.ORG_IMPORT_MAX_FILE_BYTES);
  assert.doesNotThrow(() => codec.encode.orgImportDryRunRequest(1, { format: 'csv', bytes: edge }));
});

test('an export request names the format and an optional day', { skip }, () => {
  assert.deepEqual(sent('orgExportRequest', { format: 'xlsx', at: DAY }), { ExportRequest: { format: 'xlsx', at: DAY } });
  assert.deepEqual(sent('orgExportRequest', {}), { ExportRequest: { format: 'csv', at: null } });
});

test('the import report decodes its counts, rows, issues with suggestions and the preview', { skip }, () => {
  const body = decode({
    ImportReportResponse: {
      report: {
        mode: 'replace', as_of: DAY, preview_at: DAY, applied: false, sheet: 'Osoby',
        max_file_bytes: 921600, max_rows: 5000,
        ended: [{ kind: 'position', code: 'IT-2', name: 'Analityk', holders: ['Anna Nowak'] }],
        counts: { rows: 212, added: 184, changed: 12, unchanged: 13, errors: 3, issues: 4, assignments_added: 180, assignments_ended: 1 },
        rows: [{
          row: 47, status: 'error', effect: 'added', cells: [{ header: 'login/e-mail osoby', value: 'j.kowlski' }], unit_code: 'NX', position_code: 'NX-1', person: 'j.kowlski',
          changes: [{ entity: 'assignment', field: 'share', before: '1', after: null }], unit_id: 'u-1', position_id: null,
        }],
        errors: [{
          row: 47, rows: [47], column: 'person', kind: 'unknown_person', value: 'j.kowlski', message: 'no account',
          suggestion: 'j.kowalski', suggestion_label: 'Jan Kowalski', code: null,
        }, {
          row: 89, rows: [88, 89], column: 'parent_code', kind: 'unit_cycle', message: 'cycle',
        }],
        warnings: [],
        preview: { at: DAY, timezone: 'Europe/Warsaw' },
        preview_partial: true,
      },
    },
  });
  assert.equal(body.variant, 'OrgStructureImportReportResponse');
  const report = body.report;
  assert.equal(report.asOf, DAY);
  assert.equal(report.sheet, 'Osoby');
  assert.equal(report.maxFileBytes, 921600);
  assert.equal(report.counts.assignmentsEnded, 1);
  assert.deepEqual(report.ended[0].holders, ['Anna Nowak']);
  assert.equal(report.counts.assignmentsAdded, 180);
  assert.equal(report.rows[0].positionCode, 'NX-1');
  assert.equal(report.rows[0].effect, 'added');
  assert.deepEqual(report.rows[0].cells, [{ header: 'login/e-mail osoby', value: 'j.kowlski' }]);
  assert.equal(report.counts.issues, 4);
  assert.equal(report.rows[0].changes[0].field, 'share');
  assert.equal(report.errors[0].suggestionLabel, 'Jan Kowalski');
  assert.deepEqual(report.errors[1].rows, [88, 89]);
  assert.equal(report.errors[1].suggestion, null, 'an absent suggestion is null');
  assert.equal(report.previewPartial, true);
  assert.deepEqual(report.preview.units, []);
  assert.equal(report.fileError, null);
});

test('an unusable file is a typed file error in the report', { skip }, () => {
  const body = decode({
    ImportReportResponse: {
      report: {
        mode: 'upsert', as_of: DAY, preview_at: DAY, applied: false,
        file_error: { code: 'file_too_large', message: 'the file is too large' }, counts: { rows: 0 },
      },
    },
  });
  assert.equal(body.report.fileError.code, 'file_too_large');
  assert.deepEqual(body.report.errors, []);
  assert.equal(body.report.preview, null);
});

test('an export answer hands the file over as a Uint8Array with its name and type', { skip }, () => {
  const file = new Uint8Array([0xef, 0xbb, 0xbf, 0x6b]);
  const body = decode({ ExportResponse: { file_name: 'org-structure-2026-10-01.csv', mime: 'text/csv; charset=utf-8', bytes: file } });
  assert.equal(body.variant, 'OrgStructureExportResponse');
  assert.ok(body.bytes instanceof Uint8Array);
  assert.deepEqual(body.bytes, file);
  assert.equal(body.fileName, 'org-structure-2026-10-01.csv');
  assert.equal(body.mime, 'text/csv; charset=utf-8');
});

// ----- batch of edits -------------------------------------------------------

test('a batch wraps ordinary write requests, each with its optional temporary id', { skip }, () => {
  const batch = sent('orgBatchRequest', {
    dryRun: true,
    confirmBackdated: true,
    ops: [
      { kind: 'unitTypeCreate', tempId: 'tmp:t', name: 'Dział' },
      { kind: 'unitCreate', tempId: 'tmp:u', name: 'IT', typeId: 'tmp:t', validFrom: DAY },
      { kind: 'positionCreate', tempId: 'tmp:p', unitId: 'tmp:u', name: 'CTO', validFrom: DAY },
      { kind: 'headSet', unitId: 'tmp:u', headPositionId: 'tmp:p', from: DAY },
      { kind: 'deputyHeadsSet', unitId: 'tmp:u', positionIds: ['tmp:p'], from: DAY },
      { kind: 'unitUpdate', unitId: 'u-1', name: 'IT 2', clear: ['code'], from: DAY },
      { kind: 'assign', positionId: 'tmp:p', subject: { kind: 'user', id: 'user-1' }, share: 0.5, validFrom: DAY },
      { kind: 'assignmentEnd', assignmentId: 'a-1', from: DAY },
    ],
  }).BatchRequest;
  assert.equal(batch.dry_run, true);
  assert.equal(batch.confirm_backdated, true);
  assert.equal(batch.ops.length, 8);
  assert.deepEqual(batch.ops[0], { temp_id: 'tmp:t', request: { UnitTypeCreateRequest: { name: 'Dział', color: null, icon: null } } });
  assert.deepEqual(batch.ops[1].request, {
    UnitCreateRequest: {
      name: 'IT', code: null, type_id: 'tmp:t', parent_unit_id: null, color: null,
      valid_from: DAY, valid_to: null, confirm_backdated: false,
    },
  });
  assert.equal(batch.ops[1].temp_id, 'tmp:u');
  assert.deepEqual(batch.ops[3], {
    temp_id: null,
    request: { HeadSetRequest: { unit_id: 'tmp:u', head_position_id: 'tmp:p', from: DAY, confirm_backdated: false } },
  });
  assert.deepEqual(batch.ops[4].request.DeputyHeadsSetRequest.position_ids, ['tmp:p']);
  assert.deepEqual(batch.ops[5].request.UnitUpdateRequest.clear, ['code']);
  assert.deepEqual(batch.ops[6].request.AssignRequest.subject, { kind: 'user', id: 'user-1' });
  assert.deepEqual(batch.ops[7].request, { AssignmentEndRequest: { assignment_id: 'a-1', from: DAY, confirm_backdated: false } });

  // Every operation is built by the same table the single request uses.
  const single = sent('orgUnitCreateRequest', { name: 'IT', typeId: 'tmp:t', validFrom: DAY }).UnitCreateRequest;
  assert.deepEqual(batch.ops[1].request.UnitCreateRequest, single);
  assert.deepEqual(sent('orgBatchRequest', { ops: [] }).BatchRequest, { ops: [], dry_run: false, confirm_backdated: false });
});

test('a batch that is too long, too big or has an unknown operation is refused before a frame is built', { skip }, () => {
  const unit = { kind: 'unitCreate', name: 'U', validFrom: DAY };
  assert.doesNotThrow(() => codec.encode.orgBatchRequest(1, { ops: Array.from({ length: codec.ORG_BATCH_MAX_OPS }, () => unit) }));
  assert.throws(
    () => codec.encode.orgBatchRequest(1, { ops: Array.from({ length: codec.ORG_BATCH_MAX_OPS + 1 }, () => unit) }),
    (error) => error.code === 'too_many_ops',
  );
  assert.throws(
    () => codec.encode.orgBatchRequest(1, { ops: [{ kind: 'timezoneSet', timezone: 'UTC' }] }),
    (error) => error.code === 'unknown_batch_op',
  );
  const big = 'x'.repeat(400);
  assert.throws(
    () => codec.encode.orgBatchRequest(1, {
      ops: Array.from({ length: 500 }, () => ({ kind: 'unitCreate', name: big, code: big, color: big, validFrom: DAY, typeId: big, parentUnitId: big })),
    }),
    (error) => error.code === 'batch_too_large',
    'a frame over the socket limit is never sent',
  );
});

test('the batch answer decodes per-operation results, warnings, the preview and the limit', { skip }, () => {
  const body = decode({
    BatchResponse: {
      ok: false,
      applied: false,
      results: [
        { index: 0, ok: true, result: { kind: 'unit', value: { id: 'row', unit_id: 'u-9', name: 'IT', valid_from: DAY } }, temp_id: 'tmp:u', created_id: 'u-9' },
        { index: 1, ok: false, error: { code: 'unknown_temp_id', message: 'no such id', id: 'tmp:x' } },
      ],
      warnings: [{ kind: 'unit_without_head', unit_id: 'u-9', from: DAY }],
      preview_at: DAY,
      preview: { at: DAY, timezone: 'Europe/Warsaw' },
      max_ops: 500,
    },
  });
  assert.equal(body.variant, 'OrgStructureBatchResponse');
  assert.equal(body.ok, false);
  assert.equal(body.applied, false);
  assert.equal(body.error, null);
  assert.equal(body.maxOps, 500);
  assert.equal(body.previewAt, DAY);
  assert.deepEqual(body.preview.units, []);
  assert.equal(body.warnings[0].unitId, 'u-9');
  assert.equal(body.results[0].tempId, 'tmp:u');
  assert.equal(body.results[0].createdId, 'u-9');
  assert.equal(body.results[0].result.value.unitId, 'u-9');
  assert.equal(body.results[0].error, null);
  assert.equal(body.results[1].index, 1);
  assert.equal(body.results[1].error.code, 'unknown_temp_id');
  assert.equal(body.results[1].error.id, 'tmp:x');
  assert.equal(body.results[1].createdId, null);

  const refused = decode({ BatchResponse: { ok: false, applied: false, error: { code: 'too_many_ops', message: 'at most 500' } } });
  assert.equal(refused.error.code, 'too_many_ops');
  assert.deepEqual(refused.results, []);
  assert.equal(refused.preview, null);
});

// ----- deputies, absences, escalation and visibility -------------------------

test('the cover reads carry the person, the day, the scope and the kind of data asked for', { skip }, () => {
  assert.deepEqual(sent('orgCoverRequest', {}), { CoverRequest: { user_id: null, at: null, include_past: false } });
  assert.deepEqual(sent('orgCoverRequest', { userId: 'u-1', at: DAY, includePast: true }), {
    CoverRequest: { user_id: 'u-1', at: DAY, include_past: true },
  });
  assert.deepEqual(sent('orgAvailabilityRequest', {}), { AvailabilityRequest: { at: null } });
  assert.deepEqual(sent('orgEscalationChainRequest', { userId: 'u-1', scope: 'approvals', at: DAY }), {
    EscalationChainRequest: { user_id: 'u-1', scope: 'approvals', at: DAY },
  });
  assert.deepEqual(sent('orgIsAvailableRequest', { userId: 'u-1' }), { IsAvailableRequest: { user_id: 'u-1', at: null } });
  assert.deepEqual(sent('orgCanViewPersonDataRequest', { subjectUserId: 'u-2', kind: 'absence_reason' }), {
    CanViewPersonDataRequest: { viewer_user_id: null, subject_user_id: 'u-2', kind: 'absence_reason', at: null },
  });
  assert.deepEqual(sent('orgVisibilityRequest', {}), { VisibilityRequest: { user_id: null, at: null } });
  assert.deepEqual(sent('orgWhoSeesRequest', { subjectUserId: 'u-3' }), { WhoSeesRequest: { subject_user_id: 'u-3', at: null } });
});

test('a deputy is appointed, changed and ended by day, with an exclusive end and a clear list', { skip }, () => {
  assert.deepEqual(sent('orgDeputySetRequest', { userId: 'u-1', deputyUserId: 'u-2', validFrom: DAY }), {
    DeputySetRequest: {
      user_id: 'u-1', deputy_user_id: 'u-2', scope: 'all', valid_from: DAY, valid_to: null, confirm_backdated: false,
    },
  });
  assert.deepEqual(sent('orgDeputyUpdateRequest', { id: 'd-1', scope: 'project:p-7', clear: ['valid_to'], confirmBackdated: true }), {
    DeputyUpdateRequest: {
      id: 'd-1', scope: 'project:p-7', valid_from: null, valid_to: null, clear: ['valid_to'], confirm_backdated: true,
    },
  });
  assert.deepEqual(sent('orgDeputyEndRequest', { id: 'd-1', from: DAY }), {
    DeputyEndRequest: { id: 'd-1', from: DAY, confirm_backdated: false },
  });
});

test('an absence is added for the caller by default, patched by name and deleted', { skip }, () => {
  assert.deepEqual(sent('orgAbsenceAddRequest', { validFrom: DAY, validTo: '2026-10-05', kind: 'leave', reason: 'dentist' }), {
    AbsenceAddRequest: {
      user_id: null, valid_from: DAY, valid_to: '2026-10-05', kind: 'leave', reason: 'dentist', confirm_backdated: false,
    },
  });
  assert.deepEqual(sent('orgAbsenceUpdateRequest', { id: 'a-1', kind: 'other', clear: ['reason', 'valid_to'] }), {
    AbsenceUpdateRequest: {
      id: 'a-1', valid_from: null, valid_to: null, kind: 'other', reason: null, clear: ['reason', 'valid_to'], confirm_backdated: false,
    },
  });
  assert.deepEqual(sent('orgAbsenceDeleteRequest', { id: 'a-1', confirmBackdated: true }), {
    AbsenceDeleteRequest: { id: 'a-1', confirm_backdated: true },
  });
});

test('the cover answer decodes absences with an optional reason, deputies and the caller\'s rights', { skip }, () => {
  const body = decode({
    CoverResponse: {
      user_id: 'u-1', display_name: 'Anna', available: false, today: DAY,
      absences: [{ id: 'a-1', user_id: 'u-1', valid_from: DAY, valid_to: null, kind: 'training', reason: 'szkolenie', source: 'manual' }],
      covered_by: [{
        id: 'd-1', user_id: 'u-1', user_name: 'Anna', deputy_user_id: 'u-2', deputy_name: 'Marek',
        scope: 'project:p-7', valid_from: DAY, valid_to: '2026-10-08',
      }],
      can_see_absences: true, can_see_reason: true, can_edit_absences: true, can_edit_deputies: false,
    },
  });
  assert.equal(body.variant, 'OrgStructureCoverResponse');
  assert.equal(body.available, false);
  assert.equal(body.absences[0].reason, 'szkolenie');
  assert.equal(body.absences[0].validTo, null);
  assert.equal(body.coveredBy[0].deputyName, 'Marek');
  assert.equal(body.coveredBy[0].scope, 'project:p-7');
  assert.deepEqual(body.covering, []);
  assert.equal(body.canEditDeputies, false);

  // A viewer who may not see the dates gets no absences and no rights.
  const hidden = decode({ CoverResponse: { user_id: 'u-1', available: true, today: DAY } });
  assert.deepEqual(hidden.absences, []);
  assert.equal(hidden.canSeeAbsences, false);
  assert.equal(hidden.canSeeReason, false);
});

test('availability, chain, visibility and viewer answers decode', { skip }, () => {
  const availability = decode({ AvailabilityResponse: { at: DAY, absent_user_ids: ['u-1'], deputies: [] } });
  assert.deepEqual(availability.absentUserIds, ['u-1']);

  const chain = decode({
    EscalationChainResponse: {
      steps: [{ level: 2, user_id: 'u-3', display_name: 'Ewa', position_id: 'p-1', position_name: 'Dyrektor', via: 'deputy_head', covering_user_id: 'u-4', covering_name: 'Jan' }],
      skipped: [{ level: 1, position_id: 'p-0', position_name: 'Kierownik', reason: 'unavailable' }],
      problem: { kind: 'cycle', position_id: 'p-9' },
    },
  });
  assert.equal(chain.steps[0].via, 'deputy_head');
  assert.equal(chain.steps[0].coveringUserId, 'u-4');
  assert.equal(chain.skipped[0].reason, 'unavailable');
  assert.equal(chain.problem.kind, 'cycle');
  assert.equal(decode({ EscalationChainResponse: {} }).problem, null);

  const view = decode({
    VisibilityResponse: {
      user: { user_id: 'u-1', display_name: 'Anna' },
      subtree: [{ user_id: 'u-2', display_name: 'Marek' }],
      rows: [{ area: 'absence_reasons', verdict: 'direct', rule: 'primary_manager' }],
    },
  });
  assert.equal(view.manager, null);
  assert.equal(view.rows[0].verdict, 'direct');
  assert.deepEqual(view.direct, []);

  const who = decode({
    WhoSeesResponse: {
      subject: { user_id: 'u-2', display_name: 'Marek' },
      viewers: [{ user_id: 'u-1', display_name: 'Anna', rule: 'primary_manager', kinds: ['absence_reason'] }],
    },
  });
  assert.equal(who.viewers[0].rule, 'primary_manager');
  assert.deepEqual(who.viewers[0].kinds, ['absence_reason']);

  assert.equal(decode({ IsAvailableResponse: { available: true } }).available, true);
  assert.equal(decode({ CanViewPersonDataResponse: { allowed: false, rule: 'none' } }).rule, 'none');
});

test('a write answer carries a deputy or an absence as its result, a manager its source', { skip }, () => {
  const deputy = decode({
    WriteResponse: {
      ok: true,
      result: { kind: 'deputy', value: { id: 'd-1', user_id: 'u-1', deputy_user_id: 'u-2', scope: 'all', valid_from: DAY } },
    },
  });
  assert.equal(deputy.result.kind, 'deputy');
  assert.equal(deputy.result.value.deputyUserId, 'u-2');
  const absence = decode({
    WriteResponse: {
      ok: true,
      result: { kind: 'absence', value: { id: 'a-1', user_id: 'u-1', valid_from: DAY, kind: 'leave', source: 'manual' } },
    },
  });
  assert.equal(absence.result.value.kind, 'leave');
  assert.equal(absence.result.value.validTo, null);
  assert.equal(decode({ ManagerResponse: { manager: { user_id: 'u-9', position_id: 'p-0', source: 'deputy' } } }).manager.source, 'deputy');
  assert.equal(decode({ ManagerResponse: { manager: { user_id: 'u-9', position_id: 'p-0' } } }).manager.source, '', 'an answer from before the field');
});

// ----- history and planned reorganizations -----------------------------------

test('the history reads carry their range, unit and page, absent values as null', { skip }, () => {
  assert.deepEqual(sent('orgHistoryListRequest', {}), {
    HistoryListRequest: { from: null, to: null, unit_id: null, offset: 0, limit: 0 },
  });
  assert.deepEqual(sent('orgHistoryListRequest', { from: DAY, unitId: 'u-1', offset: 30, limit: 30 }), {
    HistoryListRequest: { from: DAY, to: null, unit_id: 'u-1', offset: 30, limit: 30 },
  });
  assert.deepEqual(sent('orgHistoryDiffRequest', { from: DAY, to: '2026-11-01' }), {
    HistoryDiffRequest: { from: DAY, to: '2026-11-01', unit_id: null },
  });
});

test('a change set is saved with the batch\'s operations, and every step names it by id', { skip }, () => {
  const save = sent('orgChangeSetSaveRequest', {
    name: 'Q4',
    effectiveDate: '2026-11-01',
    ops: [
      { kind: 'unitCreate', tempId: 'tmp:q', name: 'Quality', validFrom: '2026-11-01' },
      { kind: 'positionMove', positionId: 'p-1', from: '2026-11-01' },
    ],
  }).ChangeSetSaveRequest;
  assert.equal(save.id, null);
  assert.equal(save.name, 'Q4');
  assert.equal(save.effective_date, '2026-11-01');
  assert.equal(save.ops[0].temp_id, 'tmp:q');
  assert.deepEqual(save.ops[1].request, {
    PositionMoveRequest: { position_id: 'p-1', new_parent_position_id: null, from: '2026-11-01', confirm_backdated: false },
  });
  assert.equal(sent('orgChangeSetSaveRequest', { id: 'cs-1', name: 'Q4', effectiveDate: DAY, ops: [] }).ChangeSetSaveRequest.id, 'cs-1');

  assert.deepEqual(sent('orgChangeSetListRequest', {}), { ChangeSetListRequest: {} });
  for (const step of ['Get', 'Submit', 'Approve', 'Withdraw']) {
    assert.deepEqual(sent(`orgChangeSet${step}Request`, { id: 'cs-1' }), { [`ChangeSet${step}Request`]: { id: 'cs-1' } });
  }
  assert.deepEqual(sent('orgChangeSetPreviewRequest', { id: 'cs-1', unitId: 'u-1' }), {
    ChangeSetPreviewRequest: { id: 'cs-1', unit_id: 'u-1' },
  });
  assert.throws(
    () => codec.encode.orgChangeSetSaveRequest(1, { name: 'x', effectiveDate: DAY, ops: [{ kind: 'timezoneSet' }] }),
    (error) => error.code === 'unknown_batch_op',
  );
  assert.throws(
    () => codec.encode.orgChangeSetSaveRequest(1, { name: 'x', effectiveDate: DAY, ops: Array.from({ length: codec.ORG_BATCH_MAX_OPS + 1 }, () => ({ kind: 'unitCreate', name: 'U', validFrom: DAY })) }),
    (error) => error.code === 'too_many_ops',
  );
});

test('the history answers decode entries with their changes, and diffs with their people', { skip }, () => {
  const history = decode({
    HistoryListResponse: {
      entries: [{
        id: 7, at: '2026-09-29 10:00:00', action: 'org.unit.move', target_kind: 'unit', target_id: 'u-1', actor_name: 'Hanna',
        effective_date: '2026-11-01', hidden_ops: 1,
        changes: [{ field: 'parent_unit_id', before: 'u-0', after: 'u-9', after_label: 'Realizacja' }],
        ops: [{ action: 'org.unit.create', target_kind: 'unit', target_id: 'u-2' }],
      }],
      total: 1, personal_visible: false, today: '2026-09-30',
    },
  });
  assert.equal(history.variant, 'OrgStructureHistoryListResponse');
  assert.equal(history.personalVisible, false);
  assert.equal(history.entries[0].effectiveDate, '2026-11-01');
  assert.equal(history.entries[0].hiddenOps, 1);
  assert.equal(history.entries[0].changes[0].afterLabel, 'Realizacja');
  assert.equal(history.entries[0].changes[0].before_label, null, 'an absent label is null');
  assert.equal(history.entries[0].ops[0].targetId, 'u-2');

  const diff = decode({
    HistoryDiffResponse: {
      from: DAY, to: '2026-11-01', personal_visible: true,
      items: [{ change: 'added', entity: 'assignment', id: 'p-1', name: 'Tester', subject: { kind: 'user', id: 'u-5' }, subject_name: 'Ewa' }],
    },
  });
  assert.equal(diff.items[0].subjectName, 'Ewa');
  assert.deepEqual(diff.items[0].subject, { kind: 'user', id: 'u-5' });
});

test('the change set answers decode the state, the people, the operations and the per-operation refusals', { skip }, () => {
  const answer = decode({
    ChangeSetResponse: {
      ok: false,
      error: { code: 'self_approval', message: 'no' },
      change_set: {
        id: 'cs-1', name: 'Q4', effective_date: '2026-11-01', state: 'pending', author_user_id: 'u-1', author_name: 'Hanna',
        created_at_ms: 1790000000000, op_count: 1,
        ops: [{ temp_id: 'tmp:q', request: { UnitCreateRequest: { name: 'Quality', valid_from: '2026-11-01' } } }],
      },
      valid: true,
      results: [{ index: 0, ok: false, error: { code: 'not_valid_at', message: 'm', date: '2026-11-01' } }],
    },
  });
  assert.equal(answer.variant, 'OrgStructureChangeSetResponse');
  assert.equal(answer.error.code, 'self_approval');
  assert.equal(answer.change_set.effective_date, '2026-11-01');
  assert.equal(answer.change_set.author_name, 'Hanna');
  assert.equal(answer.change_set.approver_user_id, null);
  assert.equal(answer.change_set.ops[0].temp_id, 'tmp:q');
  assert.equal(answer.change_set.ops[0].request.UnitCreateRequest.name, 'Quality');
  assert.equal(answer.results[0].error.date, '2026-11-01');

  const preview = decode({
    ChangeSetPreviewResponse: {
      ok: true, valid: true, at: '2026-11-01',
      live: { at: '2026-11-01', timezone: 'Europe/Warsaw' }, preview: { at: '2026-11-01', timezone: 'Europe/Warsaw' },
      items: [{ change: 'added', entity: 'unit', id: 'u-q', name: 'Quality' }],
    },
  });
  assert.equal(preview.variant, 'OrgStructureChangeSetPreviewResponse');
  assert.deepEqual(preview.live.units, []);
  assert.equal(preview.items[0].name, 'Quality');

  const bare = decode({ ChangeSetResponse: { ok: true } });
  assert.deepEqual([bare.valid, bare.results, bare.change_set], [false, [], null], 'an answer without the defaulted fields');
});

// ----- handover ("Do przekazania") -------------------------------------------

test('the handover requests carry the person, the reason, the day, the note and the chosen takers', { skip }, () => {
  assert.deepEqual(sent('orgHandoverListRequest', { userId: 'u-1', reason: 'departure' }), {
    HandoverListRequest: { user_id: 'u-1', reason: 'departure', project_id: null, date: null, return_date: null },
  });
  assert.deepEqual(sent('orgHandoverListRequest', { userId: 'u-1', reason: 'project_removal', projectId: 'p-7', date: DAY }), {
    HandoverListRequest: { user_id: 'u-1', reason: 'project_removal', project_id: 'p-7', date: DAY, return_date: null },
  });
  assert.deepEqual(sent('orgHandoverApplyRequest', {
    userId: 'u-1', reason: 'absence', returnDate: '2026-10-09', note: 'Wracam',
    items: [{ key: 'task:p-1:t-1', takerUserId: 'u-2' }, { key: 'member:p-1' }],
  }), {
    HandoverApplyRequest: {
      user_id: 'u-1', reason: 'absence', project_id: null, date: null, return_date: '2026-10-09', note: 'Wracam',
      items: [{ key: 'task:p-1:t-1', taker_user_id: 'u-2' }, { key: 'member:p-1', taker_user_id: null }],
    },
  });
  assert.deepEqual(sent('orgHandoverRetryRequest', { handoverId: 'h-1', keys: ['a'] }), {
    HandoverRetryRequest: { handover_id: 'h-1', keys: ['a'] },
  });
  assert.deepEqual(sent('orgHandoverRetryRequest', { handoverId: 'h-1' }), { HandoverRetryRequest: { handover_id: 'h-1', keys: [] } });
  assert.deepEqual(sent('orgHandoverPendingRequest', {}), { HandoverPendingRequest: {} });
  assert.deepEqual(sent('orgHandoverRecordsRequest', {}), { HandoverRecordsRequest: { user_id: null } });
  assert.deepEqual(sent('orgHandoverRecordsRequest', { userId: 'u-3' }), { HandoverRecordsRequest: { user_id: 'u-3' } });
});

test('a handover reason the server does not know is not encoded', { skip }, () => {
  assert.throws(() => codec.encode.orgHandoverListRequest(1, { userId: 'u-1', reason: 'holiday' }), /HandoverList/);
});

test('the handover answers decode groups, proposals, per-item results and records', { skip }, () => {
  const list = decode({
    HandoverListResponse: {
      user: { user_id: 'u-1', display_name: 'Piotr' }, reason: 'departure', date: DAY, assignment_ended_on: null,
      groups: [{
        category: 'task',
        items: [{
          key: 'task:p-1:t-1', category: 'task', title: '#1 Import', role: 'assignee', state: 'todo', project_id: 'p-1',
          project_name: 'NextApp', action: 'transfer', suggestion: { user_id: 'u-2', reason: 'deputy' },
          eligible_user_ids: ['u-2', 'u-3'],
        }],
      }],
      takers: [{ user_id: 'u-2', display_name: 'Anna' }],
    },
  });
  assert.equal(list.variant, 'OrgStructureHandoverListResponse');
  assert.equal(list.groups[0].items[0].suggestion.reason, 'deputy');
  assert.deepEqual(list.groups[0].items[0].eligible_user_ids, ['u-2', 'u-3']);
  assert.equal(list.groups[0].items[0].blocked, null, 'a defaulted field arrives as null');
  assert.equal(list.return_date, null);

  const applied = decode({
    HandoverApplyResponse: {
      ok: false, handover_id: 'h-1', applied: 1, failed: 1,
      items: [{ key: 'task:p-1:t-1', category: 'task', title: '#1', status: 'failed', reason: 'internal' }],
    },
  });
  assert.equal(applied.variant, 'OrgStructureHandoverApplyResponse');
  assert.equal(applied.handover_id, 'h-1');
  assert.equal(applied.items[0].reason, 'internal');
  assert.equal(applied.scheduled, 0);
  assert.equal(applied.error, null);

  const pending = decode({ HandoverPendingResponse: { people: [{ user_id: 'u-1', display_name: 'Piotr', count: 3, ended_on: DAY }] } });
  assert.equal(pending.people[0].count, 3);

  const records = decode({
    HandoverRecordsResponse: {
      records: [{ id: 'h-1', user_id: 'u-1', reason: 'absence', date: DAY, return_date: '2026-10-09', note: 'n', created_at_ms: 1790000000000, items: [] }],
    },
  });
  assert.equal(records.records[0].return_date, '2026-10-09');
  assert.deepEqual(decode({ HandoverRecordsResponse: {} }).records, []);
});
