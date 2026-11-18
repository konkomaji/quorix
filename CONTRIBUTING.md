# How to contribute to Quorix

Thank you for your interest in Quorix.
This file gives the rules for issues, code and pull requests.

All participants must obey the [Code of Conduct](CODE_OF_CONDUCT.md).

## Before you start

- For a defect, open an issue with the bug report form.
- For a new feature, open an issue first. Wait for agreement before you write the code.
- For a security problem, do not open an issue. Obey [SECURITY.md](SECURITY.md).

## Set up

1. Install Rust 1.85 or later from <https://rustup.rs>.
2. Install the tools for format and lint.

   ```sh
   rustup component add rustfmt clippy
   ```

3. Get the source code and build it.

   ```sh
   git clone https://github.com/konkomaji/quorix.git
   cd quorix
   cargo build --workspace
   ```

## Rules for code

- Correct behaviour is more important than speed. Speed is more important than new features.
- A download must not show as complete if data is missing or incorrect.
- Handle each error. Do not ignore a result that can fail.
- Do not use `unsafe` code. If you cannot avoid it, write the reason and the safety conditions next to the code.
- Do not start tasks without a limit.
- Do not write secrets or full links to logs or messages.
- Give each public item a documentation comment.
- Keep the engine independent of the user interface.

## Rules for tests

- Each change of behaviour must have a test.
- Each defect that you repair must have a test that fails without the repair.
- A failure test must also show that no file stays at the destination.
- Tests must give the same result each time. Do not use a real server in a test.

## Before you open a pull request

Run these three commands. All three must pass.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If you change `theme/quorix.css`, run this command:

```sh
node scripts/sync-theme.mjs
```

## Pull requests

A pull request must contain these items:

1. The problem that it solves.
2. A summary of the change.
3. The evidence from tests.
4. The risks.
5. The documentation changes, if the behaviour changes.

Keep each pull request small. One pull request must have one purpose.

## Commit messages

- Write the subject line as an instruction. Example: `Add retry limit for range requests`.
- Keep the subject line shorter than 72 characters.
- In the body, write why the change is necessary.

## Text in the interface and in documents

- Use short sentences.
- Use the active voice.
- Use one term for one thing.
- Tell the user what occurred and what to do.

## Licence of your contribution

Quorix has the licence GPL-3.0-or-later.
When you send a contribution, you agree that it has the same licence.
You must have the right to send the code that you contribute.
