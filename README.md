# pg-studio 🚀

`pg-studio` is a production-ready, publishable Rust CLI tool that automates the
process of reaching a database (SSH tunnel, direct URL, or a local file),
introspecting its schema, and launching
[Drizzle Studio](https://orm.drizzle.team/drizzle-studio/overview) — all without
requiring an existing Drizzle codebase.

It supports several engines out of the box:

| Engine | How you connect |
|---|---|
| **PostgreSQL** | SSH tunnel, direct URL, or localhost |
| **SQLite** | Any local `.db`/`.sqlite` file — including wrangler dev D1 state (`.wrangler/state/v3/d1/**/*.sqlite`) |
| **Cloudflare D1** | Remote database via `d1-http` (account ID + database ID + API token) |
| **Turso** | Remote libsql database via `libsql://…` URL + auth token |
| **MySQL** | SSH tunnel, direct URL, or localhost |

If you have a remote Postgres database (e.g. on AWS EC2, DigitalOcean, or
anywhere else) and want to visually explore your data using Drizzle Studio over
an SSH tunnel, `pg-studio` handles the entire lifecycle for you.

## Features ✨

- **Multi-Engine Support**: PostgreSQL, SQLite (local files & wrangler D1 dev
  databases), Cloudflare D1, Turso and MySQL — each with engine-specific forms,
  reachability checks, and drizzle config generation.
- **Lazygit-Style TUI Dashboard**: Interactive 3-pane terminal UI powered by
  `ratatui` (Projects List, Project Config, Logs & Field Guides).
- **Multi-Project Support**: Manage multiple database connections sorted
  automatically by your most recent usage.
- **Native OS Keychain Security**: Passwords, Cloudflare API tokens and Turso
  auth tokens are securely stored in your OS's native secure store (macOS
  Keychain, Linux Secret Service, Windows Credential Manager) — never on disk.
- **Automated SSH Tunneling**: Dynamically binds to a free local port and
  strictly manages the SSH process lifecycle (automatically cleaning up the
  background process when the app closes).
- **Zero-Config Drizzle Integration**: Automatically provisions an isolated Node
  workspace per project, installs the right driver for your engine (`pg`,
  `mysql2`, `better-sqlite3`, `@libsql/client`), generates configs that read
  credentials from environment variables, pulls your schema, sanitizes any
  unsupported Postgres data types (e.g. `bytea`), and launches the Studio.
- **In-App Self-Updating (`u` key)**: Press `u` anywhere in the TUI to
  automatically check for and install the latest binary releases directly from
  GitHub. The update runs in the background with a live progress popup - the
  TUI stays usable and you can cancel between steps.
- **Database Dump & Safe Restore** *(PostgreSQL projects)*: Back up a project's
  database via `pg_dump` (custom `.dump` or plain `.sql`), and restore a backup
  into a project's database (`restore-db`). Restores always take a fresh safety
  backup of the target database first and keep it even if the restore fails.
