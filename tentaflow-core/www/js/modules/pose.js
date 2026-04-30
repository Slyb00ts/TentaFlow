// =============================================================================
// File: modules/pose.js — Live camera pose overlay app.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { byId, escapeHtml, toast } from '/js/utils.js';
import { LivePoseCamera } from '/js/modules/vision-pose-live.js';

const STORAGE_MODEL_KEY = 'tentaflow_pose_model';
const STORAGE_CAMERA_KEY = 'tentaflow_pose_camera';
const STORAGE_FPS_KEY = 'tentaflow_pose_fps';
const POSE_MODEL_IDS = new Set(['movenet-lightning-singlepose', 'yolov8n-pose-coco']);
const POSE_ENGINE_IDS = new Set(['movenet-lightning', 'yolov8n-pose']);
const DEFAULT_MODEL_ID = 'yolov8n-pose-coco';

let camera = null;
let poseModels = [];
let state = {
  model: '',
  camera: localStorage.getItem(STORAGE_CAMERA_KEY) || 'environment',
  fps: localStorage.getItem(STORAGE_FPS_KEY) || '8',
  running: false,
  lastLatencyMs: 0,
  lastPoseCount: 0,
  error: '',
};

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function isPoseModel(model) {
  const caps = Array.isArray(model.capabilities) ? model.capabilities : [];
  return caps.includes('vision') && (
    POSE_MODEL_IDS.has(model.model_name) ||
    POSE_ENGINE_IDS.has(model.engine_id)
  );
}

function formatModelLabel(model) {
  const base = model.display_name || model.model_name;
  return model.engine_id ? `${base} (${model.engine_id})` : base;
}

function renderModelOptions() {
  const selected = state.model || poseModels[0]?.model_name || '';
  return poseModels.map((m) => {
    const value = escapeHtml(m.model_name);
    const label = escapeHtml(formatModelLabel(m));
    const selectedAttr = m.model_name === selected ? ' selected' : '';
    return `<option value="${value}"${selectedAttr}>${label}</option>`;
  }).join('');
}

function updateControls() {
  const start = byId('pose-start');
  const stop = byId('pose-stop');
  const model = byId('pose-model');
  const cameraSelect = byId('pose-camera');
  const fps = byId('pose-fps');
  const empty = byId('pose-empty');
  const live = byId('pose-live');
  const status = byId('pose-status');
  const latency = byId('pose-latency');
  const count = byId('pose-count');

  if (model) {
    const inner = model.querySelector('select');
    if (inner) {
      inner.innerHTML = renderModelOptions();
      inner.value = state.model || poseModels[0]?.model_name || '';
      model.setAttribute('value', inner.value);
      state.model = inner.value;
    }
    model.toggleAttribute('disabled', state.running || poseModels.length === 0);
  }
  cameraSelect?.toggleAttribute('disabled', state.running);
  fps?.toggleAttribute('disabled', state.running);
  start?.toggleAttribute('disabled', state.running || poseModels.length === 0);
  stop?.toggleAttribute('disabled', !state.running);
  empty?.classList.toggle('hidden', poseModels.length > 0);
  live?.classList.toggle('pose-live-running', state.running);

  if (status) {
    status.textContent = state.running
      ? I18n.t('pose.status_running')
      : state.error || I18n.t('pose.status_idle');
  }
  if (latency) latency.textContent = state.lastLatencyMs ? `${Math.round(state.lastLatencyMs)} ms` : '—';
  if (count) count.textContent = String(state.lastPoseCount);
}

async function loadPoseModels() {
  const list = await ApiBinary.list('modelListRequest', { arrayKey: 'models' });
  poseModels = (Array.isArray(list) ? list : [])
    .filter(isPoseModel)
    .sort((a, b) => {
      if (a.model_name === DEFAULT_MODEL_ID) return -1;
      if (b.model_name === DEFAULT_MODEL_ID) return 1;
      return formatModelLabel(a).localeCompare(formatModelLabel(b));
    });
  const saved = localStorage.getItem(STORAGE_MODEL_KEY);
  state.model = poseModels.some((m) => m.model_name === saved)
    ? saved
    : poseModels.find((m) => m.model_name === DEFAULT_MODEL_ID)?.model_name || poseModels[0]?.model_name || '';
  updateControls();
}

function handleCameraError(err) {
  state.running = false;
  state.error = err?.message || I18n.t('pose.camera_error');
  toast(state.error, 'error');
  updateControls();
}

