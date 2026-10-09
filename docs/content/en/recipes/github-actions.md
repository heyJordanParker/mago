+++
title = "GitHub Actions recipe"
description = "Run formatting, linting, and analysis on every push and pull request."
nav_order = 50
nav_section = "Recipes"
+++
# GitHub Actions recipe

A simple workflow that runs the formatter, linter, and analyzer on every push and pull request, with native PR annotations.

## Quick setup

Create `.github/workflows/mago.yml`:

```yaml
name: Mago Code Quality

on:
  push:
  pull_request:

jobs:
  mago:
    name: Run Mago Checks
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v6

      - name: Set up Mago
        uses: nhedger/setup-mago@v2

      - name: Check formatting
        run: mago format --check

      - name: Lint
        if: success() || failure()
        run: mago lint

      - name: Analyze
        if: success() || failure()
        run: mago analyze
```

A few notes on the structure:

- Splitting `format`, `lint`, and `analyze` into separate steps surfaces findings from all three, even when an earlier step fails. A single combined `run:` would short-circuit on the first failure and hide the rest.
- `if: success() || failure()` runs the step when the job has not been cancelled, which is what you want here. `always()` would also run it after setup failures.
- Use `mago format --check`, not `--dry-run`. `--check` exits non-zero when files need formatting; `--dry-run` only prints a diff and always exits zero.
- Mago detects GitHub Actions through the `GITHUB_ACTIONS` environment variable and switches to `--reporting-format=github` automatically, producing native PR annotations. No extra configuration needed. On 1.17.0 and earlier you must pass `--reporting-format=github` to `mago lint` and `mago analyze` manually.