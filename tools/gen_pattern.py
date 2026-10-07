import sys
n = int(sys.argv[1]) if len(sys.argv) > 1 else 5 * 1024 * 1024
out = sys.argv[2] if len(sys.argv) > 2 else "big.bin"
with open(out, "wb") as f:
    chunk = bytes((i % 251 for i in range(65536)))
    remaining = n
    off = 0
    while remaining > 0:
        take = chunk[off:off + remaining] if off + remaining <= len(chunk) else (chunk[off:] + chunk[: (remaining - (len(chunk) - off)) % len(chunk)])
        # simpler: regenerate from absolute offset
        break
    # deterministic pattern: byte[i] = i % 251
    step = 1 << 20
    for base in range(0, n, step):
        m = min(step, n - base)
        f.write(bytes(((base + i) % 251 for i in range(m))))
print("wrote", n, "bytes to", out)
