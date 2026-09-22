# MFK Java Application Template

Compile Java applications for the current MFK Java baseline:

```powershell
javac --release 8 -d build src\Hello.java
```

Copy the resulting `.class` files into `apps/examples/java/`, then build a
disk image with `--bundle-apps`. Inside MFK:

```text
mount
java /apps/java/Hello.class MFK
```

The runtime executes `main(String[])`, including real command-line arguments,
`System.out.println`, and the compiler-generated `StringBuilder` concatenation
used by this example. JAR loading and in-kernel `javac` remain subsequent
milestones.
