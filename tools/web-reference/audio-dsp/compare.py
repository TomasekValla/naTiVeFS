# Compares web_<case>.f32 with native_<case>.f32 (interleaved stereo f32, 44.1 kHz).
import sys, os, numpy as np
d = sys.argv[1]
def load(n): return np.fromfile(os.path.join(d, n), dtype='<f4').astype(np.float64).reshape(-1, 2)
def db(x): return 10*np.log10(x) if x > 0 else -999
inp = load('web_input.f32')
cases = ['neutral','eq_1k_plus6','eq_lowshelf90_plus6','filter_m40','filter_p40','width0','width180','rotate50','volume150','volume50','pitch7','reverb40','echo40']
print(f"{'case':22} {'lag':>6} {'diff dB':>8} {'web rms':>8} {'nat rms':>8} {'web pk':>7} {'nat pk':>7} {'web side':>8} {'nat side':>8}")
for c in cases:
    w, n = load('web_'+c+'.f32'), load('native_'+c+'.f32')
    m = min(len(w), len(n))
    seg = slice(22050, 22050 + 65536)
    a, b = w[seg, 0], n[seg, 0]
    # lag of native relative to web by FFT cross-correlation (search +-8192)
    N = 1 << 18
    A, B = np.fft.rfft(a, N), np.fft.rfft(b, N)
    cc = np.fft.irfft(A * np.conj(B), N)
    lags = np.concatenate([np.arange(0, 8193), np.arange(-8192, 0)])
    vals = np.concatenate([cc[:8193], cc[-8192:]])
    lag = int(lags[np.argmax(vals)])  # w[t] ~ n[t - lag]
    s0 = 22050 + max(0, lag); s1 = m - 22050 - max(0, -lag)
    ww = w[s0:s1]; nn = n[s0 - lag:s1 - lag]
    diff = db(np.sum((ww - nn)**2) / np.sum(ww**2))
    rms = lambda x: 20*np.log10(np.sqrt(np.mean(x**2)))
    pk = lambda x: 20*np.log10(np.max(np.abs(x)))
    side = lambda x: 20*np.log10(np.sqrt(np.mean(((x[:,0]-x[:,1])/2)**2)) / np.sqrt(np.mean(((x[:,0]+x[:,1])/2)**2)))
    print(f"{c:22} {lag:6d} {diff:8.1f} {rms(ww):8.2f} {rms(nn):8.2f} {pk(ww):7.2f} {pk(nn):7.2f} {side(ww):8.2f} {side(nn):8.2f}")
