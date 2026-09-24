# Drive: mkfs + mount built into an app
echo === Drive App ===
echo Formatting disk...
mkfs 1 --yes
echo Mounting filesystem...
mount 1
echo Disk ready:
diskinfo
ls
pwd
echo Drive setup complete.
