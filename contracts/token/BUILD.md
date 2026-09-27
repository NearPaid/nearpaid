# Building the coin contract

The coin is written directly against the NEAR host functions (no SDK) so the shared global
contract stays small.

Toolchain: Rust 1.93 with the `wasm32-unknown-unknown` target, and `wasm-opt` from
[binaryen](https://github.com/WebAssembly/binaryen).

```
cargo build --release --target wasm32-unknown-unknown
wasm-opt -Oz --enable-bulk-memory --enable-sign-ext --enable-mutable-globals \
  --enable-nontrapping-float-to-int --strip-debug --strip-producers \
  target/wasm32-unknown-unknown/release/nearpaid_token.wasm -o nearpaid_token.wasm
```

Compare the sha256 of the result (base58) with the global contract code hash in the README.
