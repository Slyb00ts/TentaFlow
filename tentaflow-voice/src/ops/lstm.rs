// =============================================================================
// Plik: ops/lstm.rs
// Opis: LSTM cell forward pass — pure Rust + SIMD matvec.
//       Format zgodny z PyTorch/ONNX: W_ih, W_hh, b_ih, b_hh per gate.
//
// LSTM equations (standard):
//   i_t = sigmoid(W_ii @ x + b_ii + W_hi @ h_{t-1} + b_hi)
//   f_t = sigmoid(W_if @ x + b_if + W_hf @ h_{t-1} + b_hf)
//   g_t = tanh(W_ig @ x + b_ig + W_hg @ h_{t-1} + b_hg)
//   o_t = sigmoid(W_io @ x + b_io + W_ho @ h_{t-1} + b_ho)
//   c_t = f_t * c_{t-1} + i_t * g_t
//   h_t = o_t * tanh(c_t)
//
// W_ih to [4*H, I], W_hh to [4*H, H] — wszystkie 4 gates w jednej macierzy.
// =============================================================================

use super::activation::sigmoid_scalar;
use super::matmul::matvec_f32_simd;

/// Stan LSTM: hidden + cell state
#[derive(Debug, Clone)]
pub struct LstmState {
    pub h: Vec<f32>,
    pub c: Vec<f32>,
}

impl LstmState {
    pub fn zeros(hidden_size: usize) -> Self {
        Self {
            h: vec![0.0; hidden_size],
            c: vec![0.0; hidden_size],
        }
    }

    pub fn reset(&mut self) {
        for v in self.h.iter_mut() {
            *v = 0.0;
        }
        for v in self.c.iter_mut() {
            *v = 0.0;
        }
    }
}

/// Pojedyncza komorka LSTM — trzyma wagi + bias, wykonuje jeden forward step.
pub struct LstmCell {
    pub input_size: usize,
    pub hidden_size: usize,

    /// weight_ih: [4 * hidden_size, input_size]  — 4 gates (i, f, g, o)
    pub w_ih: Vec<f32>,
    /// weight_hh: [4 * hidden_size, hidden_size]
    pub w_hh: Vec<f32>,
    /// bias_ih: [4 * hidden_size]
    pub b_ih: Vec<f32>,
    /// bias_hh: [4 * hidden_size]
    pub b_hh: Vec<f32>,

    /// Bufor scratch dla gates (reuse miedzy krokami)
    scratch: Vec<f32>,
}

impl LstmCell {
    pub fn new(
        input_size: usize,
        hidden_size: usize,
        w_ih: Vec<f32>,
        w_hh: Vec<f32>,
        b_ih: Vec<f32>,
        b_hh: Vec<f32>,
    ) -> Self {
        assert_eq!(w_ih.len(), 4 * hidden_size * input_size);
        assert_eq!(w_hh.len(), 4 * hidden_size * hidden_size);
        assert_eq!(b_ih.len(), 4 * hidden_size);
        assert_eq!(b_hh.len(), 4 * hidden_size);

        Self {
            input_size,
            hidden_size,
            w_ih,
            w_hh,
            b_ih,
            b_hh,
            scratch: vec![0.0; 4 * hidden_size],
        }
    }

    /// Wykonuje jeden step LSTM: (x, state) → new_state
    pub fn step(&mut self, x: &[f32], state: &mut LstmState) {
        debug_assert_eq!(x.len(), self.input_size);
        debug_assert_eq!(state.h.len(), self.hidden_size);
        debug_assert_eq!(state.c.len(), self.hidden_size);

        let h = self.hidden_size;

        // scratch = W_ih @ x  (shape: [4*H])
        matvec_f32_simd(&self.w_ih, x, 4 * h, self.input_size, &mut self.scratch);

        // scratch += W_hh @ h_{t-1}
        // Dodajemy do scratch, wiec uzywamy tymczasowego bufora i sumujemy
        let mut hh_out = vec![0.0_f32; 4 * h];
        matvec_f32_simd(&self.w_hh, &state.h, 4 * h, self.hidden_size, &mut hh_out);

        for i in 0..4 * h {
            self.scratch[i] += hh_out[i] + self.b_ih[i] + self.b_hh[i];
        }

        // Gate layout: [i | f | g | o]
        // i_t = sigmoid(scratch[0..H])
        // f_t = sigmoid(scratch[H..2H])
        // g_t = tanh(scratch[2H..3H])
        // o_t = sigmoid(scratch[3H..4H])
        for idx in 0..h {
            let i_gate = sigmoid_scalar(self.scratch[idx]);
            let f_gate = sigmoid_scalar(self.scratch[h + idx]);
            let g_gate = self.scratch[2 * h + idx].tanh();
            let o_gate = sigmoid_scalar(self.scratch[3 * h + idx]);

            // c_t = f * c_{t-1} + i * g
            state.c[idx] = f_gate * state.c[idx] + i_gate * g_gate;
            // h_t = o * tanh(c)
            state.h[idx] = o_gate * state.c[idx].tanh();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lstm_state_zeros() {
        let s = LstmState::zeros(128);
        assert_eq!(s.h.len(), 128);
        assert_eq!(s.c.len(), 128);
        assert!(s.h.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn lstm_cell_zero_weights_produces_zero_update() {
        // Wszystkie wagi 0, bias 0 → po kroku h i c dalej 0
        let input_size = 4;
        let hidden_size = 3;
        let w_ih = vec![0.0; 4 * hidden_size * input_size];
        let w_hh = vec![0.0; 4 * hidden_size * hidden_size];
        let b_ih = vec![0.0; 4 * hidden_size];
        let b_hh = vec![0.0; 4 * hidden_size];

        let mut cell = LstmCell::new(input_size, hidden_size, w_ih, w_hh, b_ih, b_hh);
        let mut state = LstmState::zeros(hidden_size);
        let x = vec![1.0, 2.0, 3.0, 4.0];
        cell.step(&x, &mut state);

        // Z zerowymi wagami + zero init: sigmoid(0) = 0.5, tanh(0) = 0
        // g_gate = tanh(0) = 0 → i*g = 0 → c_new = f*c_old + 0 = 0
        // h_new = o * tanh(c_new) = 0.5 * tanh(0) = 0
        for &v in &state.c {
            assert!(v.abs() < 1e-6);
        }
        for &v in &state.h {
            assert!(v.abs() < 1e-6);
        }
    }
}
