# run-hidden-rs

A Rust port of [run-hidden](https://github.com/stax76/run-hidden): launch a
program with its console window hidden (on Windows), and get out of the way.

On top of the original's behavior it adds:

- **stdout/stderr export** — by default the child's output is captured and
  re-emitted on our own stdout/stderr (so you can pipe/redirect it). With
  `--stdout-path` / `--stderr-path` it is written straight to those files
  instead, and `--stdin-path` feeds the child's stdin from a file.
- **child cleanup on exit** — a cross-platform signal handler (Ctrl-C, SIGTERM,
  SIGHUP, console-close, ...) kills the child before we go, so nothing is left
  orphaned.
- **verbatim argv forwarding** — everything after `--` is passed straight to the
  child. Arguments are never joined into one string and split on spaces, so
  arguments containing spaces, quotes, or anything else survive untouched.

## Usage

```
run-hidden-rs [OPTIONS] -- <program> [args...]
```

Options (parsed from the part *before* `--`):

| Option | Default | Effect |
| --- | --- | --- |
| `--stdin-path <FILE>` | null device | Feed the child's stdin from `FILE`. |
| `--stdout-path <FILE>` | forward to our stdout | Write the child's stdout to `FILE`. |
| `--stderr-path <FILE>` | forward to our stderr | Write the child's stderr to `FILE`. |

Everything after `--` is the program and its arguments, forwarded verbatim.

### Examples

```
# Forward the child's output to our own stdout/stderr:
run-hidden-rs -- powershell -command calc.exe

# Redirect the child's stdout/stderr to log files:
run-hidden-rs --stdout-path out.log --stderr-path err.log -- some-program --its-own --flags
```

## Exit codes

The child's exit code is propagated. If the child is killed by a signal, the
exit code is `128 + signal` (Unix convention). `2` means no program was given,
`1` a stdio file could not be opened/created, `127` the program could not be
launched.

## Notes

- The hidden-window behavior (`CREATE_NO_WINDOW`) only applies on Windows. On
  other platforms the program simply runs as a normal child — the stdio
  forwarding/redirection and child-cleanup behaviors work everywhere.
- Prebuilt Windows binaries are attached to each tagged
  [release](https://github.com/wtdcode/run-hidden-rs/releases); every push is
  also built on Windows by CI.
