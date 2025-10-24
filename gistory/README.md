# Gistory - Interactive Bash History Search

A fast, interactive replacement for bash's Ctrl+R reverse history search, built with Go and tview.

## Features

- **Fuzzy search**: Type anywhere in the command to find matches
- **Interactive TUI**: Real-time filtering as you type
- **Keyboard navigation**: Arrow keys, Vim-style keys (Ctrl+N/Ctrl+P)
- **Deduplication**: Shows only unique commands (most recent first)
- **Fast**: Written in Go for optimal performance

## Installation

### Build from source

```bash
go build -o gistory
sudo mv gistory /usr/local/bin/
```

Or install to your local bin:

```bash
go build -o gistory
mkdir -p ~/bin
mv gistory ~/bin/
# Make sure ~/bin is in your PATH
```

## Usage

Simply run:

```bash
gistory
```

### Keyboard shortcuts

- **Type**: Filter commands with fuzzy search
- **Enter**: Select current command (prints to stdout)
- **Up/Down** or **Ctrl+P/Ctrl+N**: Navigate through results
- **Esc**: Cancel and exit

## Bash Integration

To replace Ctrl+R with gistory, add this to your `~/.bashrc`:

### Option 1: Execute immediately (recommended)

```bash
# Bind Ctrl+R to gistory - auto-execute selected command
bind -x '"\C-r": __gistory'

__gistory() {
    local selected
    selected=$(gistory)
    if [ -n "$selected" ]; then
        history -s "$selected"  # Add to history
        eval "$selected"         # Execute immediately
    fi
}
```

### Option 2: Insert into command line (edit before running)

```bash
# Bind Ctrl+R to gistory - insert into readline buffer
bind -x '"\C-r": __gistory'

__gistory() {
    local selected
    selected=$(gistory)
    if [ -n "$selected" ]; then
        READLINE_LINE="$selected"
        READLINE_POINT=${#READLINE_LINE}
    fi
}
```

After adding this, reload your bashrc:

```bash
source ~/.bashrc
```

Now pressing Ctrl+R will launch gistory instead of the default reverse search!

## How it works

1. Reads commands from `~/.bash_history`
2. Deduplicates commands (keeps most recent)
3. Provides interactive fuzzy search interface
4. Outputs selected command to stdout
5. Bash integration either executes it immediately or inserts it into your command line

## Requirements

- Go 1.16 or higher (for building)
- Bash with `bind -x` support (most modern versions)
- Terminal with color support

## License

MIT
