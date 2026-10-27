// ============ File: project-studio-tree.js — Authorized project tree and task-scope preferences ============

export function projectAncestors(project, projects, breadcrumbs) {
  const names = new Map([...breadcrumbs, ...projects].map((row) => [row.projectId, row]));
  return project.path.split('/').filter(Boolean).slice(0, -1).map((id) => names.get(id)).filter(Boolean);
}

export function projectRoots(projects) {
  const ids = new Set(projects.map((project) => project.projectId));
  return projects.filter((project) => !ids.has(project.parentId));
}

export function projectTreeNodes(projects, breadcrumbs, decorate = () => ({})) {
  const nodes = new Map();
  const referenced = new Set(projects.flatMap((project) => project.path.split('/').filter(Boolean)));
  for (const row of breadcrumbs.filter((row) => referenced.has(row.projectId))) nodes.set(row.projectId, { id: row.projectId, label: row.name, disabled: true, children: [] });
  for (const project of projects) nodes.set(project.projectId, { id: project.projectId, label: project.name, children: [], ...decorate(project) });
  const parents = new Map();
  for (const project of projects) {
    const path = project.path.split('/').filter(Boolean);
    for (let i = 1; i < path.length; i++) if (nodes.has(path[i]) && nodes.has(path[i - 1])) parents.set(path[i], path[i - 1]);
  }
  const roots = [];
  for (const [id, node] of nodes) {
    const parent = nodes.get(parents.get(id));
    if (parent) parent.children.push(node);
    else roots.push(node);
  }
  const sort = (rows) => { rows.sort((a, b) => a.label.localeCompare(b.label)); for (const row of rows) sort(row.children); };
  sort(roots);
  return roots;
}

export function readTaskScope(userId, projectId, view, hasChildren) {
  try {
    const value = window.localStorage.getItem(`ps.tasks.scope.${userId}.${projectId}.${view}`);
    if (value === 'single' || value === 'descendants') return value;
  } catch { /* private storage may be unavailable */ }
  return hasChildren ? 'descendants' : 'single';
}

export function writeTaskScope(userId, projectId, view, scope) {
  try { window.localStorage.setItem(`ps.tasks.scope.${userId}.${projectId}.${view}`, scope); }
  catch { /* the active view remains usable without persistence */ }
}
