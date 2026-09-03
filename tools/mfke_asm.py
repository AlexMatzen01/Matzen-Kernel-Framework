#!/usr/bin/env python3
"""
MFKE assembler - converts text assembly to MFKE bytecode
Usage: python mfke_asm.py input.asm output.mfke
       python mfke_asm.py --hex input.asm   # prints hex for writehex
Assembly syntax:
  PUSH 42
  ADD, SUB, MUL, DIV, MOD, DUP, POP, EQ, LT, GT
  PRINT_INT, PRINT "hello", PRINT_NL
  JMP label, JZ label, JNZ label
  SLEEP 500, YIELD, HALT, EXIT
  label:
"""

import struct, sys

OP = {
 'HALT':0x00,'PUSH':0x01,'ADD':0x02,'SUB':0x03,'MUL':0x04,'DIV':0x05,'MOD':0x06,
 'PRINT_INT':0x07,'PRINT':0x08,'PRINT_STR':0x08,'PRINT_NL':0x09,'DUP':0x0A,'POP':0x0B,
 'JMP':0x0C,'JZ':0x0D,'JNZ':0x0E,'EQ':0x0F,'LT':0x10,'GT':0x11,'SLEEP':0x12,'YIELD':0x13,'EXIT':0x14,'CALL':0x15
}

def asm_to_bc(lines):
    bc = bytearray()
    labels = {}
    fixups = []  # (pos, label, is_jmp)
    for line in lines:
        line=line.split('#')[0].strip()
        if not line: continue
        if line.endswith(':'):
            labels[line[:-1].strip()] = len(bc)
            continue
        parts=line.split(None,1)
        mn=parts[0].upper()
        arg=parts[1] if len(parts)>1 else ""
        if mn=='PUSH':
            bc.append(OP['PUSH'])
            bc.extend(struct.pack('<i', int(arg)))
        elif mn in ('ADD','SUB','MUL','DIV','MOD','EQ','LT','GT','DUP','POP','PRINT_INT','PRINT_NL','YIELD','HALT','EXIT'):
            bc.append(OP[mn])
        elif mn in ('PRINT','PRINT_STR'):
            s=arg.strip()
            if s.startswith('"') and s.endswith('"'):
                s=s[1:-1].encode('utf-8').decode('unicode_escape').encode('utf-8')
            else:
                s=arg.encode()
            bc.append(OP['PRINT_STR'])
            bc.extend(struct.pack('<H', len(s)))
            bc.extend(s)
        elif mn=='SLEEP':
            bc.append(OP['SLEEP'])
            bc.extend(struct.pack('<H', int(arg)))
        elif mn in ('JMP','JZ','JNZ'):
            bc.append(OP[mn])
            # placeholder
            pos=len(bc)
            bc.extend(struct.pack('<h', 0))
            fixups.append((pos, arg.strip(), mn))
        else:
            raise ValueError(f"unknown mnemonic {mn}")
    # fixups
    for pos,label,_ in fixups:
        if label not in labels:
            raise ValueError(f"undefined label {label}")
        target=labels[label]
        nxt=pos+2
        off=target - nxt
        if off < -32768 or off>32767:
            raise ValueError(f"jump too far {off}")
        struct.pack_into('<h', bc, pos, off)
    return bytes(bc)

def build_mfke(bc):
    hdr=struct.pack('<IIIIII2I', 0x454B4D46,1,32,len(bc),0,0,0,0)
    return hdr+bc

if __name__=='__main__':
    import argparse
    ap=argparse.ArgumentParser()
    ap.add_argument('input', help='asm file')
    ap.add_argument('output', nargs='?', help='mfke file')
    ap.add_argument('--hex', action='store_true')
    args=ap.parse_args()
    with open(args.input) as f:
        bc=asm_to_bc(f.readlines())
    mfke=build_mfke(bc)
    if args.hex:
        print(mfke.hex())
    elif args.output:
        open(args.output,'wb').write(mfke)
        print(f"wrote {len(mfke)} bytes ({len(bc)} bytecode) to {args.output}")
    else:
        sys.stdout.buffer.write(mfke)
