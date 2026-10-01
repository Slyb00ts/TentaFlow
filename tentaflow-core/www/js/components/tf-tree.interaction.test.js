// ============ File: tf-tree.interaction.test.js — Controlled tree movement and inline header actions ============

import { window } from '../sdk-runtime/_dom-test-harness.js';
import test, { after, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { TfTree } from './tf-tree.js';
import { TfDetailHeader } from './tf-detail-header.js';

afterEach(() => document.body.replaceChildren());
after(async () => window.happyDOM.close());

function mountTree() {
  const tree = new TfTree();
  document.body.appendChild(tree);
  tree.nodes = [
    { id: 'root', label: 'Root', draggable: true, droppable: true, children: [
      { id: 'child', label: 'Child', draggable: true, droppable: true },
    ] },
    { id: 'target', label: 'Target', droppable: true },
    { id: 'denied', label: 'Name only', disabled: true, droppable: true, draggable: true },
  ];
  tree.expandedIds = ['root'];
  return tree;
}

function dragEvent(type, dataTransfer) {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, 'dataTransfer', { value: dataTransfer });
  return event;
}

test('tree starts with one keyboard tab stop and retains focus after controlled expansion', () => {
  const tree = mountTree();
  const row = tree.querySelector('[data-node-id="root"] > .tf-tree__row');
  assert.equal(tree.querySelectorAll('.tf-tree__row[tabindex="0"]').length, 1);
  row.focus();
  tree.expandedIds = [];
  assert.equal(document.activeElement, tree.querySelector('[data-node-id="root"] > .tf-tree__row'));
  assert.equal(document.activeElement.getAttribute('tabindex'), '0');
});

test('tree emits a permitted move without mutating controlled nodes', () => {
  const tree = mountTree();
  const transfer = { setData(type, id) { this.type = type; this.id = id; } };
  let intent;
  tree.addEventListener('move', (event) => { intent = event.detail; });
  tree.querySelector('[data-node-id="child"] > .tf-tree__row').dispatchEvent(dragEvent('dragstart', transfer));
  const target = tree.querySelector('[data-node-id="target"] > .tf-tree__row');
  const over = dragEvent('dragover', transfer);
  target.dispatchEvent(over);
  assert.equal(over.defaultPrevented, true);
  assert.ok(target.classList.contains('tf-tree__row--drop-target'));
  target.dispatchEvent(dragEvent('drop', transfer));
  assert.deepEqual(intent, { id: 'child', parentId: 'target' });
  assert.equal(transfer.type, 'application/x-tf-tree-node');
  assert.equal(transfer.effectAllowed, 'move');
  assert.equal(tree.nodes[0].children[0].id, 'child');
  assert.ok(!target.classList.contains('tf-tree__row--drop-target'));
});

test('tree rejects cycles, name-only targets, external drops and disabled drags', () => {
  const tree = mountTree();
  const transfer = { setData() {} };
  const moves = [];
  tree.addEventListener('move', (event) => moves.push(event.detail));
  tree.querySelector('[data-node-id="root"] > .tf-tree__row').dispatchEvent(dragEvent('dragstart', transfer));
  tree.querySelector('[data-node-id="child"] > .tf-tree__row').dispatchEvent(dragEvent('drop', transfer));
  tree.querySelector('[data-node-id="child"] > .tf-tree__row').dispatchEvent(dragEvent('dragstart', transfer));
  tree.querySelector('[data-node-id="denied"] > .tf-tree__row').dispatchEvent(dragEvent('drop', transfer));
  tree.querySelector('[data-node-id="target"] > .tf-tree__row').dispatchEvent(dragEvent('drop', transfer));
  const blocked = dragEvent('dragstart', transfer);
  tree.querySelector('[data-node-id="denied"] > .tf-tree__row').dispatchEvent(blocked);
  assert.equal(blocked.defaultPrevented, true);
  tree.querySelector('[data-node-id="child"] > .tf-tree__row').dispatchEvent(dragEvent('dragstart', transfer));
  tree.nodes = [{ id: 'child', label: 'Access revoked', disabled: true, draggable: true }, { id: 'target', label: 'Target', droppable: true }];
  tree.querySelector('[data-node-id="target"] > .tf-tree__row').dispatchEvent(dragEvent('drop', transfer));
  assert.deepEqual(moves, []);
});

test('inline tree actions do not select their row', () => {
  const tree = new TfTree();
  document.body.appendChild(tree);
  const action = document.createElement('tf-button');
  action.textContent = 'Move';
  tree.nodes = [{ id: 'root', label: 'Root', actions: action }];
  let selected = false;
  tree.addEventListener('select', () => { selected = true; });
  action.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  assert.equal(selected, false);
  assert.equal(tree.querySelector('.tf-tree__actions').firstElementChild, action);
});

test('detail header retains title picker beside the existing title and status', () => {
  const header = new TfDetailHeader();
  header.setAttribute('title', 'Current project');
  header.innerHTML = '<span slot="status">Active</span><span slot="title-actions"><tf-button>Change project</tf-button></span><span slot="actions">Members</span>';
  document.body.appendChild(header);
  assert.equal(header.querySelector('.tf-detail-title').textContent, 'Current project');
  assert.equal(header.querySelector('.tf-detail-top-row tf-button').textContent, 'Change project');
  assert.equal(header.querySelector('.tf-detail-actions').textContent, 'Members');
});
