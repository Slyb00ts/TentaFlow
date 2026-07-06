# =============================================================================
# File: model.py
# Purpose: Small CRNN (CNN backbone -> height collapse -> BiLSTM -> CTC head)
#          for single-row ADR digit reading. Alphabet = 0-9, CTC blank = 0.
# =============================================================================
import torch
import torch.nn as nn

ALPHABET = "0123456789"          # index i -> class (i+1); blank = 0
NUM_CLASSES = len(ALPHABET) + 1  # + CTC blank
IMG_H, IMG_W = 32, 128


def _conv(ci, co):
    return nn.Sequential(
        nn.Conv2d(ci, co, 3, 1, 1, bias=False),
        nn.BatchNorm2d(co),
        nn.ReLU(inplace=True),
    )


class CRNN(nn.Module):
    def __init__(self, n_classes=NUM_CLASSES, lstm_hidden=128):
        super().__init__()
        self.cnn = nn.Sequential(
            _conv(1, 32),  nn.MaxPool2d(2, 2),          # 32x128 -> 16x64
            _conv(32, 64), nn.MaxPool2d(2, 2),          # -> 8x32
            _conv(64, 128), nn.MaxPool2d((2, 1), (2, 1)),  # -> 4x32
            _conv(128, 128), nn.MaxPool2d((2, 1), (2, 1)),  # -> 2x32
            _conv(128, 128), nn.MaxPool2d((2, 1), (2, 1)),  # -> 1x32
        )
        self.rnn = nn.LSTM(128, lstm_hidden, num_layers=2,
                           bidirectional=True, batch_first=True, dropout=0.1)
        self.fc = nn.Linear(lstm_hidden * 2, n_classes)

    def forward(self, x):
        f = self.cnn(x)                 # [B,128,1,W']
        assert f.size(2) == 1, f.shape
        f = f.squeeze(2).permute(0, 2, 1)  # [B, W', 128]
        r, _ = self.rnn(f)              # [B, W', 2H]
        y = self.fc(r)                  # [B, W', C]
        return y                        # logits (T=B dim=1)
