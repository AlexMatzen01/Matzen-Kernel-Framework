# MFK Example App: hello.app
# Script app - each line is a shell command
echo === Hello from MFK Script App! ===
echo App path: $0
echo Args: $@ (argc=$#)
echo ---
echo Current directory:
pwd
echo Files in current dir:
ls
echo ---
echo Uptime:
uptime
echo ---
echo Color test:
color cyan
echo This is cyan!
color green
echo Back to green.
echo ---
echo App finished with exit 0
