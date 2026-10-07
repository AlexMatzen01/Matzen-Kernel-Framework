import struct
d = open("/mnt/c/Users/death/Documents/Matzen-Kernel-Framework/target/fs-test/exfat.img", "rb").read()
s11 = d[11 * 512:12 * 512]
stored = struct.unpack("<I", s11[:4])[0]
print("stored: %08x" % stored)
for skip in [(), (106, 107, 112)]:
    ck = 0
    for i in range(11 * 512):
        if i in skip:
            continue
        ck = (((ck & 1) << 31) + (ck >> 1) + d[i]) & 0xFFFFFFFF
    print("skip=%s -> %08x match=%s" % (skip, ck, ck == stored))
print("volflags:", d[106:108].hex(), "pctinuse:", d[112:113].hex())
s1 = d[1 * 512:11 * 512]
print("sectors1-10 nonzero byte count:", sum(1 for b in s1 if b != 0))
