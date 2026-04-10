// =============================================================================
// Plik: silero_vad.rs
// Opis: Silero VAD — pure Rust forward pass bez ort.
//
// Architektura (widoczna z inspekcji silero_vad.onnx):
//   1. STFT jako Conv1d: forward_basis_buffer [258, 1, 256]
//      258 = 2*129 (real+imag), kernel=256, stride=?, padding=?
//   2. Encoder — 4× (Conv1d + ReLU):
//      encoder.0: [128, 129, 3]
//      encoder.1: [64, 128, 3]
//      encoder.2: [64, 64, 3]
//      encoder.3: [128, 64, 3]
//   3. Decoder LSTM: hidden=128, input=128
//      weight_ih [512, 128], weight_hh [512, 128], bias_ih/hh [512]
//   4. Decoder output Conv1d: [1, 128, 1] + bias [1] → sigmoid → probability
//
// UWAGA o weryfikacji: ten plik implementuje pipeline, ale dokladne
// parametry stride/padding dla STFT i magnitude calculation zostana potwierdzone
// empirycznie przez porownanie z ort reference output.
// =============================================================================

use crate::error::{VoiceError, VoiceResult};
use crate::onnx_loader::OnnxWeights;
use crate::ops::{
    conv1d_simd, linear_bias, relu_inplace, sigmoid_scalar, Conv1dParams, LstmCell, LstmState,
};

const SAMPLE_RATE: u32 = 16000;
const CHUNK_SIZE: usize = 512;
const STFT_WINDOW: usize = 256;
const STFT_FFT_BINS: usize = 129; // 256/2 + 1
const HIDDEN_SIZE: usize = 128;

/// Prefix tensorow dla 16kHz branch — Silero VAD ma If node wybierajacy
/// miedzy 8kHz i 16kHz pipeline. Inspekcja pokazuje prefix "If_0_then_branch/..."
const TENSOR_PREFIX: &str = "If_0_then_branch/If_0_then_branch__Inline_0__";

/// Silero VAD — pure Rust forward pass
pub struct SileroVad {
    // STFT jako Conv1d [258, 1, 256]
    stft_weight: Vec<f32>,

    // Encoder layers (4× Conv1d + ReLU)
    encoder: [EncoderLayer; 4],

    // LSTM hidden = 128
    lstm: LstmCell,

    // Decoder output Conv1d [1, 128, 1] + bias
    decoder_conv_weight: Vec<f32>, // [1 * 128 * 1]
    decoder_conv_bias: f32,
}

struct EncoderLayer {
    weight: Vec<f32>,
    bias: Vec<f32>,
    in_channels: usize,
    out_channels: usize,
    kernel_size: usize,
    stride: usize,
}

