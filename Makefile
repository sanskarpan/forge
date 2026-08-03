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
	@echo "not yet implemented — Phase 11"

test-encoding:
	@echo "not yet implemented — Phase 6"

bench:
	@echo "not yet implemented — Phase 16"

qemu-aarch64:
	@echo "not yet implemented — Phase 9"

wasm:
	@echo "not yet implemented — Phase 14"

workbench:
	@echo "not yet implemented — Phase 15"
