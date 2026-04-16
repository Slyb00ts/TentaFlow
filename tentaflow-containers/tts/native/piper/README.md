# Piper TTS (placeholder)

Planowane: Piper TTS native — neuronowy ONNX TTS dzialajacy na Raspberry Pi.

Status: PLANNED. Manifest w `tts/_services/piper.toml` ma wszystkie warianty
oznaczone jako `status = "planned"`.

## Jak dokonczyc

1. Dodaj `build.sh` z krokami:
   - Pobierz prebuilt binarki Piper z https://github.com/rhasspy/piper/releases
   - Lub zbuduj ze zrodel (cmake + ONNX Runtime)
   - Skopiuj artefakty do `output/piper-<platform>/`
2. Zaktualizuj `_services/piper.toml` zmieniajac `status = "stable"`.
