// ============ File: project-studio-tasks.test.js — Catalogue, parent choice and historical attachment presentation ============

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { projectKeySuggestion, taskTypeLabel, taskTypeDescription, activeTaskTypes, parentCandidates, taskDuration, taskEventValue, taskEventTaskIds, taskEventAttachments, taskNotificationText } from './project-studio-tasks.js';

const translate = (key, fields) => fields ? `${key}:${JSON.stringify(fields)}` : key;

test('project prefix suggestion matches the persisted prefix rules before reservation', () => {
  assert.equal(projectKeySuggestion('Project Studio'), 'PROJECTS');
  assert.equal(projectKeySuggestion('12 demo'), 'P12DEMO');
  assert.equal(projectKeySuggestion('Żółć'), 'PP');
  assert.equal(projectKeySuggestion('A'), 'AP');
});

test('custom and inactive type names stay intact while built-ins are translated', () => {
  const types = [
    { type_id: 'custom', name: 'Customer <review>', description: 'Actual description', active: false, built_in: false, sort_order: 5 },
    { type_id: 'feature', name: 'Feature', description: 'Default', active: true, built_in: true, sort_order: 0 },
    { type_id: 'technical', name: 'Technical', active: true, built_in: true, sort_order: 10 },
  ];
  assert.equal(taskTypeLabel(types[0], translate), 'Customer <review>');
  assert.equal(taskTypeDescription(types[0], translate), 'Actual description');
  assert.equal(taskTypeLabel(types[1], translate), 'task_type_feature');
  assert.deepEqual(activeTaskTypes(types).map((type) => type.type_id), ['feature', 'technical']);
  assert.deepEqual(activeTaskTypes(types, 'custom').map((type) => type.type_id), ['feature', 'custom', 'technical']);
});

test('parent choices exclude archived and self and limit normal tasks to epics', () => {
  const rows = [
    { task_id: 'self', task_type: 'epic' },
    { task_id: 'epic', task_type: 'epic' },
    { task_id: 'feature', task_type: 'feature' },
    { task_id: 'subtask', task_type: 'subtask' },
    { task_id: 'archived', task_type: 'epic', archived_at: '2026-10-01' },
  ];
  assert.deepEqual(parentCandidates(rows, 'feature', 'self').map((task) => task.task_id), ['epic']);
  assert.deepEqual(parentCandidates(rows, 'subtask', 'self').map((task) => task.task_id), ['epic', 'feature']);
  assert.deepEqual(parentCandidates(rows, 'epic', 'self'), []);
});

test('removed attachment metadata remains available from recorded before and after values', () => {
  const old = { sha256: 'a'.repeat(64), name: 'Original <file>.avi', mime: 'video/x-msvideo', size_bytes: 90000000 };
  const current = { ...old, sha256: 'b'.repeat(64), name: 'New image.png' };
  const event = { kind: 'attachments_json', before_json: JSON.stringify(JSON.stringify([old])), after_json: JSON.stringify(JSON.stringify([old, current])) };
  assert.deepEqual(taskEventAttachments(event), [old, current]);
  assert.deepEqual(taskEventAttachments({ kind: 'created', before_json: 'null', after_json: JSON.stringify({ attachments_json: JSON.stringify([old]) }) }), [old]);
  assert.deepEqual(taskEventAttachments({ kind: 'title', before_json: '"Before"', after_json: '"After"' }), []);
});

test('status durations use recorded seconds and history uses structural person identities', () => {
  assert.equal(taskDuration(90061, translate), 'task_duration_days:{"days":1,"hours":1}');
  assert.equal(taskDuration(3661, translate), 'task_duration_hours:{"hours":1,"minutes":1}');
  const value = taskEventValue('{"assigned_to":"user-two","mention_user_ids":["user-one","user-two"]}', { translate, memberName: (id) => ({ 'user-one': 'First person', 'user-two': 'Second person' })[id], taskName: () => '' });
  assert.match(value, /Second person/);
  assert.match(value, /First person, Second person/);
});

