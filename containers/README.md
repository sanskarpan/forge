# Container validation

These images make the portable portions of the validation matrix reproducible
without installing a second toolchain on the host.

The Makefile uses Podman by default and accepts `CONTAINER_ENGINE=docker`.
The engine must already have a working Linux VM on macOS. The repository does
not create or delete that VM automatically.

```sh
podman machine start
make container-images
make container-test-x86
make container-test-arm64
make container-test-wasm
make container-test-workbench
```

`container-test-x86` should run on a native x86-64 runner for authoritative JIT
execution and CPU behavior. On Apple Silicon, `--platform linux/amd64` uses
emulation and is useful for smoke testing only. `container-test-arm64` is
native when the Linux VM/runner is ARM64 and otherwise is an emulated
correctness check. Neither Linux container reproduces macOS `MAP_JIT`,
entitlements, or Apple instruction-cache behavior; those remain in the macOS
GitHub Actions job.

The WASM target installs the pinned Rust target inside the image and checks
the wasm-bindgen API. The workbench image runs the Node smoke test and is the
place to add `npm ci` once the full frontend has a lockfile.
