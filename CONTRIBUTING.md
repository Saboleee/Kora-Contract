# Contributing to Kora Protocol

Thank you for your interest in contributing to Kora. This is an open-source protocol with real-world impact — your contributions help close the trade finance gap for African SMEs. We hold contributions to a high standard because this code handles real money.

---

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [How to Contribute](#how-to-contribute)
- [Development Setup](#development-setup)
- [Branching Strategy](#branching-strategy)
- [Commit Conventions](#commit-conventions)
- [Pull Request Process](#pull-request-process)
- [Testing Requirements](#testing-requirements)
- [Security Vulnerabilities](#security-vulnerabilities)
- [Style Guide](#style-guide)

---

## Code of Conduct

This project follows the [Contributor Covenant](https://www.contributor-covenant.org/version/2/1/code_of_conduct/). By participating, you agree to uphold a respectful, inclusive environment. Harassment, discrimination, or bad-faith contributions will not be tolerated.

---

## How to Contribute

There are several ways to contribute:

- **Bug reports** — open a GitHub issue with a clear reproduction case
- **Feature proposals** — open a GitHub discussion before writing code for significant changes
- **Documentation** — improve clarity, fix typos, add examples
- **Contract improvements** — gas optimizations, security hardening, new features
- **Tests** — additional edge cases, fuzz tests, integration scenarios

For anything that changes protocol behavior or storage layout, open a discussion first. Breaking changes to deployed contracts require a migration plan.

---

## Development Setup

```bash
# 1. Fork and clone
git clone https://github.com/your-fork/kora-contract.git
cd kora-contract

# 2. Install toolchain
rustup target add wasm32-unknown-unknown
cargo install stellar-cli --locked

# 3. Build
make build

# 4. Run tests
make test

# 5. Lint
make lint
```

All of these must pass before opening a PR.

---

## Branching Strategy

| Branch | Purpose |
|--------|---------|
| `main` | Stable, audited code. Protected. |
| `develop` | Integration branch for upcoming releases. |
| `feat/<name>` | New features. Branch from `develop`. |
| `fix/<name>` | Bug fixes. Branch from `develop` (or `main` for hotfixes). |
| `chore/<name>` | Tooling, CI, docs. |

Never push directly to `main` or `develop`. All changes go through pull requests.

---

## Commit Conventions

We follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <short description>

[optional body]

[optional footer]
```

Types: `feat`, `fix`, `docs`, `test`, `refactor`, `chore`, `perf`, `security`

Scopes: `invoice_nft`, `marketplace`, `financing_pool`, `treasury`, `risk_registry`, `access_control`, `shared`, `scripts`, `docs`

Examples:

```
feat(marketplace): add partial funding support with deadline enforcement
fix(financing_pool): correct yield distribution rounding for odd share sizes
security(invoice_nft): add require_auth on set_listed caller
docs(README): add mainnet deployment instructions
```

---

## Pull Request Process

1. **Branch** from `develop` using the naming convention above.
2. **Write tests** for every change. New features need both unit and integration tests. Bug fixes need a regression test.
3. **Run the full suite** locally: `make fmt && make lint && make test`.
4. **Open the PR** against `develop`. Fill in the PR template completely.
5. **Request review** from at least one core maintainer.
6. **Address feedback** — do not force-push after review has started; add new commits instead.
7. **Squash on merge** — maintainers will squash your branch into a single clean commit on `develop`.

PRs that touch contract storage layout, fee logic, or access control require review from two maintainers and a security checklist sign-off.

### PR Template

```markdown
## Summary
<!-- What does this PR do? Why? -->

## Changes
<!-- List the files/contracts changed and what changed in each -->

## Testing
<!-- What tests were added or modified? How was this tested? -->

## Security Considerations
<!-- Does this change auth logic, storage, or fee handling? If so, explain. -->

## Breaking Changes
<!-- Does this change any public function signatures or storage keys? -->
```

---

## Testing Requirements

Every PR must maintain or improve test coverage. Specifically:

- **Unit tests** — every public contract function must have at least one happy-path and one failure-path test.
- **Integration tests** — any change to cross-contract interactions must be covered in `contracts/tests/`.
- **Edge cases** — zero amounts, expired timestamps, invalid scores, unauthorized callers, double-initialization.

Run tests with:

```bash
make test          # all tests
make test-verbose  # with stdout output
```

Tests must pass with `cargo clippy -- -D warnings` clean.

---

## Security Vulnerabilities

**Do not open a public GitHub issue for security vulnerabilities.**

Report security issues privately to: **security@kora.finance** (or the maintainer contact listed in the repository).

A log of past internal audit findings and their resolutions is maintained in [AUDIT_LOG.md](AUDIT_LOG.md).

Include:
- A description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

We will acknowledge within 48 hours and aim to patch within 7 days for critical issues.

See [docs/SECURITY.md](docs/SECURITY.md) for the full security policy.

---

## Style Guide

### Rust

- Follow `rustfmt` defaults (`make fmt` enforces this).
- No `unwrap()` in contract code — use `?` with typed errors from `KoraError`.
- No `panic!` in contract code — Soroban panics consume the entire transaction.
- Use `checked_add`, `checked_mul`, etc. for all arithmetic on financial values.
- All public functions must have a doc comment explaining parameters and failure modes.
- Storage keys must be defined in a `DataKey` enum using `#[contracttype]`.
- Events must be emitted via the `kora_shared::events` module — do not publish raw events inline.

### Documentation

- Write in plain English. Avoid jargon where a simpler word works.
- Code examples in docs must be runnable (or clearly marked as pseudocode).
- Keep line length under 100 characters in Markdown files.

---

## Recognition

All contributors are listed in [CONTRIBUTORS.md](CONTRIBUTORS.md). Significant contributions may be recognized with a protocol grant from the Kora Foundation.

---

*Built for African trade. Open to the world.*
