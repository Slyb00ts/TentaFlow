// =============================================================================
// Plik: WhisperAudio.swift
// Opis: Preprocessing audio do log-mel spectrogramu zgodny 1:1 z
//       mlx-examples/whisper/whisper/audio.py:
//         - PCM mono 16 kHz f32 (po normalizacji int16 / 32768.0)
//         - pad/trim do 30 s (N_SAMPLES = 480_000)
//         - STFT: n_fft=400, hop=160, window=hann
//         - magnitude^2
//         - mel filterbank (n_mels=80 lub 128) liczony przy starcie (Slaney)
//         - log10 z clipem 1e-10
//         - normalizacja: clamp(max - 8.0, mag) -> (mag + 4) / 4
// =============================================================================

import Foundation
import MLX
import MLXFFT

public enum WhisperAudio {
    public static let sampleRate: Int = 16_000
    /// Hop = 160 * 100 = 16k → 1 frame na 10 ms.
    public static let nFFT: Int = 400
    public static let hopLength: Int = 160
    /// 30 s audio = 480 000 sampli.
    public static let nSamples: Int = 480_000
    /// 30 s / 10 ms = 3000 ramek.
    public static let nFrames: Int = 3000

    /// Pad lub przytnij PCM do dokladnie 30s. Whisper wymaga staloskierowanego
    /// okna — krotsze audio dopelniamy zerami, dluzsze trzeba tnac na okna
    /// po stronie wywolujacego (`transcribe` w step 3).
    public static func padOrTrim(_ samples: MLXArray) -> MLXArray {
        let length = samples.dim(0)
        if length > nSamples {
            return samples[0 ..< nSamples]
        }
        if length < nSamples {
            let padding = MLXArray.zeros([nSamples - length], dtype: samples.dtype)
            return MLX.concatenated([samples, padding], axis: 0)
        }
        return samples
    }

    /// Obliczenie log-mel spectrogramu. Wejscie `samples` to MLXArray ksztalt
    /// `(n_samples,)` z PCM f32 znormalizowanym do [-1, 1]. Zwraca tensor
    /// `(n_mels, n_frames)` gotowy do podania na encoder (`mel.expandedDimensions(axis: 0)`
    /// daje batch wymiar po stronie wywolujacego).
    public static func logMelSpectrogram(samples: MLXArray, nMels: Int) -> MLXArray {
        // Pad: reflekcja na lewo i prawo o `n_fft // 2` zeby pierwsza/ostatnia
        // ramka byla wycentrowana wokol probki 0 i N-1 — taki sam padding
        // robi `librosa.stft(center=True)` i Pythonowy whisper.
        let halfFFT = nFFT / 2
        let leftPad = samples[1 ... halfFFT][.stride(by: -1)]
        let rightLen = samples.dim(0)
        let rightPad = samples[(rightLen - halfFFT - 1) ..< (rightLen - 1)][.stride(by: -1)]
        let padded = MLX.concatenated([leftPad, samples, rightPad], axis: 0)

        // Hann window — analityczna formula, identyczna z `np.hanning(nFFT)`
        // ale Whisper uzywa wariantu otwartego: 0.5 * (1 - cos(2π i / (nFFT-1))).
        let hann = hannWindow(length: nFFT)

        // Frame sygnal w okna `n_fft` z hopem. Wykorzystujemy `as_strided`-like
        // przez `MLXArray.indexed` z manualna petla — mlx-swift nie ma jeszcze
        // `as_strided`, wiec stack po przesuniecia. To jest jednorazowa
        // operacja na CPU/GPU dla 30s audio (3000 ramek), wiec narzut z petli
        // jest pomijalny w stosunku do FFT.
        let totalFrames = (padded.dim(0) - nFFT) / hopLength + 1
        var framesList: [MLXArray] = []
        framesList.reserveCapacity(totalFrames)
        for i in 0 ..< totalFrames {
            let start = i * hopLength
            framesList.append(padded[start ..< (start + nFFT)] * hann)
        }
        let frames = MLX.stacked(framesList, axis: 0)  // (T, n_fft)

        // rfft -> (T, n_fft//2 + 1) zespolone. Bierzemy magnitude^2 = re^2 + im^2.
        let spec = MLXFFT.rfft(frames, n: nFFT, axis: -1)
        var magnitudes = MLX.abs(spec).square()  // (T, n_fft//2+1)
        // Centered STFT z reflect-pad daje T = N/hop + 1; whisper.py upuszcza
        // ostatnia ramke (`magnitudes[:-1]`) zeby dostac dokladnie 3000 ramek
        // dla 30s. Robimy to samo, inaczej `nFrames` w encoderze + sinusoidy
        // sie nie zgadzaja.
        if magnitudes.dim(0) == nFrames + 1 {
            magnitudes = magnitudes[0 ..< nFrames]
        }

        // Mel filterbank: (n_mels, n_fft//2+1). Mnozenie z transpozycja
        // magnitudes daje (n_mels, T) — co dokladnie chce encoder po `transposed`.
        let mel = melFilterbank(nMels: nMels, nFFT: nFFT, sampleRate: sampleRate)
        let melSpec = mel.matmul(magnitudes.transposed(1, 0))  // (n_mels, T)

        // Log10 z floorem 1e-10 (zamiast log naturalnego — Whisper standard).
        let logSpec = MLX.log10(MLX.maximum(melSpec, MLXArray(Float(1e-10))))
        // Normalizacja zgodna z whisper.py: clamp od dolu na (max - 8) i
        // przeskalowanie do okolic [-1, 1].
        let maxVal = logSpec.max()
        let clamped = MLX.maximum(logSpec, maxVal - 8.0)
        return (clamped + 4.0) / 4.0
    }

