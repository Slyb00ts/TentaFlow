// ============ File: face-speech.js - Acoustic cues shared by chat and meeting avatars. ============
export class FaceSpeechAnalyser {
  constructor(analyser) {
    this.analyser = analyser;
    analyser.fftSize = 1024;
    analyser.smoothingTimeConstant = 0.25;
    this.waveform = new Float32Array(analyser.fftSize);
    this.spectrum = new Float32Array(analyser.frequencyBinCount);
    this.features = { rms: 0, level: 0, round: 0, wide: 0, fricative: 0 };
  }

  read() {
    const { analyser, waveform, spectrum, features } = this;
    analyser.getFloatTimeDomainData(waveform);
    analyser.getFloatFrequencyData(spectrum);
    let sum = 0;
    for (const value of waveform) sum += value * value;
    features.rms = Math.sqrt(sum / waveform.length);
    features.level = 0.9 * (1 - Math.exp(-Math.max(0, features.rms - 0.008) * 5));
    let low = 0, mid = 0, high = 0;
    const hzPerBin = analyser.context.sampleRate / analyser.fftSize;
    for (let i = 1; i < spectrum.length; i++) {
      const hz = i * hzPerBin;
      if (hz < 180 || hz > 8000) continue;
      const power = 10 ** (spectrum[i] / 10);
      if (hz < 900) low += power;
      else if (hz < 2800) mid += power;
      else high += power;
    }
    const total = low + mid + high + 1e-12;
    // Spectral balance approximates lip shape; it does not identify phonemes.
    features.round = Math.max(0, Math.min(0.5, (low / total - 0.65) * 1.5));
    features.wide = Math.max(0, Math.min(1, (mid / total - 0.2) * 1.7));
    features.fricative = Math.max(0, Math.min(1, (high / total - 0.12) * 2.5));
    return features;
  }
}
