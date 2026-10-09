// ============ File: project-studio-tasks.js — Task catalogue and recorded-history presentation ============

export const TASK_LINK_KINDS = ['related', 'duplicate', 'fs', 'ss', 'ff', 'sf'];
const TASK_NOTIFICATION_KINDS = ['task_assigned', 'task_reassigned', 'task_unassigned', 'task_status_changed', 'task_mentioned', 'task_handed_over', 'task_handed_back'];

export function taskNotificationText(notification, translate) {
  if (!TASK_NOTIFICATION_KINDS.includes(notification.kind)) return null;
  const link = JSON.parse(notification.link_json);
  const fields = { key: link.task_key, title: link.task_title };
  if (notification.kind === 'task_status_changed') {
    fields.from = translate(`task_status_${link.from_status}`);
    fields.to = translate(`task_status_${link.to_status}`);
  }
  return { title: translate(`nk_${notification.kind}`), body: translate(notification.kind === 'task_status_changed' ? 'task_notification_status' : 'task_notification_task', fields) };
}

const ORG_NOTIFICATION_KINDS = ['work_handed_over', 'deputy_appointed', 'deputy_ended'];

// Notifications of the organizational structure carry their facts in link_json; the sentence is built
// here so it follows the reader's language, not the language of the node that wrote it.
export function orgNotificationText(notification, translate) {
  if (!ORG_NOTIFICATION_KINDS.includes(notification.kind)) return null;
  const link = JSON.parse(notification.link_json);
  const title = translate(`nk_${notification.kind}`);
  if (notification.kind === 'work_handed_over') {
    const fields = { from: link.from_name, count: link.count, note: link.note, until: link.return_on };
    return { title, body: translate(link.return_on ? 'org_notification_handover_until' : 'org_notification_handover', fields) };
  }
  const fields = { who: link.who, from: link.from, until: link.until, scope: link.scope };
  return { title, body: translate(link.until ? 'org_notification_deputy_until' : 'org_notification_deputy', fields) };
}

export function projectKeySuggestion(name) {
  let prefix = name.replace(/[^a-z0-9]/gi, '').slice(0, 8).toUpperCase();
  if (!/^[A-Z]/.test(prefix)) prefix = `P${prefix}`;
  if (prefix.length === 1) prefix += 'P';
  return prefix.slice(0, 8);
}

export function taskTypeLabel(type, translate) {
  return type.built_in ? translate(`task_type_${type.type_id}`) : type.name;
}

export function taskTypeDescription(type, translate) {
  return type.built_in ? translate(`task_type_${type.type_id}_desc`) : type.description;
}

export function taskDuration(seconds, translate) {
  const value = Math.max(0, Number(seconds) || 0);
  if (value >= 86400) return translate('task_duration_days', { days: Math.floor(value / 86400), hours: Math.floor(value % 86400 / 3600) });
  if (value >= 3600) return translate('task_duration_hours', { hours: Math.floor(value / 3600), minutes: Math.floor(value % 3600 / 60) });
  return translate('task_duration_minutes', { minutes: Math.floor(value / 60) });
}

export function taskEventValue(json, { translate, memberName, taskName, projectName, typeName, kind = '' }) {
  const value = JSON.parse(json);
  const person = (id) => memberName(id) || translate('task_history_unavailable_person');
  const fieldValue = (field, content) => {
    if (content === null || content === '') return translate('task_history_empty_value');
    if (['assigned_to', 'from_user_id', 'to_user_id'].includes(field)) return person(content);
    if (['parent_task_id', 'source_task_id', 'target_task_id'].includes(field)) return taskName(content) || translate('task_history_unavailable_record');
    if (['project_id', 'source_project_id', 'target_project_id'].includes(field)) return projectName(content) || translate('task_history_unavailable_record');
    if (field === 'project_name') return projectName(value.project_id) || translate('task_history_unavailable_record');
    if (field === 'mention_user_ids') return content.length ? content.map(person).join(', ') : translate('task_history_empty_value');
    if (field === 'task_type') return typeName(content) || translate('task_history_unavailable_type');
    if (field === 'resolution') return content === 'not_pursued' ? translate('task_not_pursued') : translate('task_history_empty_value');
    if (field === 'status') return translate(`task_status_${content}`);
    if (field === 'priority') return translate(`prio_${content}`);
    if (field === 'severity') return translate(`sev_${content}`);
    if (field === 'kind') return translate(`task_relation_${content}`);
    if (field === 'archived') return translate(content ? 'status_archived' : 'status_active');
    if (field === 'attachments_json') {
      const files = JSON.parse(content);
      return files.length ? files.map((file) => file.name).join(', ') : translate('task_history_empty_value');
    }
    if (field === 'links_json') {
      const links = JSON.parse(content);
      return links.length ? links.map((link) => `${translate(`link_kind_${link.kind}`)}: ${link.label || translate('task_history_unavailable_record')}`).join(', ') : translate('task_history_empty_value');
    }
    return String(content);
  };
  if (value === null || value === '') return translate(kind === 'transferred' ? 'task_history_unavailable_record' : 'task_history_empty_value');
  if (typeof value !== 'object') {
    const field = kind === 'status_changed' ? 'status' : ['assigned', 'reassigned', 'unassigned'].includes(kind) ? 'assigned_to' : kind;
    return fieldValue(field, value);
  }
  const fields = Object.entries(value).filter(([field]) => !['comment_id', 'link_id', 'handover_id', 'operation_id', 'relation_id'].includes(field) && !(field === 'project_id' && 'project_name' in value));
  return fields.length ? fields.map(([field, content]) => `${translate(`task_history_field_${field === 'project_id' ? 'project_name' : field}`)}: ${fieldValue(field, content)}`).join('\n') : translate('task_history_empty_value');
}

export function taskEventReferences(event) {
  const tasks = new Set();
  const projects = new Set();
  for (const json of [event.before_json, event.after_json]) {
    const value = JSON.parse(json);
    if (event.kind === 'parent_task_id' && value) tasks.add(value);
    else if (value && typeof value === 'object') {
      for (const field of ['parent_task_id', 'source_task_id', 'target_task_id']) if (value[field]) tasks.add(value[field]);
      for (const field of ['project_id', 'source_project_id', 'target_project_id']) if (value[field]) projects.add(value[field]);
    }
  }
  return { tasks: [...tasks], projects: [...projects] };
}

export function taskEventAttachments(event) {
  const attachments = [];
  for (const json of [event.before_json, event.after_json]) {
    const value = JSON.parse(json);
    const raw = event.kind === 'attachments_json' ? value : value?.attachments_json;
    if (typeof raw !== 'string') continue;
    for (const attachment of JSON.parse(raw)) if (!attachments.some((entry) => entry.sha256 === attachment.sha256)) attachments.push(attachment);
  }
  return attachments;
}

export function activeTaskTypes(types, currentId = '') {
  return types.filter((type) => type.active || type.type_id === currentId)
    .slice().sort((a, b) => Number(a.sort_order) - Number(b.sort_order) || a.name.localeCompare(b.name));
}

export function parentCandidates(tasks, type, ownId) {
  return tasks.filter((task) => task.task_id !== ownId && !task.archived_at
    && (type === 'subtask' ? task.task_type !== 'subtask' : type !== 'epic' && task.task_type === 'epic'));
}
