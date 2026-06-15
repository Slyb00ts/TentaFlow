# RTSP fleet simulator

Turns a folder of video files into many looping RTSP camera streams, so the
TentaFlow camera pipeline (ingest + always-on CV analysis) can be load-tested at
fleet scale without physical cameras.

## How it works

- `docker-compose.yml` runs **MediaMTX** (an RTSP server) on `:8554`.
- `simulate.sh` starts one **ffmpeg** process per simulated camera, each looping
  a source file into `rtsp://<host>:8554/<prefix><NN>`.
- H.264 sources are stream-copied (`-c copy`, ~0% CPU per camera), so one host
  can publish many streams. Non-H.264 sources are transcoded.

## Requirements

`docker` (+ `docker compose`), `ffmpeg`, `ffprobe`.

## Usage

```bash
cd tools/rtsp-sim

# 50 cameras from clips in ./clips:
./simulate.sh --videos ./clips --count 50

# 200 cameras, custom host/prefix:
./simulate.sh --videos ./clips --count 200 --prefix cam --host 127.0.0.1

# Force transcode to 25 fps H.264 (non-H.264 sources, or to normalize fps):
./simulate.sh --videos ./clips --count 16 --encode libx264 --fps 25

# Reuse an already-running MediaMTX:
./simulate.sh --videos ./clips --count 10 --no-server
```

The script prints every `rtsp://...` URL — add them in TentaVision as cameras.
`Ctrl-C` stops all publishers (and MediaMTX unless `--no-server`).

## Scaling notes

- With `-c copy`, hundreds of publishers fit on one box; the bottleneck becomes
  network + MediaMTX, not CPU. To push past that, run `simulate.sh` on several
  machines pointing at one MediaMTX (`--no-server --host <mediamtx-ip>`).
- The simulated bitrate equals the source clip's bitrate. Use representative
  footage (resolution/bitrate matching the real cameras) for honest numbers.
- Pair with `cargo run --release --features inference-vision-gpu --example cv_bench`
  to compare the analysis-capacity ceiling against the ingested camera count.
