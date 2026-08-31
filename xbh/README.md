# xbh - Interactive Bash History Search
A fast, interactive replacement for bash's Ctrl+R reverse history search, built with Go and tview.

<img width="1330" height="424" alt="image" src="https://github.com/user-attachments/assets/e202abae-95bc-4fbc-b84d-d55b65066d74" />


## Features

- **Fuzzy search**: Type anywhere in the command to find matches
- **Interactive TUI**: Real-time filtering as you type
- **Deduplication**: Shows only unique commands (most recent first)

## Installation

### Download pre-built binary

1. Go to the [releases page](https://github.com/zveinn/xbh/releases/latest)
2. Download the archive for your OS and architecture
3. Extract and install:

```bash
tar xzf xbh_*.tar.gz
sudo mv xbh /usr/local/bin/
```

### Build from source

```bash
go build -o xbh
sudo mv xbh /usr/local/bin/
```

Or install to your local bin:

```bash
go build -o xbh
mkdir -p ~/bin
mv xbh ~/bin/
# Make sure ~/bin is in your PATH
```

## Usage

Simply run:

```bash
xbh
```

To start directly in the bookmarks list:

```bash
xbh --bookmarks
```

### Keyboard shortcuts

- **Type**: Filter commands with fuzzy search
- **Enter**: Select current command (prints to stdout)
- **Up/Down** or **Ctrl+P/Ctrl+N**: Navigate through results
- **Esc**: Cancel and exit

## Bash Integration

To replace Ctrl+R with xbh, add this to your `~/.bashrc`:

### Option 1: Execute immediately (recommended)

```bash
# Bind Ctrl+R to xbh - auto-execute selected command
__xbh() {
    history -w
    local selected
    selected=$(xbh)
    if [ -n "$selected" ]; then
        history -s "$selected"  # Add to history
        eval "$selected"         # Execute immediately
    fi
}

[[ $- == *i* ]] && bind -x '"\C-r": __xbh'
```

### Option 2: Insert into command line (edit before running)

```bash
# Bind Ctrl+R to xbh - insert into readline buffer

__xbh() {
    history -w
    local selected
    selected=$(xbh)
    if [ -n "$selected" ]; then
        READLINE_LINE="$selected"
        READLINE_POINT=${#READLINE_LINE}
    fi
}

[[ $- == *i* ]] && bind -x '"\C-r": __xbh'
```

After adding this, reload your bashrc:

```bash
source ~/.bashrc
```

Now pressing Ctrl+R will launch xbh instead of the default reverse search!

### Accessing bookmarks directly with Ctrl+B

To launch xbh straight into the bookmarks list (using the `--bookmarks` flag), bind it to Ctrl+B. You can keep your Ctrl+R binding for normal history search at the same time.

#### Option 1: Execute immediately (recommended)

```bash
# Bind Ctrl+B to xbh --bookmarks - auto-execute selected bookmark
__xbh_bookmarks() {
    history -w
    local selected
    selected=$(xbh --bookmarks)
    if [ -n "$selected" ]; then
        history -s "$selected"  # Add to history
        eval "$selected"         # Execute immediately
    fi
}

[[ $- == *i* ]] && bind -x '"\C-b": __xbh_bookmarks'
```

#### Option 2: Insert into command line (edit before running)

```bash
# Bind Ctrl+B to xbh --bookmarks - insert into readline buffer
__xbh_bookmarks() {
    history -w
    local selected
    selected=$(xbh --bookmarks)
    if [ -n "$selected" ]; then
        READLINE_LINE="$selected"
        READLINE_POINT=${#READLINE_LINE}
    fi
}

[[ $- == *i* ]] && bind -x '"\C-b": __xbh_bookmarks'
```

After adding (or updating) your bindings, reload your bashrc:

```bash
source ~/.bashrc
```

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