async function startCamera() {
  if (!state.model || state.running) return;
  state.error = '';
  state.lastLatencyMs = 0;
  state.lastPoseCount = 0;
  updateControls();

  const video = byId('pose-video');
  const overlay = byId('pose-overlay');
  const fps = Number.parseInt(state.fps, 10) || 8;
  camera = new LivePoseCamera({
    video,
    overlay,
    serviceName: state.model,
    facingMode: state.camera,
    targetFps: fps,
    captureWidth: state.model === 'movenet-lightning-singlepose' ? 256 : 384,
    requestTimeoutMs: 4500,
    onResult(resp) {
      state.lastLatencyMs = resp?.latencyMs ?? 0;
      state.lastPoseCount = Array.isArray(resp?.poses) ? resp.poses.length : 0;
      updateControls();
    },
    onError(err) {
      state.error = err?.message || I18n.t('pose.inference_error');
      updateControls();
    },
  });

  try {
    await camera.start();
    state.running = true;
    updateControls();
  } catch (err) {
    camera = null;
    handleCameraError(err);
  }
}

function stopCamera() {
  camera?.stop();
  camera = null;
  state.running = false;
  updateControls();
}

const PoseScreen = {
  get title() { return I18n.t('pose.title'); },

  render() {
    return `
      <div class="pose-shell">
        <section class="pose-live" id="pose-live">
          <video id="pose-video" autoplay playsinline muted></video>
          <canvas id="pose-overlay" aria-hidden="true"></canvas>
          <div class="pose-empty" id="pose-empty">
            <div class="pose-empty-icon">${sprite('image')}</div>
            <h2>${escapeHtml(I18n.t('pose.no_model_title'))}</h2>
            <p>${escapeHtml(I18n.t('pose.no_model_desc'))}</p>
          </div>
        </section>
        <aside class="pose-panel">
          <div class="pose-heading">
            <div>
              <h1>${escapeHtml(I18n.t('pose.title'))}</h1>
              <p>${escapeHtml(I18n.t('pose.subtitle'))}</p>
            </div>
            <tf-chip status="info">${escapeHtml(I18n.t('pose.live_chip'))}</tf-chip>
          </div>

          <div class="pose-control-stack">
            <label class="pose-label" for="pose-model">${escapeHtml(I18n.t('pose.model_label'))}</label>
            <tf-select id="pose-model"></tf-select>

            <label class="pose-label" for="pose-camera">${escapeHtml(I18n.t('pose.camera_label'))}</label>
            <tf-select id="pose-camera" value="${escapeHtml(state.camera)}">
              <option value="user">${escapeHtml(I18n.t('pose.camera_front'))}</option>
              <option value="environment">${escapeHtml(I18n.t('pose.camera_back'))}</option>
            </tf-select>

            <label class="pose-label" for="pose-fps">${escapeHtml(I18n.t('pose.fps_label'))}</label>
            <tf-select id="pose-fps" value="${escapeHtml(state.fps)}">
              <option value="5">5 FPS</option>
              <option value="8">8 FPS</option>
              <option value="12">12 FPS</option>
            </tf-select>
          </div>

          <div class="pose-actions">
            <tf-button variant="primary" icon="play" id="pose-start">${escapeHtml(I18n.t('pose.start'))}</tf-button>
            <tf-button variant="danger" icon="stop" id="pose-stop" disabled>${escapeHtml(I18n.t('pose.stop'))}</tf-button>
          </div>

          <div class="pose-stats">
            <div>
              <span>${escapeHtml(I18n.t('pose.status'))}</span>
              <strong id="pose-status">${escapeHtml(I18n.t('pose.status_idle'))}</strong>
            </div>
            <div>
              <span>${escapeHtml(I18n.t('pose.latency'))}</span>
              <strong id="pose-latency">—</strong>
            </div>
            <div>
              <span>${escapeHtml(I18n.t('pose.poses'))}</span>
              <strong id="pose-count">0</strong>
            </div>
          </div>
        </aside>
      </div>
    `;
  },

  async mount() {
    byId('pose-model')?.addEventListener('change', (e) => {
      state.model = e.detail.value || '';
      localStorage.setItem(STORAGE_MODEL_KEY, state.model);
      camera?.setServiceName(state.model);
    });
    byId('pose-camera')?.addEventListener('change', (e) => {
      state.camera = e.detail.value || 'environment';
      localStorage.setItem(STORAGE_CAMERA_KEY, state.camera);
    });
    byId('pose-fps')?.addEventListener('change', (e) => {
      state.fps = e.detail.value || '8';
      localStorage.setItem(STORAGE_FPS_KEY, state.fps);
    });
    byId('pose-start')?.addEventListener('click', startCamera);
    byId('pose-stop')?.addEventListener('click', stopCamera);

    try {
      await loadPoseModels();
    } catch (err) {
      poseModels = [];
      state.error = err?.message || I18n.t('pose.models_error');
      updateControls();
    }
  },

  async unmount() {
    stopCamera();
  },
};

export default PoseScreen;
