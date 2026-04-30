// =============================================================================
// File: modules/vision-pose-live.js
// Description: Cross-platform live camera pose overlay for browser/WebView UI.
// =============================================================================

import { initTransport, ApiBinary } from '/js/protocol/api-binary-shim.js';

export const COCO_POSE_EDGES = [
  [5, 6],
  [5, 7],
  [7, 9],
  [6, 8],
  [8, 10],
  [5, 11],
  [6, 12],
  [11, 12],
  [11, 13],
  [13, 15],
  [12, 14],
  [14, 16],
  [0, 1],
  [0, 2],
  [1, 3],
  [2, 4],
];

const DEFAULTS = {
  facingMode: 'environment',
  captureWidth: 320,
  targetFps: 8,
  jpegQuality: 0.68,
  requestTimeoutMs: 5000,
  minKeypointScore: 0.2,
};

export class LivePoseCamera {
  constructor({
    video,
    overlay,
    serviceName,
    facingMode = DEFAULTS.facingMode,
    captureWidth = DEFAULTS.captureWidth,
    targetFps = DEFAULTS.targetFps,
    jpegQuality = DEFAULTS.jpegQuality,
    requestTimeoutMs = DEFAULTS.requestTimeoutMs,
    minKeypointScore = DEFAULTS.minKeypointScore,
    onResult = null,
    onError = null,
  }) {
    if (!video) throw new Error('LivePoseCamera requires a video element');
    if (!overlay) throw new Error('LivePoseCamera requires an overlay canvas');
    this.video = video;
    this.overlay = overlay;
    this.serviceName = serviceName;
    this.facingMode = facingMode;
    this.captureWidth = captureWidth;
    this.targetFps = targetFps;
    this.jpegQuality = jpegQuality;
    this.requestTimeoutMs = requestTimeoutMs;
    this.minKeypointScore = minKeypointScore;
    this.onResult = onResult;
    this.onError = onError;

    this.stream = null;
    this.running = false;
    this.inflight = false;
    this.frameWidth = 0;
    this.frameHeight = 0;
    this.captureCanvas = document.createElement('canvas');
    this.captureCtx = this.captureCanvas.getContext('2d', { willReadFrequently: false });
    this.overlayCtx = this.overlay.getContext('2d');
    this.boundResize = () => this.resizeOverlay();
    this.resizeObserver = typeof ResizeObserver !== 'undefined'
      ? new ResizeObserver(this.boundResize)
      : null;
  }

  async start() {
    if (this.running) return;
    if (!navigator.mediaDevices?.getUserMedia) {
      throw new Error('Camera capture is not available in this browser context');
    }
    await initTransport();
    this.stream = await navigator.mediaDevices.getUserMedia({
      audio: false,
      video: {
        facingMode: { ideal: this.facingMode },
        width: { ideal: 1280 },
        height: { ideal: 720 },
      },
    });
    this.video.srcObject = this.stream;
    this.video.muted = true;
    this.video.playsInline = true;
    await this.video.play();
    this.running = true;
    this.resizeObserver?.observe(this.video);
    if (!this.resizeObserver) window.addEventListener('resize', this.boundResize);
    this.resizeOverlay();
    this.loop();
  }

  stop() {
    this.running = false;
    this.resizeObserver?.disconnect();
    if (!this.resizeObserver) window.removeEventListener('resize', this.boundResize);
    if (this.stream) {
      for (const track of this.stream.getTracks()) track.stop();
      this.stream = null;
    }
    this.video.srcObject = null;
    this.clearOverlay();
  }

  setServiceName(serviceName) {
    this.serviceName = serviceName;
  }

  async loop() {
    const intervalMs = Math.max(1, Math.floor(1000 / Math.max(1, this.targetFps)));
    while (this.running) {
      const started = performance.now();
      if (!this.inflight) {
        this.captureAndInfer().catch((err) => {
          if (this.onError) this.onError(err);
          else console.warn('[vision-pose-live]', err);
        });
      }
      const elapsed = performance.now() - started;
      await sleep(Math.max(0, intervalMs - elapsed));
    }
  }

