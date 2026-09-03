# Filedemo: demonstrates FS ops inside app
echo === File Demo App ===
echo Creating /tmp/demo.txt ...
write /tmp/demo.txt Hello from app! This file was created by a script app.
echo Reading back:
cat /tmp/demo.txt
echo ---
echo Creating a directory:
mkdir /tmp/myapp
write /tmp/myapp/readme.txt Demo file inside myapp
ls /tmp
ls /tmp/myapp
echo Demo complete.
