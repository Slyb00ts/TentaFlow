// =============================================================================
// File: lidar-decode-worker.js
// Opis: Off-main-thread decode of canonical LiDAR/depth frames. The render path
//   was bottlenecked by `decodeLidarFrame` (i16→world reconstruction + Float32Array
//   marshalling, ~8 ms/frame) running on the main thread and stalling rAF. This
//   worker owns its OWN protocol-wasm instance, decodes a frame's raw bytes, and
//   ships the world-XYZ Float32Array back zero-copy (transferable), so the main
//   thread only uploads to the GPU. Latest-wins drop happens on the main side.
// =============================================================================
import initWasm, { decodeLidarFrame } from '../protocol/wasm_glue.js';

let ready = false;
const queued = [];

initWasm()
  .then(() => {
    ready = true;
    for (const m of queued) decode(m);
    queued.length = 0;
  })
  .catch((e) => {
    self.postMessage({ fatal: String((e && e.message) || e) });
  });

self.onmessage = (e) => {
  if (ready) decode(e.data);
  else queued.push(e.data);
};

function decode({ id, streamKey, bytes }) {
  let f;
  try {
    f = decodeLidarFrame(new Uint8Array(bytes));
  } catch (err) {
    self.postMessage({ id, streamKey, hasFrame: false, error: String((err && err.message) || err) });
    return;
  }
  if (!f || !(f.hasFrame ?? f.has_frame)) {
    self.postMessage({ id, streamKey, hasFrame: false });
    return;
  }
  // `points` is a standalone Float32Array (the wasm decode does a bulk
  // `Float32Array::from`, NOT a view into wasm memory) — its buffer is transferable.
  const points = f.points instanceof Float32Array ? f.points : null;
  self.postMessage(
    {
      id,
      streamKey,
      hasFrame: true,
      points,
      pointCount: Number(f.pointCount ?? f.point_count ?? 0),
      frameSeq: Number(f.frameSeq ?? f.frame_seq ?? 0),
      timestampUs: Number(f.timestampUs ?? f.timestamp_us ?? 0),
      hostSendUs: Number(f.hostSendUs ?? f.host_send_us ?? 0),
    },
    points ? [points.buffer] : [],
  );
}
