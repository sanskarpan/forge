.PHONY: test test-differential test-encoding bench qemu-aarch64 wasm workbench codesign spike

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
