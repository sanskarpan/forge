# Security policy

## Scope

Forge emits native machine code and manages executable memory. It is an
educational compiler and inspection tool, not a security sandbox or a
replacement for a hardened JIT runtime. Do not compile expressions, register
external function pointers, or run the Workbench against untrusted code in a
privileged process.

The highest-risk areas are:

- handwritten x86-64 and AArch64 encoders;
- W^X transitions and executable-buffer lifetime management;
- raw native addresses supplied through `ExternalFunction`;
- parser, optimizer, verifier, and artifact deserialization boundaries;
- platform-specific ABI and cache-flush code.

## Reporting a vulnerability

Please do not open a public issue for an exploitable memory-safety, code
execution, executable-memory, or supply-chain problem. Use GitHub's private
security advisory flow for `sanskarpan/forge` and include:

1. the affected commit or release;
2. platform, architecture, compiler version, and operating-system details;
3. a minimal reproducer or a clear proof of impact;
4. whether the issue requires native execution, WASM, the Workbench, or a
   caller-provided external address.

If private advisories are unavailable, contact the repository owner through
the GitHub profile and mark the message **security-sensitive**. Please allow
reasonable time for triage and coordinated disclosure.

## Security expectations

Every unsafe block must have a nearby `SAFETY:` explanation. Native execution
must remain W^X: buffers are writable only during construction and executable
only after finalization. Portable artifacts must not embed process-local
function pointers. Changes to these invariants require focused regression
tests and platform evidence in the pull request.
