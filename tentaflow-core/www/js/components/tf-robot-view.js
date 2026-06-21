// =============================================================================
// File: tf-robot-view.js — live 3D robot (interim Three.js renderer).
// Purpose: show a quadruped (Go2) articulated from the live joint angles published
// in telemetry (`joints[12]`, order FR/FL/RR/RL × hip/thigh/calf) + base
// orientation (rpy). INTERIM: a procedural box-model, replaced later by the real
// Rust+wgpu+WASM renderer (UNIFIED_SLAM_ARCHITECTURE §16). Joint targets are
// interpolated each frame so the ~1 Hz telemetry looks smooth.
//
// Usage:  const v = document.createElement('tf-robot-view');
//         v.setPose({ joints: [12 rad], rpy: [roll, pitch, yaw] });
// =============================================================================

import * as THREE from '/js/vendor/three.module.min.js';

// Go2-ish link geometry (metres). Approximate — enough to read posture/motion.
const BODY_LEN = 0.38;
const BODY_WID = 0.12;
const BODY_HT = 0.10;
const HIP_DX = BODY_LEN / 2;     // front/back hip offset (x = forward)
const HIP_DY = BODY_WID / 2 + 0.02; // left/right hip offset (y = left)
const L_THIGH = 0.21;
const L_CALF = 0.21;

// Leg index → corner sign. Order: FR, FL, RR, RL (each hip/thigh/calf).
// x: front=+1 back=-1 ; y: left=+1 right=-1.
const LEGS = [
  { name: 'FR', sx: +1, sy: -1 },
  { name: 'FL', sx: +1, sy: +1 },
  { name: 'RR', sx: -1, sy: -1 },
  { name: 'RL', sx: -1, sy: +1 },
];

class TfRobotView extends HTMLElement {
  constructor() {
    super();
    this._joints = new Array(12).fill(0);
    this._target = new Array(12).fill(0);
    this._rpy = [0, 0, 0];
    this._rpyTarget = [0, 0, 0];
    this._legNodes = [];
    this._raf = 0;
    this._spin = 0;
  }

  connectedCallback() {
    if (this._inited) return;
    this._inited = true;
    this.style.display = 'block';
    this.style.position = 'relative';
    this._initScene();
    this._loop();
  }

  disconnectedCallback() {
    cancelAnimationFrame(this._raf);
    this._ro?.disconnect();
    this._renderer?.dispose?.();
  }

  /** Feed a pose: { joints:[12] radians, rpy:[roll,pitch,yaw] radians }. */
  setPose(pose) {
    if (Array.isArray(pose?.joints) && pose.joints.length >= 12) {
      for (let i = 0; i < 12; i++) this._target[i] = Number(pose.joints[i]) || 0;
    }
    if (Array.isArray(pose?.rpy) && pose.rpy.length >= 3) {
      this._rpyTarget = pose.rpy.map((v) => Number(v) || 0);
    }
  }

