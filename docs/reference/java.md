# Java Runtime Roadmap

## Current milestone

MFK now has a `no_std` Java runtime boundary in `kernel/src/java/`:

* Parses the `CAFEBABE` class header and big-endian minor/major version.
* Maps class versions: Java 8 = 52, Java 11 = 55, Java 17 = 61, Java 21 = 65.
* Accepts Java 8 class files for the baseline runtime and gives a clear
  `UnsupportedClassVersionError` for newer releases.
* Exposes `java`, `javac`, `jar`, `jversions`, and `jsdk` shell commands.

The Phase 2 interpreter currently executes a small Java 8 subset: static
`main(String[])`, integer constants/arithmetic, locals, branches, strings,
`System.out`, and `PrintStream.print/println`. Unsupported bytecodes fail
cleanly with a VM error.

## Host build workflow

```text
javac --release 8 -d build src/Hello.java
copy build/Hello.class to apps/examples/java/
build and run with --bundle-apps
mount
java /apps/java/Hello.class
```

The existing host bundler injects files under `apps/examples` into `/apps`.
The runtime accepts `.class` files up to available filesystem and kernel memory.

## Phase 2 and Phase 3 status

1. **Implemented:** bounds-checked constant-pool, method, and `Code` parser.
2. **Implemented:** cooperative operand-stack interpreter for the initial
   Java bytecode subset.
3. **Implemented:** `System.out` and `PrintStream.print/println` native bridge.
4. **Implemented (Phase 3 slice):** bounded heap handles for argument arrays and
   `StringBuilder` objects.
5. **Implemented (Phase 3 slice):** real `String[]` arguments, nested same-class
   `invokestatic` calls, returns, array reads, and conditional branches.
6. **Implemented (Phase 3 slice):** compiler-generated `StringBuilder` string
   concatenation for the Java template.
7. Add general object fields, external class loading, exceptions, and the
   remaining Java 8 natives.
8. Add larger files, ZIP/JAR class loading, and class paths.
9. Add paging, ring-3 processes, threads, and POSIX-like services.
10. Add in-kernel `javac` after the Java runtime can execute the compiler and its
   required class library.
