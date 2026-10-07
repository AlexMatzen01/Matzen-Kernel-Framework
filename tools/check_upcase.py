import struct
d = open("/mnt/c/Users/death/Documents/Matzen-Kernel-Framework/target/fs-test/exfat.img", "rb").read()
off = (4096 + (5 - 2) * 8) * 512
# find 0x82 upcase entry in root
o = 0
while o + 32 <= 4096:
    t = d[off + o]
    if t == 0x82 and t & 0x80:
        e = d[off + o:off + o + 32]
        print("upcase entry:", e.hex())
        first = struct.unpack("<I", e[20:24])[0]
        size = struct.unpack("<Q", e[24:32])[0]
        print("first cluster:", first, "size:", size)
        # upcase table bytes: contiguous from first cluster
        heap = 4096
        start = (heap + (first - 2) * 8) * 512
        tab = d[start:start + size]
        print("table len:", len(tab))
        print("table[0:16]:", tab[:16].hex())
        # candidate: 16-bit rotate-add over table bytes?
        s16 = 0
        for b in tab:
            s16 = (((s16 & 1) << 15) + (s16 >> 1) + b) & 0xFFFF
        # candidate: 32-bit rotate-add?
        s32 = 0
        for b in tab:
            s32 = (((s32 & 1) << 31) + (s32 >> 1) + b) & 0xFFFFFFFF
        print("entry checksum field [4:8]:", e[4:8].hex())
        print("16-bit sum: %04x  32-bit sum: %08x" % (s16, s32))
        break
    sec = d[off + o + 1] if t & 0x80 else 0
    o += (1 + sec) * 32