  _initScene() {
    const w = this.clientWidth || 360;
    const h = this.clientHeight || 260;
    const scene = new THREE.Scene();
    scene.background = new THREE.Color(0x0a0e22);

    const cam = new THREE.PerspectiveCamera(45, w / h, 0.05, 100);
    cam.position.set(0.9, 0.6, 0.9);
    cam.lookAt(0, 0.05, 0);
    this._cam = cam;

    scene.add(new THREE.HemisphereLight(0xbfd0ff, 0x202040, 1.1));
    const key = new THREE.DirectionalLight(0xffffff, 1.0);
    key.position.set(1, 2, 1);
    scene.add(key);

    // Ground grid for spatial reference.
    const grid = new THREE.GridHelper(3, 24, 0x33408a, 0x1a2050);
    grid.position.y = -0.34;
    scene.add(grid);

    // Robot root. Unitree body frame is x-forward, y-left, z-up; Three.js is y-up,
    // so rotate the body group -90° about X to map robot-z → world-y.
    const root = new THREE.Group();
    scene.add(root);
    this._root = root;
    const body = new THREE.Group();
    body.rotation.x = -Math.PI / 2;
    root.add(body);
    this._body = body;

    const bodyMat = new THREE.MeshStandardMaterial({ color: 0x2a3170, metalness: 0.3, roughness: 0.6 });
    const legMat = new THREE.MeshStandardMaterial({ color: 0x818cf8, metalness: 0.2, roughness: 0.5 });
    const calfMat = new THREE.MeshStandardMaterial({ color: 0xa78bfa, metalness: 0.2, roughness: 0.5 });

    const trunk = new THREE.Mesh(new THREE.BoxGeometry(BODY_LEN, BODY_WID, BODY_HT), bodyMat);
    body.add(trunk);
    // "head" marker (forward = +x).
    const head = new THREE.Mesh(new THREE.BoxGeometry(0.07, 0.08, 0.06), calfMat);
    head.position.set(BODY_LEN / 2 + 0.03, 0, 0.02);
    body.add(head);

    const segGeo = (len) => {
      const g = new THREE.BoxGeometry(0.035, 0.035, len);
      g.translate(0, 0, -len / 2); // pivot at top, extends down -z
      return g;
    };

    this._legNodes = LEGS.map((leg) => {
      // hip joint (abduction, rotates about body x).
      const hip = new THREE.Group();
      hip.position.set(leg.sx * HIP_DX, leg.sy * HIP_DY, 0);
      body.add(hip);
      // thigh joint (pitch about y), segment hangs down.
      const thigh = new THREE.Group();
      hip.add(thigh);
      thigh.add(new THREE.Mesh(segGeo(L_THIGH), legMat));
      // calf joint at the end of the thigh.
      const calf = new THREE.Group();
      calf.position.set(0, 0, -L_THIGH);
      thigh.add(calf);
      calf.add(new THREE.Mesh(segGeo(L_CALF), calfMat));
      return { hip, thigh, calf, leg };
    });

    const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
    renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
    renderer.setSize(w, h);
    this.appendChild(renderer.domElement);
    this._renderer = renderer;
    this._scene = scene;

    this._ro = new ResizeObserver(() => this._resize());
    this._ro.observe(this);
  }

  _resize() {
    const w = this.clientWidth || 360;
    const h = this.clientHeight || 260;
    if (!this._renderer) return;
    this._renderer.setSize(w, h, false);
    this._cam.aspect = w / h;
    this._cam.updateProjectionMatrix();
  }

  _loop() {
    this._raf = requestAnimationFrame(() => this._loop());
    // Smooth toward targets (telemetry is ~1 Hz; lerp makes it fluid).
    const a = 0.15;
    for (let i = 0; i < 12; i++) this._joints[i] += (this._target[i] - this._joints[i]) * a;
    for (let i = 0; i < 3; i++) this._rpy[i] += (this._rpyTarget[i] - this._rpy[i]) * a;

    // Base orientation from rpy (roll x, pitch y, yaw z in robot frame → applied on root).
    this._root.rotation.set(0, this._rpy[2], 0); // yaw around world-up; roll/pitch subtle
    this._body.rotation.x = -Math.PI / 2 + this._rpy[1];
    this._body.rotation.z = this._rpy[0];

    // Slow auto-orbit so the user sees it in 3D without controls.
    this._spin += 0.003;
    const r = 1.25;
    this._cam.position.set(Math.cos(this._spin) * r, 0.6, Math.sin(this._spin) * r);
    this._cam.lookAt(0, 0.02, 0);

    // Apply joints: [hip, thigh, calf] per leg, order FR/FL/RR/RL.
    this._legNodes.forEach((n, li) => {
      const hip = this._joints[li * 3 + 0];
      const thigh = this._joints[li * 3 + 1];
      const calf = this._joints[li * 3 + 2];
      n.hip.rotation.x = hip * n.leg.sy; // abduction, mirrored per side
      n.thigh.rotation.y = thigh;        // pitch
      n.calf.rotation.y = calf;          // knee
    });

    this._renderer.render(this._scene, this._cam);
  }
}

customElements.define('tf-robot-view', TfRobotView);
export { TfRobotView };
