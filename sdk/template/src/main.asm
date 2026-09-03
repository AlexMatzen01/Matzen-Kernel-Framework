# Example MFKE assembly for template app
# Assemble: python tools/mfke_asm.py sdk/template/src/main.asm app.mfke
# Then in kernel: writehex /apps/myapp.mfke <hex> ; run /apps/myapp.mfke

PRINT "Hello from template app! "
PRINT_NL
PUSH 42
PRINT_INT
PRINT_NL
PRINT "Counter 0..4:"
PRINT_NL
PUSH 0
loop:
  DUP
  PRINT_INT
  PRINT_NL
  PUSH 1
  ADD
  DUP
  PUSH 5
  LT
  JZ done
  JMP loop
done:
  POP
HALT