impl SileroVad {
    /// Laduje model Silero z pliku .onnx
    pub fn from_file(path: &str) -> VoiceResult<Self> {
        let weights = OnnxWeights::load(path)?;
        tracing::info!("Silero VAD: {} tensorow zaladowanych", weights.len());

        // Pobierz STFT basis (Conv1d 258x1x256 — imag+real bins)
        let stft_name = format!("{}stft.forward_basis_buffer", TENSOR_PREFIX);
        let stft_t = weights.get(&stft_name)?;
        stft_t.expect_shape(&[258, 1, 256])?;
        let stft_weight = stft_t.data.clone();

        // Encoder: 4 warstwy — stride per warstwa wynika z dump_graph:
        // enc0: stride=1, enc1: stride=2, enc2: stride=2, enc3: stride=1
        let encoder = [
            load_encoder_layer(&weights, 0, 128, 129, 1)?,
            load_encoder_layer(&weights, 1, 64, 128, 2)?,
            load_encoder_layer(&weights, 2, 64, 64, 2)?,
            load_encoder_layer(&weights, 3, 128, 64, 1)?,
        ];

        // LSTM — official Silero VAD uzywa natywnego ONNX LSTM operatora.
        // Wagi sa wynikami Unsqueeze nodes (Constants):
        //   /Unsqueeze_7 = W [1, 4*H, in]   - input weights
        //   /Unsqueeze_8 = R [1, 4*H, H]    - recurrence weights
        //   /Unsqueeze_9 = B [1, 8*H]       - bias (W_bias + R_bias)
        //
        // ONNX LSTM gate order: [i, o, f, g]
        // PyTorch (nasz LstmCell) order: [i, f, g, o]
        // Trzeba spermutowac bloki po H elementow.
        let w_onnx = weights
            .get(&format!("{}/Unsqueeze_7_output_0_subg_96_sub_graph2", TENSOR_PREFIX))?
            .data
            .clone();
        let r_onnx = weights
            .get(&format!("{}/Unsqueeze_8_output_0_subg_96_sub_graph2", TENSOR_PREFIX))?
            .data
            .clone();
        let b_onnx = weights
            .get(&format!("{}/Unsqueeze_9_output_0_subg_96_sub_graph2", TENSOR_PREFIX))?
            .data
            .clone();

        let lstm_w_ih = permute_lstm_gates_2d(&w_onnx, HIDDEN_SIZE, HIDDEN_SIZE);
        let lstm_w_hh = permute_lstm_gates_2d(&r_onnx, HIDDEN_SIZE, HIDDEN_SIZE);
        // ONNX bias [8H] = [Wb_iofg, Rb_iofg], rozdziel i przepermutuj
        let mut bw = b_onnx[..4 * HIDDEN_SIZE].to_vec();
        let mut br = b_onnx[4 * HIDDEN_SIZE..].to_vec();
        let lstm_b_ih = permute_lstm_gates_1d(&bw, HIDDEN_SIZE);
        let lstm_b_hh = permute_lstm_gates_1d(&br, HIDDEN_SIZE);
        let _ = (&mut bw, &mut br); // used above

        let lstm = LstmCell::new(HIDDEN_SIZE, HIDDEN_SIZE, lstm_w_ih, lstm_w_hh, lstm_b_ih, lstm_b_hh);

        // Decoder conv output
        let dec_w = weights
            .get(&format!("{}decoder.decoder.2.weight", TENSOR_PREFIX))?
            .data
            .clone();
        let dec_b = weights
            .get(&format!("{}decoder.decoder.2.bias", TENSOR_PREFIX))?
            .data[0];

        Ok(Self {
            stft_weight,
            encoder,
            lstm,
            decoder_conv_weight: dec_w,
            decoder_conv_bias: dec_b,
        })
    }

    pub fn sample_rate() -> u32 {
        SAMPLE_RATE
    }
    pub fn chunk_size() -> usize {
        CHUNK_SIZE
    }

    /// Forward pass: 512 probek → probability
    ///
    /// Pipeline:
    ///   1. Conv1D z stft_weight (258 → [129 real, 129 imag]) → magnitude [129, T]
    ///   2. 4× Conv1d + ReLU encoder
    ///   3. LSTM per-timestep
    ///   4. Decoder conv + sigmoid
    pub fn forward(&mut self, samples: &[f32], state: &mut LstmState) -> VoiceResult<f32> {
        if samples.len() != CHUNK_SIZE {
            return Err(VoiceError::InvalidInput(format!(
                "Silero VAD: oczekuje {} probek, dostal {}",
                CHUNK_SIZE,
                samples.len()
            )));
        }

        // --- KROK 1: STFT jako Conv1d ---
        // Input: [1, 512], weight: [258, 1, 256], stride=128, padding=0
        // Output: [258, out_t] gdzie out_t = (512 - 256) / 128 + 1 = 3
        let stft_params = Conv1dParams {
            in_channels: 1,
            out_channels: 258,
            kernel_size: STFT_WINDOW,
            stride: 128,
            padding: 0,
            dilation: 1,
        };
        let stft_out_len = stft_params.output_length(CHUNK_SIZE);
        let mut stft_out = vec![0.0_f32; 258 * stft_out_len];
        conv1d_simd(samples, &self.stft_weight, None, &stft_params, CHUNK_SIZE, &mut stft_out);

        // Magnitude: |z| = sqrt(real^2 + imag^2)
        // Pierwsze 129 kanalow = real, kolejne 129 = imag
        let mut magnitude = vec![0.0_f32; STFT_FFT_BINS * stft_out_len];
        for bin in 0..STFT_FFT_BINS {
            for t in 0..stft_out_len {
                let real = stft_out[bin * stft_out_len + t];
                let imag = stft_out[(bin + STFT_FFT_BINS) * stft_out_len + t];
                magnitude[bin * stft_out_len + t] = (real * real + imag * imag).sqrt();
            }
        }

        // --- KROK 2: Encoder (4× Conv1d + ReLU) ---
        let mut current = magnitude;
        let mut current_len = stft_out_len;

        for layer in &self.encoder {
            let params = Conv1dParams {
                in_channels: layer.in_channels,
                out_channels: layer.out_channels,
                kernel_size: layer.kernel_size,
                stride: layer.stride,
                padding: 1, // pads=[1,1] dla wszystkich encoder layers
                dilation: 1,
            };
            let out_len = params.output_length(current_len);
            let mut out = vec![0.0_f32; layer.out_channels * out_len];
            conv1d_simd(&current, &layer.weight, Some(&layer.bias), &params, current_len, &mut out);
            relu_inplace(&mut out);
            current = out;
            current_len = out_len;
        }

        // current shape: [128, current_len]

        // --- KROK 3: LSTM per timestep ---
        // Wejscie LSTM to feature vector [128] per timestep.
        // Wyniki: h za ostatni timestep (standard LSTM output)
        let mut frame = vec![0.0_f32; HIDDEN_SIZE];
        for t in 0..current_len {
            for c in 0..HIDDEN_SIZE {
                frame[c] = current[c * current_len + t];
            }
            self.lstm.step(&frame, state);
        }

        // --- KROK 4: Decoder output (linear projection + sigmoid) ---
        // decoder_conv: [1, 128, 1] — to jest matvec na state.h
        let mut logit = [0.0_f32; 1];
        linear_bias(
            &self.decoder_conv_weight,
            &[self.decoder_conv_bias],
            &state.h,
            HIDDEN_SIZE,
            1,
            &mut logit,
        );
        Ok(sigmoid_scalar(logit[0]))
    }

