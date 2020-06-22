all:
	cargo build

check:
	cargo fmt -- --check
	cargo clippy
	cargo test

clean:
	git clean -ffxd
