# pg-studio

`pg-studio` is a production-ready, publishable Rust CLI tool that automates the
incredibly tedious process of SSH tunneling into a remote server, introspecting
a Postgres database, and launching
[Drizzle Studio](https://orm.drizzle.team/drizzle-studio/overview) — all without
requiring an existing Drizzle codebase.

If you have a remote Postgres database (e.g. on AWS EC2, DigitalOcean, or
anywhere else) and want to visually explore your data using the beautiful
Drizzle Studio interface over an SSH tunnel, `pg-studio` handles the entire
lifecycle for you.

## Features

- **Beautiful Terminal UI**: Rich interactive prompts (powered by `inquire`) to
  configure your database connection.
- **Smart Config Management**: Remembers your SSH strings, ports, and usernames,
  while strictly _never_ saving your password to disk.
- **Automated SSH Tunneling**: Dynamically binds to a free local port and
  strictly manages the SSH process lifecycle (automatically cleaning up the
  background process when the app closes).
- **Zero-Config Drizzle Integration**: Automatically provisions an isolated Node
  workspace, installs Drizzle, generates configs, pulls your schema, sanitizes
  any unsupported Postgres data types (e.g. `bytea`), and launches the Studio.

## Installation

### Via GitHub

Since this project is currently in early development, installing directly from GitHub is the easiest method.

**Option 1: Install a specific Beta release (Recommended)**
To install a specific version that has been tagged as a release, use the `--tag` flag.
```bash
cargo install --git https://github.com/YOUR_USERNAME/pg-studio --tag v0.1.0-beta.1
```
*(To update an existing installation to a newer beta, simply append the `--force` flag).*

**Option 2: Install the latest edge version (main branch)**
If you want the absolute latest features and fixes straight from the `main` branch, omit the tag:
```bash
cargo install --git https://github.com/YOUR_USERNAME/pg-studio --force
```

### Via Crates.io (When published)

Once published to `crates.io`, you can install the beta explicitly:

```bash
cargo install pg-studio --version 0.1.0-beta.1
```

*(Or simply `cargo install pg-studio` once the `1.0.0` stable release drops!)*

## Prerequisites

- **Rust/Cargo**: To build and install the tool.
- **Node.js (`npm` & `npx`)**: Required under-the-hood to fetch and run
  `drizzle-kit`.
- **SSH Client**: Standard `ssh` must be available in your terminal.

## Usage

Simply run the tool in your terminal:

```bash
pg-studio
```

You will be greeted with an interactive prompt:

1. **SSH Connection String**: e.g., `ubuntu@192.168.1.5`
2. **Remote Database Port**: Usually `5432` for Postgres.
3. **Database Name**: The name of the database you want to introspect.
4. **Database Username**: Your Postgres user.
5. **Database Password**: Your Postgres password (masked in the terminal and
   never saved).

`pg-studio` will then:

1. Establish a secure SSH tunnel in the background.
2. Initialize an isolated workspace in your OS's data directory.
3. Introspect your database schema.
4. Launch Drizzle Studio in your browser on `https://local.drizzle.studio`.

To cleanly exit and tear down the SSH tunnel, simply press `Ctrl+C` or exit
Drizzle Studio.

## License

MIT
