# Shell Completions Specification

## Purpose

Defines shell tab completion for the `woluma` CLI: the `woluma completions <shell>` subcommand that emits zsh, bash and fish scripts, completion of subcommands and flags from the same definition used for parsing, dynamic completion of output names from the running daemon or the config file, and the speed, silence and installation requirements for the zsh function.

## Requirements

### Requirement: Completion script generation
The `woluma` binary SHALL provide a `completions <shell>` subcommand that writes a completion script for the given shell to stdout and exits with status 0. Supported shells SHALL include `zsh`, `bash` and `fish`. The command SHALL NOT require a config file or a running daemon.

#### Scenario: Generate zsh completions
- **WHEN** the user runs `woluma completions zsh`
- **THEN** a zsh completion script defining completion for the `woluma` command is printed to stdout and the exit status is 0

#### Scenario: Works without config or daemon
- **WHEN** no config file exists and no daemon is running and the user runs `woluma completions zsh`
- **THEN** the script is still printed and the exit status is 0

#### Scenario: Unsupported shell
- **WHEN** the user runs `woluma completions powershell-legacy` (an unsupported value)
- **THEN** the command exits non-zero with a usage error listing the supported shells

### Requirement: Completions follow the CLI definition
Subcommand and flag completion SHALL be derived from the same command definition used to parse arguments, so every subcommand and flag accepted by the parser is offered and no non-existent ones are offered.

#### Scenario: Subcommands offered
- **WHEN** the user types `woluma <TAB>` in zsh with completions installed
- **THEN** the candidates include `daemon`, `toggle`, `pause`, `resume`, `status`, `watch`, `check` and `completions`

#### Scenario: Flags offered
- **WHEN** the user types `woluma watch --<TAB>`
- **THEN** the candidates include `--raw` and `--json`

### Requirement: Output name completion
Arguments that accept an OUTPUT target SHALL complete to the currently known output names plus the literal `*`. Known output names SHALL be taken from the running daemon when its control socket is reachable, and otherwise from the output tables of the config file woluma would load.

#### Scenario: Names from the running daemon
- **WHEN** the daemon is running and manages `eDP-1` and `DP-1` and the user types `woluma toggle <TAB>`
- **THEN** the candidates are `eDP-1`, `DP-1` and `*`

#### Scenario: Names from config when daemon is not running
- **WHEN** no daemon is running and the config defines `[outputs.eDP-1]` and `[outputs.DP-1]` and the user types `woluma pause <TAB>`
- **THEN** the candidates are `eDP-1`, `DP-1` and `*`

#### Scenario: Nothing available
- **WHEN** no daemon is running and no config file can be read and the user types `woluma resume <TAB>`
- **THEN** the only candidate is `*`, and no error text is printed to the terminal

### Requirement: Completion is fast and silent
Dynamic completion SHALL bound any daemon query by a short timeout (at most 200 ms) and SHALL never write diagnostics to the user's terminal; failures SHALL degrade to fewer candidates.

#### Scenario: Unresponsive daemon socket
- **WHEN** the control socket exists but does not answer and the user types `woluma toggle <TAB>`
- **THEN** candidates from the config file (and `*`) are shown within roughly the timeout, with no error output

### Requirement: Free-form values are not completed
Duration arguments and other free-form values SHALL NOT offer candidates (no file-name fallback).

#### Scenario: Duration argument
- **WHEN** the user types `woluma pause eDP-1 <TAB>`
- **THEN** no candidates are offered (in particular, no file names)

### Requirement: Installable zsh function
The zsh script SHALL be valid as a `_woluma` file in a directory on `$fpath` (e.g. `share/zsh/site-functions/_woluma`), so that `compinit` registers it without any extra user configuration.

#### Scenario: Installed into site-functions
- **WHEN** the output of `woluma completions zsh` is saved as `_woluma` in a directory on `$fpath` and a new zsh runs `compinit`
- **THEN** `woluma <TAB>` offers woluma's subcommands
