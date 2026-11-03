#!/usr/bin/env python3
"""Declick and declip a 16-bit stereo WAV.

Each channel is modelled block by block with an autoregressive (AR) model.
Samples whose prediction error is far above the block's typical error, and
runs at full scale, are marked; each marked run is rebuilt by least-squares AR
interpolation (Vaseghi/Godsill style) from the samples around it.

Then scales the whole file so the peak sits at 23000, leaving room for the
Opus encoder's overshoot (the game decodes movies to 16-bit, which would
hard-clip it).

Used for prod1.mkv, whose disc audio is mastered hot:
    flm audio -> prod1.wav (22050 Hz), then
    movie_declick.py prod1.wav prod1_fixed.wav 5
    ffmpeg -i hd/prod1.mkv -i prod1_fixed.wav -map 0:v -map 1:a -c:v copy \
      -af aresample=48000:resampler=soxr:precision=28 -c:a libopus -b:a 160k \
      -shortest out.mkv

usage: movie_declick.py in.wav out.wav [threshold]
"""
import sys
import numpy as np

ORDER = 24
BLOCK = 2048
MARGIN = 2          # extra samples each side of a detection
MAX_GAP = 160       # longer runs are left alone
CLIP = 32700


def read(path):
    raw = open(path, 'rb').read()
    rate = int.from_bytes(raw[24:28], 'little')
    i = raw.index(b'data') + 8
    return rate, np.frombuffer(raw[i:], dtype='<i2').reshape(-1, 2).astype(float)


def write(path, rate, d):
    pcm = np.clip(np.round(d), -32768, 32767).astype('<i2').tobytes()
    hdr = b'RIFF' + (36 + len(pcm)).to_bytes(4, 'little') + b'WAVEfmt ' + (16).to_bytes(4, 'little')
    hdr += (1).to_bytes(2, 'little') + (2).to_bytes(2, 'little') + rate.to_bytes(4, 'little')
    hdr += (rate * 4).to_bytes(4, 'little') + (4).to_bytes(2, 'little') + (16).to_bytes(2, 'little')
    open(path, 'wb').write(hdr + b'data' + len(pcm).to_bytes(4, 'little') + pcm)


def ar(x, p):
    """AR coefficients a (x[n] ~ sum a[k] x[n-1-k]) by Levinson-Durbin."""
    r = np.array([x[: len(x) - k] @ x[k:] for k in range(p + 1)])
    r[0] *= 1.0 + 1e-4
    if r[0] <= 0:
        return np.zeros(p)
    a = np.zeros(p)
    e = r[0]
    for i in range(p):
        k = (r[i + 1] - a[:i] @ r[i:0:-1]) / e
        a[:i] = a[:i] - k * a[:i][::-1]
        a[i] = k
        e *= 1 - k * k
        if e <= 0:
            break
    return a


def residual(x, a):
    p = len(a)
    e = np.zeros_like(x)
    for n in range(p, len(x)):
        e[n] = x[n] - a @ x[n - 1 : n - 1 - p : -1] if n - 1 - p >= 0 else x[n] - a @ x[n - 1 :: -1][:p]
    return e


def interpolate(x, a, lo, hi):
    """Least-squares AR fill of x[lo:hi] using the samples around it."""
    p = len(a)
    s, t = max(0, lo - p), min(len(x), hi + p)
    seg = x[s:t].copy()
    n = len(seg)
    # Prediction-error matrix: rows n = p..n-1, e = A seg.
    rows = n - p
    A = np.zeros((rows, n))
    for i in range(rows):
        A[i, i + p] = 1.0
        A[i, i : i + p] = -a[::-1]
    unknown = np.arange(lo - s, hi - s)
    known = np.setdiff1d(np.arange(n), unknown)
    Au, Ak = A[:, unknown], A[:, known]
    sol, *_ = np.linalg.lstsq(Au, -Ak @ seg[known], rcond=None)
    x[lo:hi] = sol


def repair(x, threshold):
    x = x.copy()
    fixed = 0
    runs = 0
    for b0 in range(0, len(x), BLOCK):
        lo_b, hi_b = max(0, b0 - ORDER * 4), min(len(x), b0 + BLOCK + ORDER * 4)
        blk = x[lo_b:hi_b]
        if np.abs(blk).max() < 64:
            continue
        a = ar(blk * np.hanning(len(blk)), ORDER)
        e = np.zeros(len(blk))
        p = ORDER
        for n in range(p, len(blk)):
            e[n] = blk[n] - a @ blk[n - 1 :: -1][:p]
        scale = np.median(np.abs(e[p:])) / 0.6745 + 1.0
        bad = np.abs(e) > threshold * scale
        bad |= np.abs(blk) >= CLIP
        bad[:p] = False
        idx = np.flatnonzero(bad)
        idx = idx[(idx + lo_b >= b0) & (idx + lo_b < b0 + BLOCK)]
        if len(idx) == 0:
            continue
        # Merge into runs with a margin.
        start = idx[0] - MARGIN
        end = idx[0] + MARGIN + 1
        spans = []
        for i in idx[1:]:
            if i - MARGIN <= end + ORDER // 2:
                end = i + MARGIN + 1
            else:
                spans.append((start, end))
                start, end = i - MARGIN, i + MARGIN + 1
        spans.append((start, end))
        for s, t in spans:
            s, t = max(s + lo_b, ORDER), min(t + lo_b, len(x) - ORDER)
            if 0 < t - s <= MAX_GAP:
                interpolate(x, a, s, t)
                fixed += t - s
                runs += 1
    return x, fixed, runs


def main():
    rate, d = read(sys.argv[1])
    threshold = float(sys.argv[3]) if len(sys.argv) > 3 else 6.0
    out = d.copy()
    for ch in range(2):
        out[:, ch], fixed, runs = repair(d[:, ch], threshold)
        # Second pass catches what the first one's model hid.
        out[:, ch], fixed2, runs2 = repair(out[:, ch], threshold)
        fixed, runs = fixed + fixed2, runs + runs2
        print(f'channel {ch}: {runs} runs, {fixed} samples rebuilt '
              f'({100 * fixed / len(d):.2f}%)')
    peak = np.abs(out).max()
    gain = min(1.0, 23000.0 / peak)
    print(f'rebuilt peak {peak:.0f}, gain {gain:.3f} ({20 * np.log10(gain):.1f} dB)')
    write(sys.argv[2], rate, out * gain)


main()
