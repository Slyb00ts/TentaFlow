// =============================================================================
// File: tf-robot-view.js — live 3D robot (real articulated Go2 model).
// Purpose: show the Go2 quadruped articulated from the live joint angles
// published in telemetry (`joints[12]`, order FR/FL/RR/RL × hip/thigh/calf) +
// base orientation (rpy). The mesh is the real Go2 URDF (assets/go2/go2.urdf)
// assembled from decimated glTF link meshes; the URDF kinematics (origins, axes,
// parent/child) drive the joint hierarchy. If the model fails to load we fall
// back to a procedural box quadruped so the view never blanks. Joint targets are
// interpolated each frame so the ~1 Hz telemetry looks smooth.
//
// Usage:  const v = document.createElement('tf-robot-view');
//         v.setPose({ joints: [12 rad], rpy: [roll, pitch, yaw] });
// =============================================================================

import * as THREE from '/js/vendor/three.module.min.js';
import { GLTFLoader } from '/js/vendor/GLTFLoader.module.js';

const URDF_URL = '/assets/go2/go2.urdf';
// URDF mesh refs look like `package://go2_robot_sdk/dae/<part>.dae`; the decimated
// glTF equivalents live next to the URDF as `/assets/go2/<part>.glb`.
const MESH_BASE = '/assets/go2/';

// Telemetry joint layout: [hip, thigh, calf] per leg, legs in order FR/FL/RR/RL.
// Each entry maps a leg's three telemetry slots to the URDF revolute joint names.
const LEG_JOINTS = [
  { prefix: 'FR', base: 0 },
  { prefix: 'FL', base: 3 },
  { prefix: 'RR', base: 6 },
  { prefix: 'RL', base: 9 },
];
const JOINT_KINDS = ['hip', 'thigh', 'calf'];

// Procedural fallback geometry (metres) — approximate, enough to read posture.
const BODY_LEN = 0.38;
const BODY_WID = 0.12;
const BODY_HT = 0.10;
const HIP_DX = BODY_LEN / 2;
const HIP_DY = BODY_WID / 2 + 0.02;
const L_THIGH = 0.213;
const L_CALF = 0.213;
// x: front=+1 back=-1 ; y: left=+1 right=-1.
const FALLBACK_LEGS = [
  { name: 'FR', sx: +1, sy: -1 },
  { name: 'FL', sx: +1, sy: +1 },
  { name: 'RR', sx: -1, sy: -1 },
  { name: 'RL', sx: -1, sy: +1 },
];

// One shared loader for all instances.
const gltfLoader = new GLTFLoader();

