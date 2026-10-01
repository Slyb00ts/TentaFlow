// ============ File: project-studio-access.js — Project Studio UI projections of authoritative server access ============

export const PROJECT_AREAS = ['tasks', 'board', 'sprints', 'roadmap', 'modules', 'changelog', 'releases', 'tests', 'environments', 'repos', 'knowledge', 'docs', 'chat', 'security', 'security.confidential', 'settings'];
export const PERMISSION_LEVELS = ['none', 'read', 'write', 'admin'];

// Only original labels are translated; edited catalogue entries remain project data.
export const BUILTIN_FUNCTIONS = [
  ['pm', 'PM / Product Owner', 'Project planning and acceptance'],
  ['analyst', 'Analyst', 'Requirements and acceptance criteria'],
  ['designer', 'UI / UX Designer', 'Interface and experience design'],
  ['developer', 'Developer', 'Implementation and code review'],
  ['tester', 'Tester', 'Manual and automated testing'],
  ['security', 'Security', 'Security review and vulnerability triage'],
  ['devops', 'DevOps', 'Environments and delivery integrations'],
  ['release_manager', 'Release Manager', 'Release planning and approval'],
  ['observer', 'Observer / Client', 'Progress and published results'],
].map(([functionId, name, description]) => ({ functionId, name, description, builtin: true }));

export function accessField(value, key) {
  return value?.[key] ?? value?.[key.replace(/_([a-z])/g, (_, letter) => letter.toUpperCase())];
}

export function allowsArea(access, area, minimum = 'read') {
  const required = PERMISSION_LEVELS.indexOf(minimum);
  if (!accessField(access, 'has_access') || required < 1) return false;
  if (access.archived && required > 1) return false;
  const grant = access.areas?.find((entry) => entry.area === area);
  return grant?.enabled === true && PERMISSION_LEVELS.indexOf(grant.level) >= required;
}

export function canCreateTask(access) {
  return accessField(access, 'has_access') === true && accessField(access, 'can_create_tasks') === true && !access.archived;
}

export function projectTabs(access) {
  if (!accessField(access, 'has_access')) return [];
  const tabs = ['overview'];
  for (const area of ['knowledge', 'tests', 'tasks', 'chat']) {
    if (allowsArea(access, area) || (area === 'tasks' && allowsArea(access, 'board'))) tabs.push(area);
  }
  if (allowsArea(access, 'knowledge')) tabs.push('connections');
  tabs.push('members');
  if (allowsArea(access, 'settings')) tabs.push('settings');
  return tabs;
}

export function catalogueLabel(definition, field, translate) {
  const id = accessField(definition, 'function_id');
  const original = BUILTIN_FUNCTIONS.find((entry) => entry.functionId === id);
  const text = String(definition?.[field] ?? '');
  return definition?.builtin && original?.[field] === text
    ? translate(`function_${id}_${field}`)
    : text;
}

export function memberGroup(member) {
  if (member.active !== true) return 'expired';
  return accessField(member, 'expires_at') ? 'temporary' : 'people';
}

export function expiryToInstant(localValue, now = Date.now()) {
  if (!localValue) return null;
  const date = new Date(localValue);
  if (!Number.isFinite(date.getTime()) || date.getTime() <= now) throw new RangeError('Expiry must be in the future');
  return date.toISOString();
}

export function expiryToLocal(value) {
  if (!value) return '';
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return '';
  const pad = (number) => String(number).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}