  async captureAndInfer() {
    if (!this.serviceName || this.video.readyState < HTMLMediaElement.HAVE_CURRENT_DATA) return;
    const vw = this.video.videoWidth;
    const vh = this.video.videoHeight;
    if (!vw || !vh) return;

    this.inflight = true;
    try {
      const scale = this.captureWidth / vw;
      this.frameWidth = Math.max(1, Math.round(vw * scale));
      this.frameHeight = Math.max(1, Math.round(vh * scale));
      this.captureCanvas.width = this.frameWidth;
      this.captureCanvas.height = this.frameHeight;
      this.captureCtx.drawImage(this.video, 0, 0, this.frameWidth, this.frameHeight);
      const blob = await canvasToBlob(this.captureCanvas, this.jpegQuality);
      const image = new Uint8Array(await blob.arrayBuffer());
      const resp = await ApiBinary.action(
        'visionInferRequest',
        { serviceName: this.serviceName, image },
        { timeoutMs: this.requestTimeoutMs },
      );
      if (resp?.kind === 'poses') {
        this.drawPoses(resp.poses || []);
      } else {
        this.clearOverlay();
      }
      this.onResult?.(resp);
    } finally {
      this.inflight = false;
    }
  }

  resizeOverlay() {
    const rect = this.video.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(1, Math.round(rect.width));
    const h = Math.max(1, Math.round(rect.height));
    this.overlay.style.width = `${w}px`;
    this.overlay.style.height = `${h}px`;
    this.overlay.width = Math.max(1, Math.round(w * dpr));
    this.overlay.height = Math.max(1, Math.round(h * dpr));
  }

  clearOverlay() {
    this.overlayCtx.clearRect(0, 0, this.overlay.width, this.overlay.height);
  }

  drawPoses(poses) {
    this.clearOverlay();
    if (!this.frameWidth || !this.frameHeight) return;

    const dpr = window.devicePixelRatio || 1;
    const metrics = this.displayMetrics(dpr);
    const sx = metrics.width / this.frameWidth;
    const sy = metrics.height / this.frameHeight;
    const ctx = this.overlayCtx;
    ctx.save();
    ctx.setTransform(sx, 0, 0, sy, metrics.x, metrics.y);
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';

    for (const pose of poses) {
      const points = new Map();
      for (const kp of pose.keypoints || []) {
        if ((kp.score ?? 0) >= this.minKeypointScore) points.set(Number(kp.id), kp);
      }

      ctx.lineWidth = 3;
      ctx.strokeStyle = '#54f0a8';
      for (const [a, b] of COCO_POSE_EDGES) {
        const pa = points.get(a);
        const pb = points.get(b);
        if (!pa || !pb) continue;
        ctx.beginPath();
        ctx.moveTo(pa.x, pa.y);
        ctx.lineTo(pb.x, pb.y);
        ctx.stroke();
      }

      ctx.fillStyle = '#ffffff';
      for (const kp of points.values()) {
        ctx.beginPath();
        ctx.arc(kp.x, kp.y, 3.5, 0, Math.PI * 2);
        ctx.fill();
      }
    }

    ctx.restore();
  }

  displayMetrics(dpr) {
    const rect = this.video.getBoundingClientRect();
    const containerWidth = rect.width * dpr;
    const containerHeight = rect.height * dpr;
    const videoRatio = this.frameWidth / this.frameHeight;
    const containerRatio = containerWidth / containerHeight;
    if (containerRatio > videoRatio) {
      const height = containerHeight;
      const width = height * videoRatio;
      return { x: (containerWidth - width) / 2, y: 0, width, height };
    }
    const width = containerWidth;
    const height = width / videoRatio;
    return { x: 0, y: (containerHeight - height) / 2, width, height };
  }
}

function canvasToBlob(canvas, quality) {
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => {
      if (blob) resolve(blob);
      else reject(new Error('Failed to encode camera frame'));
    }, 'image/jpeg', quality);
  });
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
