# Contributing

## Development environment

| Component | Toolchain |
|---|---|
| Bot | Rust 1.78+, `cargo` |
| Contracts | Foundry (`forge`, `cast`) |
| Dashboard | Node.js 20+, npm |

Clone, then build each component per the [README](./README.md#build).

## Before opening a PR

Run the full local suite and make sure it passes:

```bash
cd bot && cargo fmt --check && cargo clippy -- -D warnings && cargo test -p kestrel
cd ../contracts && forge fmt --check && forge test -vv
cd ../dashboard && npm run build
```

## Code style

- Rust: `cargo fmt` defaults; no `clippy` warnings. Prefer explicit error handling over `unwrap()` in non-startup paths.
- Solidity: `forge fmt`; custom errors over string reverts; document every `assembly` block.
- Keep code comments about behavior, not change history — record the rationale for a change in its commit message, not inline.

## PR guidelines

- One logical change per PR, with a clear description of what and why.
- Include tests for any behavioral change. Contract changes need both unit and fuzz coverage.
- A change to the money path (spread detection, sizing, simulation, signing, submission, or any contract) must state how it was verified against a fork or live node.

## Commit messages

Use present-tense imperative subject lines (`fix premium-direction sizing`, not `fixed`). Explain the why in the body.