- **25 Themes, Always Readable**: Catppuccin, Tokyo Night, Dracula, Gruvbox,
  Nord, Solarized, One Dark, GitHub, Rosé Pine, Everforest, Kanagawa, Monokai —
  switch with `--theme` or the `T` key. Every color is contrast-checked against
  the theme's own background, so text, selected rows and popups stay readable
  on dark *and* light terminals (and on your terminal's own palette).

## Installation 📦

### Option 1: Install Pre-compiled Binary

Download and install the latest pre-compiled binary directly from
[GitHub Releases](https://github.com/ujjwal6792/pg-studio/releases)
(pick the archive for your platform):

```bash
# Apple Silicon Mac
curl -L https://github.com/ujjwal6792/pg-studio/releases/latest/download/pg-studio-v0.5.5-aarch64-apple-darwin.tar.gz | tar -xz
# Intel Mac
curl -L https://github.com/ujjwal6792/pg-studio/releases/latest/download/pg-studio-v0.5.5-x86_64-apple-darwin.tar.gz | tar -xz
# Linux (x86_64)
curl -L https://github.com/ujjwal6792/pg-studio/releases/latest/download/pg-studio-v0.5.5-x86_64-unknown-linux-gnu.tar.gz | tar -xz
# Windows (x86_64, PowerShell): download and extract
# pg-studio-v0.5.5-x86_64-pc-windows-msvc.tar.gz, then move pg-studio.exe
# somewhere on your PATH.
sudo mv pg-studio /usr/local/bin/
```

### Option 2: Install via Cargo (GitHub)

To build and install a specific tagged release directly via Cargo:

```bash
cargo install --git https://github.com/ujjwal6792/pg-studio --tag v0.5.5 --force
```

Or install the latest edge version straight from the `main` branch:

```bash
cargo install --git https://github.com/ujjwal6792/pg-studio --force
```

### Option 3: Install via Crates.io (When published)

```bash
cargo install pg-studio --version 0.5.5
```

## Prerequisites

- **Rust/Cargo**: To build and install the tool (if installing via Cargo).
- **Node.js (`npm` & `npx`)**: Required under-the-hood to fetch and run
  `drizzle-kit`.
- **SSH Client**: Standard `ssh` must be available in your terminal.

## Usage & Navigation 💻

Simply run:

```bash
pg-studio
```

### CLI Arguments

- **`pg-studio`** (no args): Launch the interactive TUI.
- **`pg-studio --check` / `-c`**: Check GitHub for the latest release without
  installing.
- **`pg-studio --update` / `-u`**: Check for and install the latest GitHub
  release, then exit (shows live progress).
- **`pg-studio backup [FILE]`**: Password-free JSON backup of all projects.
- **`pg-studio restore FILE`**: Import projects from such a backup.
- **`pg-studio new`**: Create a project non-interactively, e.g.:
  - `pg-studio new --type ssh --ssh ubuntu@host -d app -u admin --password-stdin`
  - `pg-studio new --engine sqlite --file ~/data/app.db`
  - `pg-studio new --engine d1 --account-id <id> --database-id <id> --password-stdin`
  - `pg-studio new --engine turso --url libsql://acme.turso.io --password-stdin`
- **`pg-studio test <PROJECT>`**: Check reachability without launching.
- **`pg-studio start <PROJECT> [--open]`**: Launch detached and print the
  Studio URL.
- **`pg-studio dump <PROJECT> [-o FILE]`**: Database dump via `pg_dump`
  (PostgreSQL projects only).
- **`pg-studio restore-db <PROJECT> <FILE>`**: Restore a `.dump`/`.sql`
  backup into the project's database after taking a mandatory safety backup
  (PostgreSQL projects only).
- **`pg-studio --version` / `-v` / `-V`**: Print the current version.
- **`pg-studio --help` / `-h`**: Print usage help.
- **`pg-studio --theme <NAME>`**: Pick a TUI color theme and save it as the
  default (see [Themes](#themes-) below).

### Themes 🎨

pg-studio ships 25 themes — Catppuccin (Mocha, Latte, Frappé, Macchiato),
Tokyo Night (Night, Storm, Light), Dracula, Gruvbox (Dark/Light), Nord,
Solarized (Dark/Light), One Dark, GitHub (Dark/Light), Rosé Pine (Pine,
Moon, Dawn), Everforest (Dark/Light), Kanagawa and Monokai — plus
`terminal`, which follows the colors of your own terminal (detected via
OSC 10/11/4). Every theme is contrast-checked against its own background,
so text, status colors, selected rows and popups stay readable on both dark
and light terminals.

```bash
pg-studio --theme catppuccin-mocha   # switch and remember
pg-studio --theme terminal          # go back to following your terminal
```

Run `pg-studio --help` for the full list of theme names.

### Keyboard Navigation in TUI:

- **`Tab`**: Switch focus between **Projects List**, **Project Form**, and
  **Logs**.
- **`n`**: Create a **New Project**.
- **`e`**: **Edit** the selected project.
- **`d` / `Backspace`**: **Delete** the selected project (with confirmation
  modal).
- **`Enter`**:
  - On Projects List: **Launch SSH Tunnel & Drizzle Studio**.
  - On Project Form: **Save Project**.
- **`u`**: **Self-Update** (fetches & installs the latest GitHub binary
  release).
- **`Shift+T`**: **Choose a Color Theme**. Up/Down previews, Enter saves, and
  Esc cancels and restores the previous theme.
- **`g`**: **Toggle the theme background** on/off. The choice is saved in the
  config file; background fill is on by default.
- **`q` / `Esc`**: **Quit** (with confirmation modal).

## License

MIT
