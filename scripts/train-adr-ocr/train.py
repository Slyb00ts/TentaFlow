# =============================================================================
# File: train.py
# Purpose: Train the ADR CRNN on on-the-fly synthetic data (CTC loss, AMP, 4090).
#          Validates on a held-out synthetic split. Saves best weights.
# =============================================================================
import os, random, time, argparse
import numpy as np
import torch
import torch.nn as nn
from torch.utils.data import Dataset, DataLoader

import gen_synth
from model import CRNN, ALPHABET, IMG_H, IMG_W

HERE = os.path.dirname(os.path.abspath(__file__))
CHAR2IDX = {c: i + 1 for i, c in enumerate(ALPHABET)}  # blank = 0


class SynthDataset(Dataset):
    """On-the-fly synthetic samples. length only bounds one epoch."""
    def __init__(self, length, seed_base):
        self.length = length
        self.seed_base = seed_base

    def __len__(self):
        return self.length

    def __getitem__(self, idx):
        # deterministic-ish per (worker,idx) but varied
        g, text = gen_synth.make_sample()
        x = torch.from_numpy(g).float().div_(255.0).sub_(0.5).div_(0.5).unsqueeze(0)
        target = torch.tensor([CHAR2IDX[c] for c in text], dtype=torch.long)
        return x, target, len(text), text


def collate(batch):
    xs, targets, tlens, texts = zip(*batch)
    xs = torch.stack(xs, 0)
    targets_cat = torch.cat(targets)
    tlens = torch.tensor(tlens, dtype=torch.long)
    return xs, targets_cat, tlens, texts


def worker_init(wid):
    seed = (torch.initial_seed() % 2**31) + wid
    random.seed(seed)
    np.random.seed(seed % 2**31)


def greedy_decode(logits):
    # logits [B,T,C] -> list of strings
    idx = logits.argmax(-1).cpu().numpy()  # [B,T]
    out = []
    for row in idx:
        prev = 0
        s = []
        for v in row:
            if v != 0 and v != prev:
                s.append(ALPHABET[v - 1])
            prev = v
        out.append("".join(s))
    return out


@torch.no_grad()
def evaluate(model, loader, device, max_batches=40):
    model.eval()
    correct = tot = 0
    for i, (xs, tcat, tlens, texts) in enumerate(loader):
        if i >= max_batches:
            break
        xs = xs.to(device)
        with torch.autocast("cuda", dtype=torch.float16):
            logits = model(xs)
        preds = greedy_decode(logits.float())
        for p, t in zip(preds, texts):
            correct += int(p == t)
            tot += 1
    return correct / max(1, tot)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--epochs", type=int, default=12)
    ap.add_argument("--steps", type=int, default=1500)   # batches per epoch
    ap.add_argument("--batch", type=int, default=256)
    ap.add_argument("--workers", type=int, default=12)
    ap.add_argument("--lr", type=float, default=1e-3)
    args = ap.parse_args()

    device = "cuda"
    torch.backends.cudnn.benchmark = True

    model = CRNN().to(device)
    nparams = sum(p.numel() for p in model.parameters())
    print(f"model params: {nparams/1e6:.3f}M  (~{nparams*4/1e6:.2f} MB fp32)")

    train_ds = SynthDataset(args.steps * args.batch, seed_base=1)
    val_ds = SynthDataset(60 * args.batch, seed_base=99999)
    train_ld = DataLoader(train_ds, batch_size=args.batch, shuffle=False,
                          num_workers=args.workers, collate_fn=collate,
                          worker_init_fn=worker_init, drop_last=True,
                          persistent_workers=True, prefetch_factor=4, pin_memory=True)
    val_ld = DataLoader(val_ds, batch_size=args.batch, shuffle=False,
                        num_workers=4, collate_fn=collate,
                        worker_init_fn=worker_init, drop_last=True)

    ctc = nn.CTCLoss(blank=0, zero_infinity=True)
    opt = torch.optim.AdamW(model.parameters(), lr=args.lr, weight_decay=1e-4)
    total_steps = args.epochs * args.steps
    sched = torch.optim.lr_scheduler.OneCycleLR(opt, max_lr=args.lr,
                                                total_steps=total_steps, pct_start=0.1)
    scaler = torch.amp.GradScaler("cuda")

    best = 0.0
    step = 0
    for ep in range(args.epochs):
        model.train()
        t0 = time.time()
        run = 0.0
        for xs, tcat, tlens, texts in train_ld:
            xs = xs.to(device, non_blocking=True)
            tcat = tcat.to(device)
            with torch.autocast("cuda", dtype=torch.float16):
                logits = model(xs)                 # [B,T,C]
                logp = logits.log_softmax(-1).permute(1, 0, 2)  # [T,B,C]
                T = logp.size(0)
                in_lens = torch.full((xs.size(0),), T, dtype=torch.long, device=device)
                loss = ctc(logp, tcat, in_lens, tlens.to(device))
            opt.zero_grad(set_to_none=True)
            scaler.scale(loss).backward()
            scaler.unscale_(opt)
            nn.utils.clip_grad_norm_(model.parameters(), 5.0)
            scaler.step(opt)
            scaler.update()
            sched.step()
            run += loss.item()
            step += 1
        acc = evaluate(model, val_ld, device)
        print(f"ep {ep+1}/{args.epochs}  loss {run/args.steps:.4f}  "
              f"val_exact {acc*100:.2f}%  lr {sched.get_last_lr()[0]:.2e}  "
              f"{time.time()-t0:.1f}s", flush=True)
        if acc >= best:
            best = acc
            torch.save(model.state_dict(), os.path.join(HERE, "crnn_best.pt"))
    print(f"best val exact-match: {best*100:.2f}%")
    with open(os.path.join(HERE, "adr_ocr_alphabet.txt"), "w") as f:
        f.write(ALPHABET + "\n")


if __name__ == "__main__":
    main()
