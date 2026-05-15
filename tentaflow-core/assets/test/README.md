# Test Fixtures

Binary test assets are **not** committed to git. Regenerate locally.

## `sample_traffic.mp4`

5-minute 720p30 synthetic video (ffmpeg `testsrc` color bars + 440 Hz sine
audio). Used by the camera ingest test harness (`fake_file` vendor, M1.W6).

```bash
ffmpeg -y -f lavfi -i "testsrc=duration=300:size=1280x720:rate=30" \
  -f lavfi -i "sine=frequency=440:duration=300" \
  -c:v libx264 -preset veryfast -crf 28 -pix_fmt yuv420p \
  -c:a aac -b:a 128k \
  tentaflow-core/assets/test/sample_traffic.mp4
```

Expected size: ~7-15 MB. If you tweak duration/resolution keep the output
under 30 MB so the dev experience stays snappy.