// Parse the URDF text into a flat link/joint description we can assemble.
function parseUrdf(xmlText) {
  const doc = new DOMParser().parseFromString(xmlText, 'application/xml');
  if (doc.querySelector('parsererror')) throw new Error('URDF XML parse error');

  const toNums = (s) => (s || '').trim().split(/\s+/).map(Number);
  const links = new Map();
  for (const el of doc.querySelectorAll('robot > link')) {
    const name = el.getAttribute('name');
    const visual = el.querySelector('visual');
    let mesh = null;
    if (visual) {
      const meshEl = visual.querySelector('geometry > mesh');
      const vorigin = visual.querySelector('origin');
      if (meshEl) {
        const file = meshEl.getAttribute('filename') || '';
        // Strip `package://.../dae/` down to the bare part, swap .dae → .glb.
        const part = file.replace(/^.*\//, '').replace(/\.dae$/i, '.glb');
        mesh = {
          url: MESH_BASE + part,
          xyz: vorigin ? toNums(vorigin.getAttribute('xyz')) : [0, 0, 0],
          rpy: vorigin ? toNums(vorigin.getAttribute('rpy')) : [0, 0, 0],
        };
      }
    }
    links.set(name, { name, mesh });
  }

  const joints = [];
  for (const el of doc.querySelectorAll('robot > joint')) {
    const origin = el.querySelector('origin');
    const axis = el.querySelector('axis');
    joints.push({
      name: el.getAttribute('name'),
      type: el.getAttribute('type'),
      parent: el.querySelector('parent')?.getAttribute('link'),
      child: el.querySelector('child')?.getAttribute('link'),
      xyz: origin ? toNums(origin.getAttribute('xyz')) : [0, 0, 0],
      rpy: origin ? toNums(origin.getAttribute('rpy')) : [0, 0, 0],
      axis: axis ? toNums(axis.getAttribute('xyz')) : [1, 0, 0],
    });
  }
  return { links, joints };
}

// Load one glTF and return its scene group (already in metres / real scale).
function loadGlb(url) {
  return new Promise((resolve, reject) => {
    gltfLoader.load(url, (gltf) => resolve(gltf.scene), undefined, reject);
  });
}

class TfRobotView extends HTMLElement {
  constructor() {
    super();
    this._joints = new Array(12).fill(0);
    this._target = new Array(12).fill(0);
    this._rpy = [0, 0, 0];
    this._rpyTarget = [0, 0, 0];
    // Map of telemetry index → { node, axis(THREE.Vector3), rest(THREE.Quaternion) }.
    this._jointNodes = [];
    // Fallback leg node refs (only populated when the URDF model fails to load).
    this._fallbackLegs = [];
    this._usingFallback = false;
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
    this._loadModel();
  }

  disconnectedCallback() {
    cancelAnimationFrame(this._raf);
    this._ro?.disconnect();
    // Three.js never frees geometries/materials/textures on its own, and this
    // element is recreated on every offline/online cycle, so dispose them here.
    this._disposeScene();
    this._renderer?.dispose?.();
  }

  // Walk the scene and release GPU/CPU resources held by meshes loaded for the
  // real model or built for the fallback (geometries, materials, textures).
  _disposeScene() {
    const seenMat = new Set();
    this._scene?.traverse((obj) => {
      obj.geometry?.dispose?.();
      const mats = Array.isArray(obj.material) ? obj.material : (obj.material ? [obj.material] : []);
      for (const m of mats) {
        if (seenMat.has(m)) continue;
        seenMat.add(m);
        for (const v of Object.values(m)) {
          if (v && v.isTexture) v.dispose();
        }
        m.dispose?.();
      }
    });
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
    const fill = new THREE.DirectionalLight(0xa9b9ff, 0.4);
    fill.position.set(-1, 1, -1);
    scene.add(fill);

    // Ground grid for spatial reference.
    const grid = new THREE.GridHelper(3, 24, 0x33408a, 0x1a2050);
    grid.position.y = -0.34;
    scene.add(grid);

    // Robot root, yawed in world space. Unitree body frame is x-forward, y-left,
    // z-up; Three.js is y-up, so the body group is rotated -90° about X to map
    // robot-z → world-y. The URDF model and the procedural fallback both live
    // under `_body` and so share this frame mapping.
    const root = new THREE.Group();
    scene.add(root);
    this._root = root;
    const body = new THREE.Group();
    body.rotation.x = -Math.PI / 2;
    root.add(body);
    this._body = body;

    const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
    renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
    renderer.setSize(w, h);
    renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.appendChild(renderer.domElement);
    this._renderer = renderer;
    this._scene = scene;

    this._ro = new ResizeObserver(() => this._resize());
    this._ro.observe(this);
  }

  async _loadModel() {
    try {
      const res = await fetch(URDF_URL);
      if (!res.ok) throw new Error(`URDF fetch ${res.status}`);
      const { links, joints } = parseUrdf(await res.text());
      await this._buildFromUrdf(links, joints);
    } catch (err) {
      // Real model unavailable → keep the view usable with the box quadruped.
      console.warn('[tf-robot-view] real Go2 model load failed, using fallback:', err);
      this._buildFallback();
    }
  }

  // Assemble the URDF kinematic tree as a Three.js Object3D hierarchy. Each joint
  // becomes a Group positioned by its <origin>; the child link's visual mesh is
  // parented under it. Revolute leg joints are recorded so setPose() can rotate
  // them about their <axis>.
  async _buildFromUrdf(links, joints) {
    const childrenOf = new Map();
    for (const j of joints) {
      if (!childrenOf.has(j.parent)) childrenOf.set(j.parent, []);
      childrenOf.get(j.parent).push(j);
    }

    // Find the kinematic root: a link that is never a joint child.
    const childLinkNames = new Set(joints.map((j) => j.child));
    let rootLink = null;
    for (const name of links.keys()) {
      if (!childLinkNames.has(name)) { rootLink = name; break; }
    }
    if (!rootLink) rootLink = 'base_link';

    // Revolute-joint name → telemetry index, for the 12 driven leg joints.
    const jointToIndex = new Map();
    for (const { prefix, base } of LEG_JOINTS) {
      JOINT_KINDS.forEach((kind, k) => {
        jointToIndex.set(`${prefix}_${kind}_joint`, base + k);
      });
    }

    const modelRoot = new THREE.Group();
    const jointNodes = [];
    const pending = [];

    const attachVisual = (group, link) => {
      if (!link?.mesh) return;
      pending.push(
        loadGlb(link.mesh.url).then((mesh) => {
          mesh.position.set(...link.mesh.xyz);
          mesh.rotation.set(...link.mesh.rpy);
          group.add(mesh);
        })
      );
    };

    // Depth-first build from the root link. `parentGroup` is the Three.js node the
    // current link's frame is expressed in.
    const buildLink = (linkName, parentGroup) => {
      const link = links.get(linkName);
      attachVisual(parentGroup, link);
      for (const j of childrenOf.get(linkName) || []) {
        const jointGroup = new THREE.Group();
        jointGroup.name = j.name;
        jointGroup.position.set(...j.xyz);
        jointGroup.rotation.set(...j.rpy);
        parentGroup.add(jointGroup);

        if (j.type === 'revolute' && jointToIndex.has(j.name)) {
          jointNodes[jointToIndex.get(j.name)] = {
            node: jointGroup,
            axis: new THREE.Vector3(...j.axis).normalize(),
            rest: jointGroup.quaternion.clone(),
          };
        }
        buildLink(j.child, jointGroup);
      }
    };

    buildLink(rootLink, modelRoot);
    await Promise.all(pending);

    if (jointNodes.filter(Boolean).length < 12) {
      throw new Error('URDF missing expected leg joints');
    }

    this._body.add(modelRoot);
    this._model = modelRoot;
    this._jointNodes = jointNodes;
    this._usingFallback = false;
  }

  // Procedural box quadruped — identical posture semantics to the real model so
  // joint motion still reads correctly when the glTF assets are unavailable.
  _buildFallback() {
    const legMat = new THREE.MeshStandardMaterial({ color: 0x818cf8, metalness: 0.2, roughness: 0.5 });
    const calfMat = new THREE.MeshStandardMaterial({ color: 0xa78bfa, metalness: 0.2, roughness: 0.5 });
    const bodyMat = new THREE.MeshStandardMaterial({ color: 0x2a3170, metalness: 0.3, roughness: 0.6 });

    const trunk = new THREE.Mesh(new THREE.BoxGeometry(BODY_LEN, BODY_WID, BODY_HT), bodyMat);
    this._body.add(trunk);
    const head = new THREE.Mesh(new THREE.BoxGeometry(0.07, 0.08, 0.06), calfMat);
    head.position.set(BODY_LEN / 2 + 0.03, 0, 0.02);
    this._body.add(head);

    const segGeo = (len) => {
      const g = new THREE.BoxGeometry(0.035, 0.035, len);
      g.translate(0, 0, -len / 2); // pivot at top, extends down -z
      return g;
    };

    // Build one fallback leg per corner and record per-joint refs keyed by the
    // telemetry index so the animation loop drives both paths identically.
    this._fallbackLegs = FALLBACK_LEGS.map((leg, li) => {
      const hip = new THREE.Group();
      hip.position.set(leg.sx * HIP_DX, leg.sy * HIP_DY, 0);
      this._body.add(hip);
      const thigh = new THREE.Group();
      hip.add(thigh);
      thigh.add(new THREE.Mesh(segGeo(L_THIGH), legMat));
      const calf = new THREE.Group();
      calf.position.set(0, 0, -L_THIGH);
      thigh.add(calf);
      calf.add(new THREE.Mesh(segGeo(L_CALF), calfMat));
      return { hip, thigh, calf, leg, base: li * 3 };
    });
    this._usingFallback = true;
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

    // Base orientation from rpy (roll x, pitch y, yaw z in robot frame).
    this._root.rotation.set(0, this._rpy[2], 0); // yaw around world-up
    this._body.rotation.x = -Math.PI / 2 + this._rpy[1];
    this._body.rotation.z = this._rpy[0];

    // Slow auto-orbit so the user sees it in 3D without controls.
    this._spin += 0.003;
    const r = 1.25;
    this._cam.position.set(Math.cos(this._spin) * r, 0.6, Math.sin(this._spin) * r);
    this._cam.lookAt(0, 0.02, 0);

    this._applyJoints();
    this._renderer.render(this._scene, this._cam);
  }

  // Drive the active model from the smoothed joint angles. The real model rotates
  // each URDF joint about its declared axis (composed onto the joint's rest
  // rotation); the fallback uses fixed axis conventions per segment.
  _applyJoints() {
    if (this._usingFallback) {
      this._fallbackLegs.forEach((n) => {
        const hip = this._joints[n.base + 0];
        const thigh = this._joints[n.base + 1];
        const calf = this._joints[n.base + 2];
        n.hip.rotation.x = hip * n.leg.sy; // abduction, mirrored per side
        n.thigh.rotation.y = thigh;        // pitch
        n.calf.rotation.y = calf;          // knee
      });
      return;
    }
    const jn = this._jointNodes;
    if (!jn.length) return;
    const q = this._scratchQuat || (this._scratchQuat = new THREE.Quaternion());
    for (let i = 0; i < 12; i++) {
      const e = jn[i];
      if (!e) continue;
      // joint = rest * rotation(axis, angle)
      q.setFromAxisAngle(e.axis, this._joints[i]);
      e.node.quaternion.copy(e.rest).multiply(q);
    }
  }
}

customElements.define('tf-robot-view', TfRobotView);
export { TfRobotView };
