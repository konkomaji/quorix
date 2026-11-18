## Problem

<!-- What problem does this change solve? Link the issue: Closes #123 -->

## Change

<!-- A short summary of what you changed. -->

## Test evidence

<!-- Which tests did you add or change? Paste the result of `cargo test --workspace`. -->

## Risk

<!-- What can go wrong? Does this change touch file integrity, resume, or security? -->

## Checklist

- [ ] `cargo fmt --all --check` passes.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes.
- [ ] `cargo test --workspace` passes.
- [ ] Each change of behaviour has a test.
- [ ] Each failure test shows that no file stays at the destination.
- [ ] The documentation agrees with the new behaviour.
- [ ] No secret, token or private link is in the code, the tests or the messages.
