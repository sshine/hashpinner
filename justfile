# Default recipe: list available commands
default:
    @just --list

# Format all code (Rust + Nix + Markdown)
fmt:
    treefmt

# Check formatting (Rust + Nix + Markdown)
fmt-check:
    treefmt --fail-on-change --no-cache

# Run clippy lints
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Run all tests
test:
    cargo test --all-features

# Run tests with verbose output
test-verbose:
    cargo test --all-features -- --nocapture

# Build release
build:
    cargo build --release --all-features

# Generate documentation
doc *args='':
    cargo doc --no-deps --all-features {{args}}

readme_args := "--project-root crates/hashpinner-cli --no-title --no-license --no-badges"

# Regenerate README.md from the hashpinner-cli crate docs
readme:
    cargo readme {{readme_args}} -o README.md

# Check README.md is in sync with the crate docs
readme-check:
    cargo readme {{readme_args}} | diff - README.md

release_targets := "x86_64-unknown-linux-musl aarch64-unknown-linux-musl"

# Build the static release artifacts and their checksums into dist/
dist version:
    rm -rf dist && mkdir -p dist
    for target in {{release_targets}}; do \
        cargo build --release --locked --target "$target" -p hashpinner-cli; \
        name="hashpinner-{{version}}-$target"; \
        tar -czf "dist/$name.tar.gz" -C "target/$target/release" hashpinner; \
        ( cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256" ); \
    done
    @ls -1 dist

# Check this repository's own workflows are pinned
selfcheck: build
    ./target/release/hashpinner --check --deep

# Run CI checks locally
ci: fmt-check lint test doc readme-check build
    @echo "All CI checks passed!"

# Watch for changes and run tests
watch:
    cargo watch -x test

# Clean build artifacts
clean:
    cargo clean

# Review snapshot test changes
snap:
    cargo insta test --review
