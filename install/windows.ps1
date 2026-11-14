# Installs naTiVeFS on Windows for the current user, no administrator rights needed:
#   irm https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/windows.ps1 | iex
# The download is checked against the release's minisign signature before it runs. A file that does not match is
# never started. If the check itself is impossible here, a red warning says so and the install goes on.
$ErrorActionPreference = "Stop"
$repo = "TomasekValla/naTiVeFS"
$publicKey = "RWSQa45JZ1TSMKJ+gVjv+fsOf40z6FcRFMCMz2CplMxl40yszLg0URRu"

# Minisign check without extra tools: BLAKE2b-512 (the "prehashed" signatures) + Ed25519 (RFC 8032), compiled by
# PowerShell itself. Verify returns false for a wrong signature and throws when the files cannot be read.
$verifier = @'
using System;
using System.IO;
using System.Numerics;
using System.Security.Cryptography;
using System.Text;

public static class NativefsMinisign {
    static readonly ulong[] IV = {
        0x6a09e667f3bcc908UL, 0xbb67ae8584caa73bUL, 0x3c6ef372fe94f82bUL, 0xa54ff53a5f1d36f1UL,
        0x510e527fade682d1UL, 0x9b05688c2b3e6c1fUL, 0x1f83d9abfb41bd6bUL, 0x5be0cd19137e2179UL };
    static readonly int[][] Sigma = {
        new[] { 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15 },
        new[] { 14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3 },
        new[] { 11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4 },
        new[] { 7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8 },
        new[] { 9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13 },
        new[] { 2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9 },
        new[] { 12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11 },
        new[] { 13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10 },
        new[] { 6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5 },
        new[] { 10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0 } };

    static ulong Rotr(ulong x, int n) { return (x >> n) | (x << (64 - n)); }

