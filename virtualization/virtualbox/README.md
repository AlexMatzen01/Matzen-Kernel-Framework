# VirtualBox Template

VirtualBox can boot the `bootimage-mfk-kernel.bin` artifact via a synthetic raw disk attached to an IDE controller.

## Fast start

1. Build the boot image:
   ```bash
   cargo bootimage -p mfk-kernel
   ```
2. Copy `virtualization/virtualbox/mfk-kernel.vbox` next to the boot image or update the `<VirtualBoxMachine>` `Location` attribute to point at your repo.
3. Import the VM:
   ```bash
   VBoxManage registervm virtualization/virtualbox/mfk-kernel.vbox
   ```
4. Attach the latest kernel binary as a virtual disk:
   ```bash
   VBoxManage storageattach MFK-Kernel \
     --storagectl "IDE" --port 0 --device 0 \
     --type hdd \
     --medium target/x86_64-mfk/debug/bootimage-mfk-kernel.bin
   ```
5. Start the VM headless or with the GUI.

## Notes

- Nested virtualization must be enabled if running inside another VM.
- The template intentionally disables EFI to mimic BIOS boot, aligning with the current bootloader configuration.
- VirtualBox caches disk geometry aggressively; run `VBoxManage closemedium disk <path> --delete` before reusing an older artifact.
