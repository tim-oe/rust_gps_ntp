# Rust Error Handling (Java Mapping)

This project uses a consistent error-handling pattern that maps cleanly from a
Java mindset while keeping Rust idiomatic.

## Java -> Rust mental model

- Java exceptions (`throw`/`catch`) -> Rust `Result<T, E>` + `?` + boundary `match`
- Java nullable values -> Rust `Option<T>`
- "Propagate error to caller" -> `?`
- "Best effort, keep running" -> handle at boundary, log, and continue

## Project pattern

Use this structure throughout firmware code:

1. **Low-level I/O and drivers**
   - Return `Result<T, E>` (often `anyhow::Result<T>`).
   - Add context to failures.
   - Use `?` for concise propagation.

2. **Mid-level composition**
   - Keep chaining with `?`.
   - Avoid repetitive `if` checks.

3. **Boundary loops/tasks (`main`)**
   - Handle once with `match`/`if let Err`.
   - Log + fallback/retry as needed.
   - Keep device running unless the failure is fatal.

## Current repository verification

### Follows the pattern

- `src/battery.rs`
  - sensor reads use `anyhow::Result<_>` and `?`
  - I2C errors include context (register/chip)
- `src/wifi.rs`
  - setup path uses `Result + ?` with contextual failures
- `src/ntp.rs`
  - socket operations use `Result + ?`
  - caller boundary handles poll errors in `main`
- `src/main.rs`
  - initialization uses `?` for fatal setup failures
  - runtime boundaries log non-fatal failures and continue

### Intentional `Option` use (not treated as hard error)

- `src/gps.rs`
  - NMEA parsing returns `Option` because malformed/incomplete sentences are
    expected during serial stream parsing and are treated as "skip sample".
- `battery::detect_monitor(...)`
  - probe function returns `Option` because "no monitor found" is a valid state.

### Best-effort operations now explicitly logged

Silent drops were removed for key UI operations:

- button pull-up enable in `main`
- display backlight toggles in `main` and `display::init_display`
- display boot-test draw operations in `display`

Each now logs on failure instead of discarding errors.

## Practical guideline

When adding new code:

- Use `Result` for hardware, network, filesystem, and protocol operations.
- Reserve `Option` for truly optional data/absence, not operational failures.
- Decide once at the caller boundary whether to:
  - fail fast (`?`), or
  - log and continue (`match` / `if let Err`).