    /// Lista wszystkich nazw tensorow (dla debugowania)
    #[allow(dead_code)]
    pub fn debug_tensor_names(weights: &OnnxWeights) -> Vec<String> {
        let mut names: Vec<String> = weights.names().iter().map(|s| s.to_string()).collect();
        names.sort();
        names
    }
}

/// Permutuje gate order [i, o, f, g] (ONNX) na [i, f, g, o] (PyTorch).
/// Wagi maja shape [4*H, X], podzielone na 4 bloki [H, X] per gate.
fn permute_lstm_gates_2d(onnx: &[f32], hidden: usize, x_dim: usize) -> Vec<f32> {
    let block = hidden * x_dim;
    let i = &onnx[0..block];
    let o = &onnx[block..2 * block];
    let f = &onnx[2 * block..3 * block];
    let g = &onnx[3 * block..4 * block];
    let mut out = Vec::with_capacity(4 * block);
    out.extend_from_slice(i);
    out.extend_from_slice(f);
    out.extend_from_slice(g);
    out.extend_from_slice(o);
    out
}

/// Permutuje 1D bias [4*H] z [i,o,f,g] do [i,f,g,o]
fn permute_lstm_gates_1d(onnx: &[f32], hidden: usize) -> Vec<f32> {
    let i = &onnx[0..hidden];
    let o = &onnx[hidden..2 * hidden];
    let f = &onnx[2 * hidden..3 * hidden];
    let g = &onnx[3 * hidden..4 * hidden];
    let mut out = Vec::with_capacity(4 * hidden);
    out.extend_from_slice(i);
    out.extend_from_slice(f);
    out.extend_from_slice(g);
    out.extend_from_slice(o);
    out
}

fn load_encoder_layer(
    weights: &OnnxWeights,
    idx: usize,
    out_channels: usize,
    in_channels: usize,
    stride: usize,
) -> VoiceResult<EncoderLayer> {
    let w_name = format!("{}encoder.{}.reparam_conv.weight", TENSOR_PREFIX, idx);
    let b_name = format!("{}encoder.{}.reparam_conv.bias", TENSOR_PREFIX, idx);
    let w = weights.get(&w_name)?;
    w.expect_shape(&[out_channels, in_channels, 3])?;
    let b = weights.get(&b_name)?;
    b.expect_shape(&[out_channels])?;
    Ok(EncoderLayer {
        weight: w.data.clone(),
        bias: b.data.clone(),
        in_channels,
        out_channels,
        kernel_size: 3,
        stride,
    })
}

/// Wysoko-poziomowy wrapper: utrzymuje persistent LSTM state miedzy chunkami
pub struct SileroVadStreaming {
    model: SileroVad,
    state: LstmState,
}

impl SileroVadStreaming {
    pub fn from_file(path: &str) -> VoiceResult<Self> {
        Ok(Self {
            model: SileroVad::from_file(path)?,
            state: LstmState::zeros(HIDDEN_SIZE),
        })
    }

    /// Przetwarza 512 probek — state jest utrzymywany miedzy wywolaniami
    pub fn predict(&mut self, samples: &[f32]) -> VoiceResult<f32> {
        self.model.forward(samples, &mut self.state)
    }

    /// Reset LSTM state (nowy meeting/stream)
    pub fn reset(&mut self) {
        self.state = LstmState::zeros(HIDDEN_SIZE);
    }
}
