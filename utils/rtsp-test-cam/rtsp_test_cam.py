#!/usr/bin/env python3
# =============================================================================
# File: rtsp_test_cam.py
# Purpose: Standalone test RTSP camera. Scans the directory it is run from for
#          video files and streams them in a loop over RTSP, so TentaVision (or
#          any RTSP client) has a stable fake camera to ingest. Self-contained:
#          fetches the mediamtx RTSP server binary on first run; ffmpeg does the
#          decode/normalize/encode and publishes the looped playlist to it.
# Usage:   cd /folder/with/videos && python3 rtsp_test_cam.py [--port 8554]
#                                                             [--path cam]
# =============================================================================
import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import tarfile
import time
import urllib.request
from pathlib import Path

VIDEO_EXTS = {".mp4", ".mkv", ".mov", ".avi", ".webm", ".m4v", ".flv", ".ts", ".mpg", ".mpeg", ".wmv"}
APP_DIR = Path(__file__).resolve().parent
TOOLS_DIR = APP_DIR / ".tools"


def log(msg):
    print(f"[rtsp-test-cam] {msg}", flush=True)


def find_videos(folder: Path):
    vids = sorted(p for p in folder.iterdir() if p.is_file() and p.suffix.lower() in VIDEO_EXTS)
    return vids


def ensure_mediamtx() -> Path:
    """Return path to a mediamtx binary, downloading it once into .tools/ if absent."""
    binary = TOOLS_DIR / "mediamtx"
    if binary.exists():
        return binary
    TOOLS_DIR.mkdir(parents=True, exist_ok=True)
    log("mediamtx not found locally — fetching latest release…")
    api = "https://api.github.com/repos/bluenviron/mediamtx/releases/latest"
    with urllib.request.urlopen(api, timeout=20) as r:
        rel = json.load(r)
    tag = rel["tag_name"]
    asset = f"mediamtx_{tag}_linux_amd64.tar.gz"
    url = f"https://github.com/bluenviron/mediamtx/releases/download/{tag}/{asset}"
    tgz = TOOLS_DIR / asset
    log(f"downloading {url}")
    urllib.request.urlretrieve(url, tgz)
    with tarfile.open(tgz, "r:gz") as t:
        t.extract("mediamtx", TOOLS_DIR)
    tgz.unlink(missing_ok=True)
    binary.chmod(0o755)
    log(f"mediamtx ready at {binary}")
    return binary


def write_mediamtx_config(port: int) -> Path:
    """Minimal config: RTSP only on the chosen port; every other protocol disabled
    so nothing clashes with other local services."""
    cfg = TOOLS_DIR / "mediamtx.yml"
    cfg.write_text(
        "logLevel: warn\n"
        f"rtspAddress: :{port}\n"
        "rtmp: false\n"
        "hls: false\n"
        "webrtc: false\n"
        "srt: false\n"
        "api: false\n"
        "metrics: false\n"
        "pprof: false\n"
        "paths:\n"
        "  all_others:\n"
    )
    return cfg


def write_playlist(videos) -> Path:
    pl = TOOLS_DIR / "playlist.txt"
    # concat demuxer format; absolute paths require -safe 0.
    lines = []
    for v in videos:
        esc = str(v.resolve()).replace("'", "'\\''")
        lines.append(f"file '{esc}'")
    pl.write_text("\n".join(lines) + "\n")
    return pl


def wait_for_port(port: int, timeout=10.0) -> bool:
    import socket

    deadline = time.time() + timeout
    while time.time() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.settimeout(0.5)
            if s.connect_ex(("127.0.0.1", port)) == 0:
                return True
        time.sleep(0.2)
    return False


def ffmpeg_cmd(playlist: Path, port: int, path: str, width: int, height: int, fps: int):
    target = f"rtsp://127.0.0.1:{port}/{path}"
    vf = (
        f"scale={width}:{height}:force_original_aspect_ratio=decrease,"
        f"pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,format=yuv420p,fps={fps}"
    )
    return [
        "ffmpeg", "-hide_banner", "-loglevel", "warning",
        "-re", "-stream_loop", "-1",
        "-fflags", "+genpts+igndts",
        "-f", "concat", "-safe", "0", "-i", str(playlist),
        "-vf", vf,
        "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency",
        "-g", str(fps * 2), "-pix_fmt", "yuv420p", "-fps_mode", "cfr",
        "-an",
        "-f", "rtsp", "-rtsp_transport", "tcp", target,
    ]


def main():
    ap = argparse.ArgumentParser(description="Loop local video files as an RTSP test camera.")
    ap.add_argument("--dir", default=".", help="folder to scan for videos (default: current dir)")
    ap.add_argument("--port", type=int, default=8554, help="RTSP port (default 8554)")
    ap.add_argument("--path", default="cam", help="RTSP mount path (default 'cam')")
    ap.add_argument("--width", type=int, default=1280)
    ap.add_argument("--height", type=int, default=720)
    ap.add_argument("--fps", type=int, default=25)
    args = ap.parse_args()

    if not shutil.which("ffmpeg"):
        log("ERROR: ffmpeg not found in PATH.")
        sys.exit(1)

    folder = Path(args.dir).resolve()
    videos = find_videos(folder)
    if not videos:
        log(f"ERROR: no video files found in {folder} (looked for {sorted(VIDEO_EXTS)}).")
        sys.exit(1)
    log(f"found {len(videos)} video(s) in {folder}:")
    for v in videos:
        log(f"  • {v.name}")

    binary = ensure_mediamtx()
    cfg = write_mediamtx_config(args.port)
    playlist = write_playlist(videos)

    procs = []

    def shutdown(*_):
        log("shutting down…")
        for p in procs:
            if p.poll() is None:
                p.terminate()
        for p in procs:
            try:
                p.wait(timeout=4)
            except subprocess.TimeoutExpired:
                p.kill()
        sys.exit(0)

    signal.signal(signal.SIGINT, shutdown)
    signal.signal(signal.SIGTERM, shutdown)

    log(f"starting mediamtx (RTSP :{args.port})…")
    mtx = subprocess.Popen([str(binary), str(cfg)])
    procs.append(mtx)
    if not wait_for_port(args.port):
        log("ERROR: mediamtx did not open the RTSP port.")
        shutdown()

    url = f"rtsp://<this-host>:{args.port}/{args.path}"
    log("=" * 60)
    log(f"RTSP test camera live → {url}")
    log(f"  local: rtsp://127.0.0.1:{args.port}/{args.path}")
    log("=" * 60)

    # Run ffmpeg; if it ever exits (bad file, transient error) restart it so the
    # camera stays up. Ctrl-C exits cleanly via the signal handler.
    while True:
        log("publishing looped playlist via ffmpeg…")
        ff = subprocess.Popen(ffmpeg_cmd(playlist, args.port, args.path, args.width, args.height, args.fps))
        procs.append(ff)
        ff.wait()
        if mtx.poll() is not None:
            log("mediamtx exited — stopping.")
            shutdown()
        log("ffmpeg stopped; restarting in 2s…")
        time.sleep(2)


if __name__ == "__main__":
    main()
