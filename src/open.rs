use anyhow::{Context, Result, anyhow};
use std::process::{Command, Stdio};

pub fn open_url(url: &str) -> Result<()> {
    let status = match std::env::consts::OS {
        "macos" => Command::new("open").arg(url).status(),
        "windows" => Command::new("cmd")
            .arg("/C")
            .arg("start")
            .arg("")
            .arg(url)
            .status(),
        _ => Command::new("xdg-open").arg(url).status(),
    }
    .context("Failed to launch browser")?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("Failed to open URL in browser"))
    }
}

pub fn copy_to_clipboard(text: &str) -> Result<()> {
    match std::env::consts::OS {
        "macos" => pipe_to_clipboard(&mut Command::new("pbcopy"), text),
        "windows" => pipe_to_clipboard(&mut Command::new("clip"), text),
        _ => {
            // Prefer wl-copy (Wayland), fall back to xclip (X11).
            match pipe_to_clipboard(&mut Command::new("wl-copy"), text) {
                Ok(()) => Ok(()),
                Err(_) => pipe_to_clipboard(
                    Command::new("xclip").arg("-selection").arg("clipboard"),
                    text,
                )
                .context("Failed to copy to clipboard (xclip not installed)"),
            }
        }
    }
}

fn pipe_to_clipboard(command: &mut Command, text: &str) -> Result<()> {
    use std::io::Write;

    let mut child = command
        .stdin(Stdio::piped())
        .spawn()
        .context("Failed to spawn clipboard process")?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    let _ = child.wait();
    Ok(())
}
