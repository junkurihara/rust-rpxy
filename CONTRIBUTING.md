# Contributing to rust-rpxy

Thank you for your interest in contributing to rust-rpxy!
This project is maintained primarily based on the code owner's personal interests.
It is not backed by any commercial contract or SLA. Contributions are therefore handled on a **best-effort basis**.

---

## Ways to contribute

### 1. Reporting issues

- Use the appropriate Issue template (Bug, Feature request, or Question).
- For questions, please also consider using **GitHub Discussions** instead of Issues.
- Please provide as much detail as possible (logs, configs, environment) to make the report useful.

### 2. Suggesting new features

- Feature requests should explain **why** the feature is needed and what alternatives exist.
- Unclear or vague requests may be closed without action.
- Features that introduce compatibility problems or are outside the project scope are unlikely to be accepted.
- If the feature is critical for your environment, please consider:
  - Implementing it in your own fork, or
  - Sponsoring its development.

### 3. Submitting code (Pull Requests)

- Contributions are welcome! Please:
  - Ensure code is formatted with `cargo fmt`.
  - Run tests with `cargo test` and confirm they pass.
  - Keep commits clean and focused.
- **Large behavioral or architectural changes should be discussed in an Issue or Discussion before a PR is submitted.**
- **A large PR without prior design agreement may be closed even if the implementation itself is substantial.**

### 4. AI-assisted contributions

AI-assisted contributions are generally allowed, but **the contributor is fully responsible for the submitted changes**. Contributors are expected to **minimize unnecessary maintainer burden** by providing clear rationale, focused changes, and sufficient code-level explanation of the changed parts for review.

Please do not submit generated patches that you cannot personally explain and justify. You should understand every non-trivial part of the change, including its design rationale, behavior, interactions with existing features, and trade-offs.

Large AI-generated or AI-assisted patches are not acceptable as a substitute for prior design discussion. Changes that affect core proxy behavior, configuration semantics, load balancing, health checks, caching, TLS handling, request/response streaming, or other major components must be discussed first in an Issue or Discussion before submitting an implementation PR.

Maintainers should not be asked to reverse-engineer, validate, or debug a large generated patch without a clear design rationale and prior agreement on the direction.

### 5. Documentation improvements

- Fixing typos, clarifying explanations, and improving examples are always appreciated.

### 6. Security reports

- Do **not** open public Issues for security vulnerabilities.
- Please report them via [GitHub's Private vulnerability reporting](../../security/advisories/new).
- Reports are handled confidentially and on a best-effort basis.

---

## Code of Conduct

Be respectful and constructive.
Aggressive, entitled, or hostile communication may result in your Issue or PR being closed without further discussion.

---

## Final notes

- This project exists because of personal research and interests of the maintainer.
- Time and resources are limited. Contributions that come with clear motivation, good context, or sponsorship are far more likely to be accepted.
- Thank you for understanding and for supporting rust-rpxy!