test('history uses authorized record labels and translated values without internal identities in all languages', () => {
  const parentId = '467e6d78-fda9-48a2-bca0-d2874c124188';
  const unavailableId = '23ce94e1-061c-4232-8b60-0e54dc6bfdda';
  for (const locale of ['pl', 'en', 'de', 'es', 'fr']) {
    const dictionary = JSON.parse(readFileSync(new URL(`../../i18n/${locale}.json`, import.meta.url))).project_studio;
    const localized = (key) => { assert.equal(typeof dictionary[key], 'string', `${locale}: ${key}`); return dictionary[key]; };
    const context = { translate: localized, memberName: (id) => id === 'actual-person' ? 'Actual person' : null,
      taskName: (id) => id === parentId ? 'WF-1 · Actual parent' : null,
      typeName: (id) => id === 'release_review' ? 'Customer review' : null };
    const created = taskEventValue(JSON.stringify({ task_type: 'release_review', status: 'in_progress', priority: 'high', severity: 'critical', parent_task_id: parentId, assigned_to: unavailableId }), context);
    assert.ok(created.includes('Customer review')); assert.ok(created.includes('WF-1 · Actual parent'));
    for (const key of ['task_status_in_progress', 'prio_high', 'sev_critical', 'task_history_unavailable_person']) assert.ok(created.includes(dictionary[key]));
    assert.equal(created.includes(parentId), false); assert.equal(created.includes(unavailableId), false);
    assert.equal(created.includes('release_review'), false);
    const relation = taskEventValue(JSON.stringify({ link_id: 32, source_task_id: parentId, target_task_id: unavailableId, kind: 'related', lag_days: 3 }), context);
    assert.ok(relation.includes(dictionary.task_relation_related)); assert.ok(relation.includes(dictionary.task_history_unavailable_record));
    assert.equal(relation.includes(parentId), false); assert.equal(relation.includes(unavailableId), false); assert.equal(relation.includes('32'), false);
    const comment = taskEventValue(JSON.stringify({ comment_id: unavailableId, handover_id: parentId, body_md: 'Recorded comment', mention_user_ids: ['actual-person', unavailableId] }), context);
    assert.ok(comment.includes('Recorded comment')); assert.ok(comment.includes('Actual person'));
    assert.equal(comment.includes(parentId), false); assert.equal(comment.includes(unavailableId), false);
    assert.equal(taskEventValue(JSON.stringify(parentId), { ...context, kind: 'parent_task_id' }), 'WF-1 · Actual parent');
    assert.equal(taskEventValue('"review"', { ...context, kind: 'status_changed' }), dictionary.task_status_review);
    assert.equal(taskEventValue('"unknown_type"', { ...context, kind: 'task_type' }), dictionary.task_history_unavailable_type);
    const file = { sha256: 'a'.repeat(64), name: 'Historical recording.avi', mime: 'video/avi', size_bytes: 124709766 };
    assert.equal(taskEventValue(JSON.stringify(JSON.stringify([file])), { ...context, kind: 'attachments_json' }), file.name);
  }
});

test('historical record resolution collects only structural task endpoints', () => {
  assert.deepEqual(taskEventTaskIds({ kind: 'parent_task_id', before_json: '"previous"', after_json: '"current"' }), ['previous', 'current']);
  assert.deepEqual(taskEventTaskIds({ kind: 'link_deleted', before_json: '{"source_task_id":"source","target_task_id":"target","link_id":42}', after_json: 'null' }), ['source', 'target']);
  assert.deepEqual(taskEventTaskIds({ kind: 'created', before_json: 'null', after_json: '{"parent_task_id":"parent","comment_id":"private-comment","title":"User text"}' }), ['parent']);
  assert.deepEqual(taskEventTaskIds({ kind: 'comment_deleted', before_json: '{"comment_id":"private-comment"}', after_json: 'null' }), []);
});

test('task notifications use structured keys and translated status names in every supported language', () => {
  const link = { task_key: 'WF-12', task_title: 'Review <contract>', from_status: 'todo', to_status: 'in_progress', from_user_id: 'previous-assignee', to_user_id: 'next-assignee', event_id: 12 };
  for (const locale of ['pl', 'en', 'de', 'es', 'fr']) {
    const dictionary = JSON.parse(readFileSync(new URL(`../../i18n/${locale}.json`, import.meta.url))).project_studio;
    const localized = (key, fields = {}) => {
      assert.equal(typeof dictionary[key], 'string', `${locale}: ${key}`);
      return dictionary[key].replace(/\{(\w+)\}/g, (match, name) => String(fields[name]));
    };
    const text = taskNotificationText({ kind: 'task_status_changed', title: 'Untranslated server text', body: 'Old body', link_json: JSON.stringify(link) }, localized);
    assert.equal(text.title, dictionary.nk_task_status_changed);
    assert.match(text.body, /WF-12 · Review <contract>/);
    assert.ok(text.body.includes(dictionary.task_status_todo));
    assert.ok(text.body.includes(dictionary.task_status_in_progress));
    assert.equal(text.body.includes('Untranslated server text'), false);
    for (const kind of ['task_assigned', 'task_reassigned', 'task_unassigned', 'task_mentioned', 'task_handed_over', 'task_handed_back']) {
      const result = taskNotificationText({ kind, title: 'Unlocalized server title', body: 'Unlocalized server body', link_json: JSON.stringify({ ...link, to_user_id: kind === 'task_unassigned' ? '' : link.to_user_id }) }, localized);
      assert.equal(result.title, dictionary[`nk_${kind}`]);
      assert.equal(result.body, 'WF-12 · Review <contract>');
      assert.equal(result.title.includes('Unlocalized server'), false);
    }
  }
  assert.equal(taskNotificationText({ kind: 'run_finished' }, translate), null);
});
