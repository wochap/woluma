//! End-to-end shell completion checks against the built binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("woluma-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("cfg/woluma")).unwrap();
    std::fs::create_dir_all(dir.join("run")).unwrap();
    dir
}

/// `woluma` with a temp config home, an empty runtime dir (no daemon) and no `HOME` fallback.
fn woluma(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_woluma"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("XDG_CONFIG_HOME", dir.join("cfg"))
        .env("XDG_RUNTIME_DIR", dir.join("run"));
    command
}

fn with_config(tag: &str) -> PathBuf {
    let dir = tmp(tag);
    std::fs::write(
        dir.join("cfg/woluma/config.toml"),
        "[outputs.eDP-1]\n[outputs.DP-1]\n",
    )
    .unwrap();
    dir
}

fn stdout(output: Output) -> String {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn candidates(dir: &Path, shell: &str, words: &[&str]) -> Vec<String> {
    let mut command = woluma(dir);
    command
        .env("COMPLETE", shell)
        .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
        .arg("--")
        .args(words);
    if shell == "bash" {
        command
            .env("_CLAP_COMPLETE_COMP_TYPE", "9")
            .env("_CLAP_COMPLETE_SPACE", "false")
            .env("_CLAP_IFS", "\n");
    } else {
        command.env("_CLAP_IFS", "\n");
    }
    stdout(command.output().unwrap())
        .lines()
        .map(|line| line.split(':').next().unwrap().to_owned())
        // clap also offers the subcommand's flags on an empty word; only values matter here.
        .filter(|line| !line.is_empty() && !line.starts_with('-'))
        .collect()
}

#[test]
fn output_names_complete_from_config() {
    let dir = with_config("names");
    for shell in ["zsh", "bash"] {
        assert_eq!(
            candidates(&dir, shell, &["woluma", "toggle", ""]),
            ["DP-1", "eDP-1", "*"],
            "{shell}"
        );
    }
}

#[test]
fn no_config_offers_only_star() {
    let dir = tmp("star");
    assert_eq!(candidates(&dir, "zsh", &["woluma", "resume", ""]), ["*"]);
}

#[test]
fn duration_offers_nothing() {
    let dir = with_config("duration");
    assert!(candidates(&dir, "zsh", &["woluma", "pause", "eDP-1", ""]).is_empty());
}

#[test]
fn script_needs_no_config_or_daemon() {
    let dir = tmp("script");
    for shell in ["zsh", "bash", "fish"] {
        let script = stdout(woluma(&dir).args(["completions", shell]).output().unwrap());
        assert!(script.contains("woluma"), "{shell}");
    }
    assert!(!woluma(&dir)
        .args(["completions", "powershell-legacy"])
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn zsh_script_autoloads_from_fpath() {
    if Command::new("zsh").arg("-fc").arg("true").output().is_err() {
        eprintln!("zsh not installed; skipping");
        return;
    }
    let dir = tmp("fpath");
    let script = stdout(woluma(&dir).args(["completions", "zsh"]).output().unwrap());
    let functions = dir.join("functions");
    std::fs::create_dir_all(&functions).unwrap();
    std::fs::write(functions.join("_woluma"), script).unwrap();
    let check = format!(
        "fpath=({} $fpath); autoload -U compinit; compinit -u -D; print -r -- $_comps[woluma]",
        functions.display()
    );
    let output = Command::new("zsh")
        .env("HOME", &dir)
        .args(["-f", "-c", &check])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "_woluma",
        "{output:?}"
    );
}
