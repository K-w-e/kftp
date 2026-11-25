# kftp

A dual-pane terminal client for SFTP, FTP, and FTPS inspired by FileZilla. Built with Rust and Ratatui.

## Features

- **Multi-protocol support**: connects over SFTP (SSH via `ssh2`), plain FTP, and FTPS (explicit TLS via `suppaftp` + `native-tls`).
- **Flexible authentication**: supports password authentication, SSH private key files (with optional passphrase), and SSH agent forwarding. Secrets are only written to disk when explicitly opted in; otherwise they are prompted at runtime.
- **Dual-pane file manager**: local filesystem on the left, remote filesystem on the right, with draggable mouse splitter or keyboard resizing (`[` / `]`).
- **Site Manager**: save, edit, delete, and quickly connect to server profiles with default local and remote starting directories.
- **Built-in file preview**: inspect local or remote files (up to 64 KB) without downloading them. Includes syntax highlighting and formatting for JSON, JSONL/NDJSON, XML/HTML, CSV/TSV (aligned table), TOML/INI, YAML, Markdown, logs, source code, plain text, and hex dump for binary files.
- **Transfer queue and activity log**: background worker thread handles uploads and downloads with progress reporting, alongside a toggleable connection log panel (`t`).
- **File operations**: create directories (`m`), rename (`r`), delete (`d`), jump to any path (`p`), and toggle hidden files (`.`) on both local and remote panes.
- **Full mouse support**: left-click to select or focus panes, double-click to open directories or start transfers, right-click to preview a file, scroll wheel for lists and previews, and drag-and-drop on the vertical divider to resize panes.

## Installation

### Prerequisites

- Rust 1.85 or newer (edition 2024)
- System OpenSSL / libssh2 build dependencies if required by your platform (macOS uses system libraries automatically)

### Build from source

```sh
cargo build --release
```

The compiled binary is written to `target/release/kftp`.

Run directly with Cargo:

```sh
cargo run
```

## Configuration

Saved connection profiles are stored in TOML format with owner-only permissions (`0600` on Unix):

- macOS: `~/Library/Application Support/kftp/connections.toml`
- Linux: `~/.config/kftp/connections.toml`
- Windows: `%APPDATA%\kftp\connections.toml`

If `save_secret` is `false` (the default), the password or key passphrase is stripped before saving and prompted interactively when connecting.

## Controls

### Navigation and panes

| Key | Action |
|---|---|
| `Tab` / `Left` / `Right` / `h` / `l` | Switch focus between Local and Remote panes |
| `Up` / `Down` / `j` / `k` | Move selection |
| `PgUp` / `PgDn` / `g` / `G` / `Home` / `End` | Jump in file list |
| `Enter` | Open directory or transfer selected file |
| `Backspace` | Go to parent directory |
| `p` | Go to a specific local or remote path |
| `.` | Toggle hidden files |
| `R` / `F5` | Refresh both panes |
| `[` / `]` | Resize left pane narrower / wider |

### Files, preview, and transfers

| Key | Action |
|---|---|
| `u` | Upload (from Local pane) or Download (from Remote pane) |
| `v` / `Space` | Open file preview (up to 64 KB) |
| `m` | Create a new directory in the active pane |
| `r` | Rename selected file or directory |
| `d` / `Delete` | Delete selected file or directory (with confirmation) |
| `t` | Switch bottom panel between Transfers and Log |
| `c` | Clear completed and failed transfers |

### Preview modal

| Key | Action |
|---|---|
| `j` / `k` / `Up` / `Down` / `PgUp` / `PgDn` / `g` / `G` | Scroll vertically |
| `h` / `l` / `Left` / `Right` | Scroll horizontally |
| `f` | Toggle formatted / raw view (JSON, JSONL, XML, CSV) |
| `w` | Toggle line wrapping |
| `u` / `Enter` | Transfer the previewed file |
| `Esc` / `q` / `v` / `Space` | Close preview |

### Connections

| Key | Action |
|---|---|
| `s` | Open Site Manager (`n` new, `e` edit, `d` delete, `Enter` connect) |
| `n` | Open New Connection form (`Ctrl+S` saves, `Enter` saves and connects) |
| `D` | Disconnect from the remote server |
| `?` / `F1` | Open help overlay |
| `q` / `Ctrl+C` | Quit |

## Architecture

```
kftp/
├── src/
│   ├── main.rs      # Terminal setup and main event loop
│   ├── app.rs       # Application state, modals, keyboard and mouse dispatch
│   ├── ui.rs        # Ratatui dual-pane layout, splitter, modals, preview rendering
│   ├── remote.rs    # Background worker thread for SFTP, FTP, and FTPS operations
│   ├── preview.rs   # Format detection, pretty-printing, syntax highlighting, hex viewer
│   ├── fs_model.rs  # Local/remote entry types, sorting, and path helpers
│   └── config.rs    # Connection profiles and TOML persistence
└── Cargo.toml
```
