# Per-truck ADR/plate separation — staged plan

Problem: the event recorder pools ALL scene detections into one bag → two trucks' plates/ADR/stickers mix.
Fix: a parallel vehicle detector + associate each sign/plate to the vehicle it sits on → per-truck reads.

## Model
YOLOv8n COCO ONNX 640x640, single output `[1,84,8400]` (4 bbox + 80 class, no objectness), input `images [1,3,640,640]` f32 RGB /255 NO ImageNet-normalize. Keep COCO ids {2 car,5 bus,7 truck}. In-process `VehicleDetector` mirroring RfDetrDetector (SessionPool + YOLO decode/NMS via vision/nms.rs) — NOT the onnx_cv `detect` contract (that is RF-DETR-only). Weights in vision_models_dir(), pulled from tentaflow.nextapp.pl like RF-DETR.

## Parallelism (no added frame time)
In each detect spawn closure (vision_analysis.rs ~1488 device / ~1554 nv12 / ~1608 rgb) run RF-DETR and the vehicle forward with tokio::join! — different ort session pools → independent CUDA streams → wall-time ~= max(DETR,YOLO). Vehicle boxes attach to FrameJob.vehicles so association sees the SAME frame. get_vehicle_detector() OnceCell (1-2 sessions, ~tens of MB).

## Association (tail of run_cold_stages)
Per sign detection: center-in-box → tie-break by containment fraction area(s∩v)/area(s), then smallest vehicle area, then vehicle track_id. Fallback max-overlap >= MIN_OVERLAP(0.3). No vehicle → vehicle_id=0 (unassigned, kept for overlay, excluded from per-truck grouping). Stable vehicle_id = the "vehicles" IOU-tracker track_id (tracker::key(camera,"vehicles")). Add vehicle_id:u32 (serde default) to Detection.

## Aggregation + storage (no migration)
EventMeta: flat bag → vehicles: BTreeMap<u32, VehicleMeta> (each = today's classes/texts/stany/best_thumb/tracks/frames). absorb routes by d.vehicle_id, reuses winner()/stany_winners()/event_thumb() per vehicle. to_json emits vehicles:[{vehicle_id,plate,adr,stany,thumb,detection_frames}]. Scalar plate_text/adr_text/thumb_ref columns = the primary vehicle (most frames) → panel row + search unchanged; event_meta.vehicles[] carries the full breakdown.

## Panel
Reuse 2-column Zdjęcie/Odczyty. recording_meta_vehicles() parses vehicles[] (old-shape fallback). Odczyty renders one block per truck when >1 ("Pojazd 1 — Rejestracja/ADR/Nalepki"), single-truck unchanged. Zdjęcie = primary thumb.

## Stages
A VehicleDetector + model + latency bench (gate: YOLO forward < RF-DETR forward).
B parallel tokio::join! launch + publish vehicle boxes (gate: proc_ms unchanged on/off).
C vehicle_id on Detection + "vehicles" tracker + containment association (unit-test tie-breaks; gate: signs on correct truck on fisheye clips).
D EventMeta per-vehicle map + to_json + single-truck scalar back-compat (gate: 2-truck → 2 sets; 1-truck byte-identical).
E panel multi-truck Odczyty (gate: old rows render; 2-truck shows both).

Biggest risk: association accuracy at the fisheye gate (Stage C, isolated testable fn).