    static void Compress(ulong[] h, byte[] block, ulong t0, ulong t1, bool last) {
        var m = new ulong[16];
        for (int i = 0; i < 16; i++) m[i] = BitConverter.ToUInt64(block, i * 8);
        var v = new ulong[16];
        for (int i = 0; i < 8; i++) { v[i] = h[i]; v[i + 8] = IV[i]; }
        v[12] ^= t0; v[13] ^= t1;
        if (last) v[14] = ~v[14];
        for (int r = 0; r < 12; r++) {
            var s = Sigma[r % 10];
            G(v, 0, 4, 8, 12, m[s[0]], m[s[1]]); G(v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
            G(v, 2, 6, 10, 14, m[s[4]], m[s[5]]); G(v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
            G(v, 0, 5, 10, 15, m[s[8]], m[s[9]]); G(v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
            G(v, 2, 7, 8, 13, m[s[12]], m[s[13]]); G(v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
        }
        for (int i = 0; i < 8; i++) h[i] ^= v[i] ^ v[i + 8];
    }

    static void G(ulong[] v, int a, int b, int c, int d, ulong x, ulong y) {
        v[a] = v[a] + v[b] + x; v[d] = Rotr(v[d] ^ v[a], 32);
        v[c] = v[c] + v[d]; v[b] = Rotr(v[b] ^ v[c], 24);
        v[a] = v[a] + v[b] + y; v[d] = Rotr(v[d] ^ v[a], 16);
        v[c] = v[c] + v[d]; v[b] = Rotr(v[b] ^ v[c], 63);
    }

    public static byte[] Blake2b512(Stream input) {
        var h = (ulong[])IV.Clone();
        h[0] ^= 0x01010040UL;
        var buf = new byte[128]; int fill = 0; ulong t0 = 0, t1 = 0;
        var chunk = new byte[1 << 16]; int n;
        while ((n = input.Read(chunk, 0, chunk.Length)) > 0) {
            int off = 0;
            while (off < n) {
                if (fill == 128) {
                    t0 += 128; if (t0 < 128) t1++;
                    Compress(h, buf, t0, t1, false); fill = 0;
                }
                int take = Math.Min(128 - fill, n - off);
                Buffer.BlockCopy(chunk, off, buf, fill, take); fill += take; off += take;
            }
        }
        t0 += (ulong)fill; if (t0 < (ulong)fill) t1++;
        for (int i = fill; i < 128; i++) buf[i] = 0;
        Compress(h, buf, t0, t1, true);
        var outp = new byte[64];
        for (int i = 0; i < 8; i++) Buffer.BlockCopy(BitConverter.GetBytes(h[i]), 0, outp, i * 8, 8);
        return outp;
    }

    // ── Ed25519 verification (RFC 8032, extended coordinates) ──────────────────────────────────────
    static readonly BigInteger P = BigInteger.Pow(2, 255) - 19;
    static readonly BigInteger L = BigInteger.Pow(2, 252) + BigInteger.Parse("27742317777372353535851937790883648493");
    static readonly BigInteger D = Mod(-121665 * Inv(121666));
    static readonly BigInteger SqrtM1 = BigInteger.ModPow(2, (P - 1) / 4, P);

    static BigInteger Mod(BigInteger a) { var r = a % P; return r.Sign < 0 ? r + P : r; }
    static BigInteger Inv(BigInteger a) { return BigInteger.ModPow(Mod(a), P - 2, P); }
    static BigInteger Le(byte[] b, int off, int len) {
        var t = new byte[len + 1]; Buffer.BlockCopy(b, off, t, 0, len); return new BigInteger(t);
    }

    static BigInteger[] Add(BigInteger[] p, BigInteger[] q) {
        var a = Mod((p[1] - p[0]) * (q[1] - q[0]));
        var b = Mod((p[1] + p[0]) * (q[1] + q[0]));
        var c = Mod(p[3] * 2 * D * q[3]);
        var d = Mod(p[2] * 2 * q[2]);
        BigInteger e = b - a, f = d - c, g = d + c, hh = b + a;
        return new[] { Mod(e * f), Mod(g * hh), Mod(f * g), Mod(e * hh) };
    }

    static BigInteger[] Mul(BigInteger s, BigInteger[] p) {
        var q = new[] { BigInteger.Zero, BigInteger.One, BigInteger.One, BigInteger.Zero };
        while (s.Sign > 0) {
            if (!s.IsEven) q = Add(q, p);
            p = Add(p, p);
            s >>= 1;
        }
        return q;
    }

    static BigInteger[] Decompress(byte[] b, int off) {
        var t = new byte[32]; Buffer.BlockCopy(b, off, t, 0, 32);
        int sign = t[31] >> 7; t[31] &= 0x7f;
        var y = Le(t, 0, 32);
        if (y >= P) return null;
        var x2 = Mod((y * y - 1) * Inv(D * y * y + 1));
        if (x2.IsZero) { if (sign == 1) return null; return new[] { BigInteger.Zero, y, BigInteger.One, BigInteger.Zero }; }
        var x = BigInteger.ModPow(x2, (P + 3) / 8, P);
        if (!Mod(x * x - x2).IsZero) x = Mod(x * SqrtM1);
        if (!Mod(x * x - x2).IsZero) return null;
        if ((int)(x % 2) != sign) x = P - x;
        return new[] { x, y, BigInteger.One, Mod(x * y) };
    }

    public static bool Ed25519Verify(byte[] pk, byte[] msg, byte[] sig) {
        if (pk.Length != 32 || sig.Length != 64) return false;
        var a = Decompress(pk, 0); if (a == null) return false;
        var r = Decompress(sig, 0); if (r == null) return false;
        var s = Le(sig, 32, 32); if (s >= L) return false;
        byte[] hash;
        using (var sha = SHA512.Create()) {
            var all = new byte[64 + msg.Length];
            Buffer.BlockCopy(sig, 0, all, 0, 32); Buffer.BlockCopy(pk, 0, all, 32, 32); Buffer.BlockCopy(msg, 0, all, 64, msg.Length);
            hash = sha.ComputeHash(all);
        }
        var h = Le(hash, 0, 64) % L;
        var by = Mod(4 * Inv(5));
        var basePoint = Decompress(Pad32(by), 0); // B: y = 4/5, x even
        var left = Mul(s, basePoint);
        var right = Add(r, Mul(h, a));
        return Mod(left[0] * right[2] - right[0] * left[2]).IsZero && Mod(left[1] * right[2] - right[1] * left[2]).IsZero;
    }

    static byte[] Pad32(BigInteger v) {
        var raw = v.ToByteArray(); var o = new byte[32];
        Buffer.BlockCopy(raw, 0, o, 0, Math.Min(32, raw.Length)); return o;
    }

    // minisign: public key "Ed" + key id (8) + key (32); signature line "ED"/"Ed" + key id + signature (64);
    // the trusted comment is covered by the global signature over signature + comment.
    public static bool Verify(string file, string sigFile, string publicKeyB64) {
        var pk = Convert.FromBase64String(publicKeyB64);
        var lines = File.ReadAllLines(sigFile);
        if (pk.Length != 42 || lines.Length < 4) return false;
        var sg = Convert.FromBase64String(lines[1].Trim());
        if (sg.Length != 74) return false;
        for (int i = 0; i < 8; i++) if (sg[2 + i] != pk[2 + i]) return false;
        var key = new byte[32]; Buffer.BlockCopy(pk, 10, key, 0, 32);
        var sig = new byte[64]; Buffer.BlockCopy(sg, 10, sig, 0, 64);
        byte[] msg;
        if (sg[0] == (byte)'E' && sg[1] == (byte)'D') { using (var f = File.OpenRead(file)) msg = Blake2b512(f); }
        else if (sg[0] == (byte)'E' && sg[1] == (byte)'d') { msg = File.ReadAllBytes(file); }
        else return false;
        if (!Ed25519Verify(key, msg, sig)) return false;
        const string prefix = "trusted comment: ";
        if (!lines[2].StartsWith(prefix)) return false;
        var comment = Encoding.UTF8.GetBytes(lines[2].Substring(prefix.Length));
        var global = Convert.FromBase64String(lines[3].Trim());
        var both = new byte[64 + comment.Length];
        Buffer.BlockCopy(sig, 0, both, 0, 64); Buffer.BlockCopy(comment, 0, both, 64, comment.Length);
        return Ed25519Verify(key, both, global);
    }
}
'@

# "verified", "mismatch" or the reason it could not be checked
function Test-NativefsSignature([string]$file, [string]$sigFile) {
    if (Get-Command minisign -ErrorAction SilentlyContinue) {
        minisign -Vm $file -x $sigFile -P $publicKey | Out-Null
        if ($LASTEXITCODE) { return "mismatch" } else { return "verified" }
    }
    try {
        if (-not ("NativefsMinisign" -as [type])) {
            # Windows PowerShell 5.1 (.NET Framework) needs the BigInteger assembly named; PowerShell 7 has it already
            if ($PSVersionTable.PSEdition -eq "Core") { Add-Type -TypeDefinition $verifier }
            else { Add-Type -TypeDefinition $verifier -ReferencedAssemblies System.Numerics }
        }
        if ([NativefsMinisign]::Verify($file, $sigFile, $publicKey)) { return "verified" } else { return "mismatch" }
    } catch {
        return "the built-in check could not run here ($($_.Exception.Message))"
    }
}

function Show-UnverifiedWarning([string]$why) {
    $line = "!" * 72
    Write-Host ""
    Write-Host $line -ForegroundColor Red
    Write-Host " WARNING: this copy of naTiVeFS could NOT be verified." -ForegroundColor Red
    Write-Host " Reason: $why" -ForegroundColor Red
    Write-Host " It was downloaded and installed anyway. You are using an unverified copy at your own risk." -ForegroundColor Red
    Write-Host " To check it next time, install minisign (winget install jedisct1.minisign) and run this again," -ForegroundColor Red
    Write-Host " or download it from https://github.com/$repo/releases and verify the .minisig file yourself." -ForegroundColor Red
    Write-Host $line -ForegroundColor Red
    Write-Host ""
}

$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
# the newest release, pre-releases included
$release = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases?per_page=1")[0]
$name = "naTiVeFS-windows-$arch-setup.exe"
$asset = $release.assets | Where-Object name -eq $name
if (-not $asset) { throw "No $name in $($release.tag_name)" }

$dir = Join-Path ([IO.Path]::GetTempPath()) "nativefs-install-$PID"
New-Item -ItemType Directory -Force $dir | Out-Null
try {
    $setup = Join-Path $dir $name
    Write-Host "Downloading naTiVeFS $($release.tag_name)"
    Invoke-WebRequest $asset.browser_download_url -OutFile $setup -UseBasicParsing
    $result = try {
        Invoke-WebRequest "$($asset.browser_download_url).minisig" -OutFile "$setup.minisig" -UseBasicParsing
        Test-NativefsSignature $setup "$setup.minisig"
    } catch { "the signature file could not be downloaded ($($_.Exception.Message))" }
    if ($result -eq "mismatch") {
        Write-Host "The signature does NOT match: this file is not the published naTiVeFS. Nothing was installed." -ForegroundColor Red
        throw "Signature mismatch, nothing was installed."
    } elseif ($result -eq "verified") {
        Write-Host "Signature verified." -ForegroundColor Green
    } else {
        Show-UnverifiedWarning $result
    }
    Start-Process $setup -ArgumentList "/S" -Wait
    Write-Host "naTiVeFS $($release.tag_name) is installed. Find it in the Start menu."
} finally {
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}
