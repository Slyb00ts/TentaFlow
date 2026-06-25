# rtsp-test-cam

Standalone test RTSP camera for TentaVision (or any RTSP client). Scans the folder
it is run from for video files and streams them in a loop over RTSP.

## Usage

```bash
cd /folder/with/videos
python3 /home/critix/rtsp-test-cam/rtsp_test_cam.py
# → rtsp://<this-host>:8554/cam
```

Options: `--dir <folder>` (default: current dir), `--port 8554`, `--path cam`,
`--width 1280 --height 720 --fps 25`.

## How it works

- Finds video files (mp4/mkv/mov/avi/webm/…) in the target folder, sorted.
- Fetches the `mediamtx` RTSP server binary once into `.tools/` (needs internet on
  first run only).
- `ffmpeg` reads the playlist with `-stream_loop -1`, normalizes every clip to a
  uniform H264 / yuv420p / fixed resolution+fps (so mixed-format files just work)
  and publishes it to the local mediamtx mount. Clients read the stream from there.

## Add it in TentaVision

TentaVision → **Kamery** → **Dodaj kamerę** → **Strumień RTSP/RTSPS** →
URL `rtsp://127.0.0.1:8554/cam` → Testuj połączenie → name → Zakończ. The loop
shows up under **Live view**.

## Run as a background service (optional)

```bash
systemd-run --user --unit=rtsp-testcam --working-directory=/path/to/videos \
  python3 /home/critix/rtsp-test-cam/rtsp_test_cam.py
systemctl --user stop rtsp-testcam   # stop
```
