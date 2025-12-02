# QEMU Launch Profiles

Use these presets to integrate the Matzen Kernel Framework with local scripts, CI, or VS Code tasks.

## Minimal CLI

```bash
qemu-system-x86_64 \
  -drive format=raw,file=target/x86_64-mfk/debug/bootimage-mfk-kernel.bin \
  -serial stdio -display none -m 256M -cpu qemu64 -smp 2 -no-reboot
```

## qemu-system-x86_64 config file

The repo ships with `virtualization/qemu/qemu-args.toml`. Feed it to QEMU via `-readconfig`:

```bash
qemu-system-x86_64 -readconfig virtualization/qemu/qemu-args.toml \
  -drive format=raw,file=target/x86_64-mfk/debug/bootimage-mfk-kernel.bin
```

## Tips

- Enable `-d int,cpu_reset` when chasing interrupt issues.
- Use `-s -S` to let GDB attach before the CPU starts executing the kernel.
- For framebuffer work, add `-vga std` and drop `-display none`.