    /// Hann window o danej dlugosci. Identyczne z `np.hanning(N)` (czyli z
    /// zerami na koncach) — `whisper.py` uzywa `torch.hann_window(N, periodic=True)`
    /// czyli `0.5 - 0.5*cos(2π i / N)`. Implementujemy ten wariant.
    private static func hannWindow(length: Int) -> MLXArray {
        let n = MLXArray(0 ..< length).asType(.float32)
        let twoPi = Float.pi * 2
        return 0.5 - 0.5 * MLX.cos(twoPi * n / Float(length))
    }

    /// Slaney mel filterbank: szereg trojkatnych filtrow o powierzchni 1.
    /// Wzor: identyczny z `librosa.filters.mel(sr, n_fft, n_mels, htk=False)`.
    /// HF tokenizer Whispera wymaga DOKLADNIE tej skali — htk=True dawaloby
    /// inne wartosci i model halucynowal.
    private static func melFilterbank(nMels: Int, nFFT: Int, sampleRate: Int) -> MLXArray {
        let nFreqs = nFFT / 2 + 1
        let fMin: Float = 0.0
        let fMax: Float = Float(sampleRate) / 2.0

        // Mel scale (Slaney): linear do 1000 Hz, log powyzej.
        func hzToMel(_ hz: Float) -> Float {
            let fMinSlaney: Float = 0.0
            let fSp: Float = 200.0 / 3.0  // step liniowy dla Hz w skali mel
            let minLogHz: Float = 1000.0
            let minLogMel = (minLogHz - fMinSlaney) / fSp
            let logstep: Float = log(Float(6.4)) / 27.0
            if hz >= minLogHz {
                return minLogMel + log(hz / minLogHz) / logstep
            }
            return (hz - fMinSlaney) / fSp
        }
        func melToHz(_ mel: Float) -> Float {
            let fMinSlaney: Float = 0.0
            let fSp: Float = 200.0 / 3.0
            let minLogHz: Float = 1000.0
            let minLogMel = (minLogHz - fMinSlaney) / fSp
            let logstep: Float = log(Float(6.4)) / 27.0
            if mel >= minLogMel {
                return minLogHz * exp(logstep * (mel - minLogMel))
            }
            return fMinSlaney + fSp * mel
        }

        let melMin = hzToMel(fMin)
        let melMax = hzToMel(fMax)
        var melPoints = [Float](repeating: 0, count: nMels + 2)
        for i in 0 ... (nMels + 1) {
            melPoints[i] = melMin + (melMax - melMin) * Float(i) / Float(nMels + 1)
        }
        let hzPoints = melPoints.map(melToHz)
        // FFT bin frequencies: linspace(0, sr/2, n_freqs).
        var fftFreqs = [Float](repeating: 0, count: nFreqs)
        for k in 0 ..< nFreqs {
            fftFreqs[k] = Float(sampleRate) * Float(k) / Float(nFFT)
        }

        var weights = [Float](repeating: 0, count: nMels * nFreqs)
        for m in 0 ..< nMels {
            let lower = hzPoints[m]
            let center = hzPoints[m + 1]
            let upper = hzPoints[m + 2]
            // Slaney enorm: 2 / (upper - lower) — dzieki temu kazdy filter ma
            // powierzchnie 1, niezaleznie od szerokosci pasma.
            let enorm = 2.0 / (upper - lower)
            for k in 0 ..< nFreqs {
                let f = fftFreqs[k]
                if f < lower || f > upper { continue }
                let w: Float
                if f <= center {
                    w = (f - lower) / (center - lower)
                } else {
                    w = (upper - f) / (upper - center)
                }
                weights[m * nFreqs + k] = w * enorm
            }
        }
        return MLXArray(weights, [nMels, nFreqs])
    }

    /// Wczytuje plik WAV/PCM z dysku do MLXArray f32 znormalizowanego do [-1, 1].
    /// Obsluguje WAV PCM 16-bit mono 16 kHz — caller musi zapewnic ten format
    /// (resampling robi router/audio bridge zanim PCM trafia do nas).
    public static func loadPCM16(url: URL) throws -> MLXArray {
        let data = try Data(contentsOf: url)
        // Skip 44-byte WAV header (uproszczenie — zaklada PCM 16-bit mono 16k).
        // Pelny parser RIFF nie jest potrzebny bo bot wysyla zawsze ten format.
        let headerSize = 44
        guard data.count > headerSize else {
            throw NSError(
                domain: "WhisperAudio", code: 1,
                userInfo: [NSLocalizedDescriptionKey: "WAV file too small: \(data.count) bytes"]
            )
        }
        let pcmData = data.subdata(in: headerSize ..< data.count)
        let sampleCount = pcmData.count / 2
        var samples = [Float](repeating: 0, count: sampleCount)
        pcmData.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            let int16Ptr = raw.bindMemory(to: Int16.self)
            for i in 0 ..< sampleCount {
                samples[i] = Float(int16Ptr[i]) / 32768.0
            }
        }
        return MLXArray(samples)
    }
}
