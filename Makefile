.PHONY: test test-differential test-encoding bench qemu-aarch64 wasm workbench codesign spike \
	container-images container-test-x86 container-test-arm64 container-test-wasm container-test-workbench

CONTAINER_ENGINE ?= podman
CONTAINER_RUST_IMAGE ?= forge-rust-ci:1.87
CONTAINER_WASM_IMAGE ?= forge-wasm-ci:1.87
CONTAINER_WORKBENCH_IMAGE ?= forge-workbench-ci:22

spike:
	cargo build --example spike -p forge-mem
	codesign --entitlements entitlements.plist -s - target/debug/examples/spike
	./target/debug/examples/spike

codesign:
	codesign --entitlements entitlements.plist -s - target/debug/examples/spike

test:
	cargo test --workspace

test-differential:
	cargo test -p forge-opt --test differential

test-encoding:
	cargo test -p forge-x64 --test round_trip

bench:
	cargo bench -p forge-regalloc --bench allocation -- --noplot

qemu-aarch64:
	@if command -v rustup >/dev/null 2>&1 && command -v qemu-aarch64 >/dev/null 2>&1 && \
		rustup target list --installed 2>/dev/null | grep -q '^aarch64-unknown-linux-gnu$$'; then \
		cargo test -p forge-aarch64 --target aarch64-unknown-linux-gnu; \
	else \
		echo "cross/QEMU AArch64 toolchain unavailable; running native backend tests"; \
		cargo test -p forge-aarch64; \
	fi

wasm:
	cargo test -p forge-wasm -p forge-wasm-api

workbench:
	@test -f workbench/package.json || \
		{ echo "workbench is not present in this repository"; exit 2; }
	npm test --prefix workbench

container-images:
	$(CONTAINER_ENGINE) build -f containers/Dockerfile.rust-ci -t $(CONTAINER_RUST_IMAGE) .
	$(CONTAINER_ENGINE) build -f containers/Dockerfile.workbench -t $(CONTAINER_WORKBENCH_IMAGE) .

container-test-x86:
	$(CONTAINER_ENGINE) build --platform linux/amd64 -f containers/Dockerfile.rust-ci -t $(CONTAINER_RUST_IMAGE)-amd64 .
	$(CONTAINER_ENGINE) run --rm --platform linux/amd64 -v "$(CURDIR):/workspace" -w /workspace $(CONTAINER_RUST_IMAGE)-amd64 cargo test --workspace --locked

container-test-arm64:
	$(CONTAINER_ENGINE) build --platform linux/arm64 -f containers/Dockerfile.rust-ci -t $(CONTAINER_RUST_IMAGE)-arm64 .
	$(CONTAINER_ENGINE) run --rm --platform linux/arm64 -v "$(CURDIR):/workspace" -w /workspace $(CONTAINER_RUST_IMAGE)-arm64 cargo test --workspace --locked

container-test-wasm:
	$(CONTAINER_ENGINE) build -f containers/Dockerfile.wasm-ci -t $(CONTAINER_WASM_IMAGE) .
	$(CONTAINER_ENGINE) run --rm -v "$(CURDIR):/workspace" -w /workspace $(CONTAINER_WASM_IMAGE) cargo check -p forge-wasm-api --target wasm32-unknown-unknown --locked

container-test-workbench:
	$(CONTAINER_ENGINE) build -f containers/Dockerfile.workbench -t $(CONTAINER_WORKBENCH_IMAGE) .
	$(CONTAINER_ENGINE) run --rm -v "$(CURDIR):/workspace" -w /workspace $(CONTAINER_WORKBENCH_IMAGE)
