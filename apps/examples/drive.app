# Drive: mkfs + mount built into an app
echo === Drive App ===
echo Formatting disk...
mkfs
echo Mounting filesystem...
mount
echo Disk ready:
diskinfo
ls
pwd
echo Drive setup complete.
