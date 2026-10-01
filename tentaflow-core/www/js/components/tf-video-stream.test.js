// ============ File: tf-video-stream.test.js — Recorded native media mode preserves live-stream configuration boundaries ============

import test, { beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import { window, I18n, cleanBody } from '../lib/actions/_test-setup.js';
import { ApiBinary } from '../protocol/api-binary-shim.js';
import './tf-video-stream.js';

let subscriptionCalls = 0;
ApiBinary.subscribe = async () => { subscriptionCalls += 1; throw new Error('Recorded video must not subscribe to a live stream'); };
beforeEach(() => { cleanBody(); subscriptionCalls = 0; });
after(async () => { cleanBody(); await window.happyDOM.close(); });

test('recorded src uses native controls, metadata preload, audio and exposed media element', () => {
  const player = document.createElement('tf-video-stream');
  player.setAttribute('src', 'https://localhost/__project_attachment/recording/movie.mp4');
  player.setAttribute('controls', '');
  document.body.appendChild(player);
  assert.equal(player.mediaElement.tagName, 'VIDEO');
  assert.equal(player.mediaElement.controls, true);
  assert.equal(player.mediaElement.autoplay, false);
  assert.equal(player.mediaElement.muted, false);
  assert.equal(player.mediaElement.preload, 'metadata');
  assert.equal(player.mediaElement.style.objectFit, 'contain');
  assert.equal(subscriptionCalls, 0);
  player.removeAttribute('controls');
  assert.equal(player.mediaElement.controls, false);
});

test('ambiguous live and recorded sources are rejected before reading either source', () => {
  const player = document.createElement('tf-video-stream');
  player.setAttribute('src', 'https://localhost/movie.mp4');
  player.setAttribute('stream-id', 'camera:one');
  const errors = [];
  player.addEventListener('media-error', (event) => errors.push(event.detail));
  document.body.appendChild(player);
  assert.deepEqual(errors, [{ code: 'ambiguous_source' }]);
  assert.equal(player.mediaElement.getAttribute('src'), null);
  assert.match(player.shadowRoot.textContent, new RegExp(I18n.t('project_studio.attachment_video_source_error')));
  assert.equal(subscriptionCalls, 0);
});

test('changing or detaching recorded video unloads the previous native source', () => {
  const player = document.createElement('tf-video-stream');
  player.setAttribute('src', 'https://localhost/first.mp4');
  document.body.appendChild(player);
  let unloaded = 0;
  player.mediaElement.load = () => { unloaded += 1; assert.equal(player.mediaElement.getAttribute('src'), null); };
  player.setAttribute('src', 'https://localhost/second.mp4');
  assert.equal(unloaded, 1);
  assert.equal(player.mediaElement.src, 'https://localhost/second.mp4');
  player.remove();
  assert.equal(unloaded, 2);
  assert.equal(player.mediaElement.getAttribute('src'), null);
  assert.equal(subscriptionCalls, 0);
});

test('recorded native playback errors are available to the existing feature error controls', () => {
  const player = document.createElement('tf-video-stream');
  player.setAttribute('src', 'https://localhost/movie.mp4');
  document.body.appendChild(player);
  let code;
  player.addEventListener('media-error', (event) => { code = event.detail.code; });
  Object.defineProperty(player.mediaElement, 'error', { value: { code: 4 } });
  player.mediaElement.dispatchEvent(new Event('error'));
  assert.equal(code, 4);
  assert.equal(subscriptionCalls, 0);
});

test('live configuration keeps muted autoplay and the existing no-MSE error without a native src', () => {
  const player = document.createElement('tf-video-stream');
  player.setAttribute('stream-id', 'camera:one');
  document.body.appendChild(player);
  assert.equal(player.mediaElement.autoplay, true);
  assert.equal(player.mediaElement.muted, true);
  assert.equal(player.mediaElement.controls, false);
  assert.equal(player.mediaElement.preload, 'auto');
  assert.equal(player.mediaElement.getAttribute('src'), null);
  assert.match(player.shadowRoot.textContent, /MediaSource Extensions/);
});
