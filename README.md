# pg-studio 🚀

`pg-studio` is a production-ready, publishable Rust CLI tool that automates the
process of SSH tunneling into a remote server, introspecting a Postgres
database, and launching
[Drizzle Studio](https://orm.drizzle.team/drizzle-studio/overview) — all without
requiring an existing Drizzle codebase.

If you have a remote Postgres database (e.g. on AWS EC2, DigitalOcean, or
anywhere else) and want to visually explore your data using Drizzle Studio over
an SSH tunnel, `pg-studio` handles the entire lifecycle for you.

## Features ✨

- **Lazygit-Style TUI Dashboard**: Interactive 3-pane terminal UI powered by
  `ratatui` (Projects List, Project Config, Logs & Field Guides).
- **Multi-Project Support**: Manage multiple database connections sorted
  automatically by your most recent usage.
- **Native OS Keychain Security**: Passwords are securely stored in your OS's
  native secure store (macOS Keychain, Linux Secret Service, Windows Credential
  Manager).
- **Automated SSH Tunneling**: Dynamically binds to a free local port and
  strictly manages the SSH process lifecycle (automatically cleaning up the
  background process when the app closes).
- **Zero-Config Drizzle Integration**: Automatically provisions an isolated Node
  workspace, installs Drizzle, generates configs, pulls your schema, sanitizes
  any unsupported Postgres data types (e.g. `bytea`), and launches the Studio.
- **In-App Self-Updating (`u` key)**: Press `u` anywhere in the TUI to
  automatically check for and install the latest binary releases directly from
  GitHub.

## Installation 📦

### Option 1: Install Pre-compiled Binary (Apple Silicon Mac)

Download and install the latest pre-compiled binary directly from
[GitHub Releases](https://github.com/ujjwal6792/pg-studio/releases):

```bash
curl -L https://github.com/ujjwal6792/pg-studio/releases/latest/download/pg-studio-v0.2.0-beta.2-aarch64-apple-darwin.tar.gz | tar -xz
sudo mv pg-studio /usr/local/bin/
```

### Option 2: Install via Cargo (GitHub)

To build and install a specific tagged release directly via Cargo:

```bash
cargo install --git https://github.com/ujjwal6792/pg-studio --tag v0.2.0-beta.2 --force
```

Or install the latest edge version straight from the `main` branch:

```bash
cargo install --git https://github.com/ujjwal6792/pg-studio --force
```

### Option 3: Install via Crates.io (When published)

```bash
cargo install pg-studio --version 0.2.0-beta.2
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
  release, then exit.
- **`pg-studio --version` / `-v` / `-V`**: Print the current version.
- **`pg-studio --help` / `-h`**: Print usage help.

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
- **`q` / `Esc`**: **Quit** (with confirmation modal).

## License

MIT
