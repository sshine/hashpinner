#!/usr/bin/env bash

export PATH="$PWD/target/release:$PATH"
export PS1='\[\033[38;5;213m\]❯\[\033[0m\] '
export GIT_TERMINAL_PROMPT=0

demo=$(mktemp -d)
mkdir -p "$demo/.github/workflows"

cat > "$demo/.github/workflows/ci.yml" << 'YAML'
name: CI
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo test
YAML

cd "$demo" || exit 1
