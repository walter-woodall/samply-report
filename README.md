# samply-report

`perf report`-style TUI for [samply](https://github.com/mstange/samply) / Firefox Profiler profiles.

## Usage

```bash
# Interactive call tree (default)
cargo run --release -- /path/to/profile.json

# Metadata only
cargo run --release -- /path/to/profile.json --meta

# Text call tree (no TUI)
cargo run --release -- /path/to/profile.json --tree --expand-pct 1

# Pick a thread / skip symbols
cargo run --release -- profile.json -t 0 --no-symbols
```

Accepts plain JSON or gzip-compressed profiles.

### TUI keys

| Key | Action |
|-----|--------|
| `j` / `k` / arrows | Move |
| `e` | Expand current node (immediate children only) |
| `c` | Collapse current node (or parent if already collapsed) |
| `h` / `l` / ← / → | Scroll long symbol names horizontally |
| `/` | Filter by symbol substring |
| `q` / Esc | Quit |

Symbols stay space-indented by depth; use horizontal scroll when a line is wider than the terminal. With an active filter, matching frames are re-rooted at depth 0 and their children indent relative to that match, so deep hits are easier to browse.

## How it works

1. Parse the processed Firefox Profiler JSON
2. Collect unique `(lib, relative address)` frames
3. Symbolicate with [`wholesym`](https://crates.io/crates/wholesym) against `libs[].path`
4. Aggregate a top-down call tree keyed by **resolved symbol name**
5. Browse in ratatui (crossterm backend via ratatui defaults)
