# dtch

`dtch` is a minimal terminal session runner inspired by dtach. It starts a 
command inside a pseudo-terminal and keeps that command running after the 
current terminal disconnects. A later `dtch attach` reconnects to the same 
named session.

Sessions are exposed as Unix sockets at `/tmp/dtch_<name>.sock`. Attachments
forward terminal input, output, and resize events. Press `Ctrl-E` to detach
without stopping the command. On attach, `dtch` prints its version and the
socket modification time.

`dtch` is intended for users who want persistent terminal sessions without 
replacing their normal terminal workflow.


# AI disclaimer

AI/LLM has been heavily used in developing this utility.


## Usage

Start a session:

```sh
dtch new work /bin/zsh
```

Press `Ctrl-E` to detach from a session without terminating the running command.

Attach to an existing session:

```sh
dtch attach work
```

Create the session during attach if it does not exist:

```sh
dtch attach -c work /bin/zsh
```

List active sessions:

```sh
dtch list
```

Use `--buffer-lines <LINES>` with `new`, or with `attach -c`, to retain 
recent terminal output for replay when later attaching. Replay is disabled 
by default because raw terminal output can disrupt full-screen programs 
such as `vim`, `less`, or terminal multiplexers.


## OS

This is Unix-only.


## Install

Build release version:

```sh
cargo build --release
```


Install to ~/.cargo/bin/dtch

```sh
cargo install --path .
```


## Why

If all you need is "keep this shell running so I can reconnect later", `dtch` provides 
that functionality without the complexity of a full terminal multiplexer.

`dtch` is a minimal terminal session persistence tool inspired by `dtach`. Unlike terminal 
multiplexers such as `tmux` or `screen`, it does not provide windows, panes, layouts, 
session management, status bars, or other terminal UI features.

It simply runs a command inside a pseudo-terminal and keeps it running after your 
SSH connection or terminal disconnects. Later, you can reattach to the same session 
and continue interacting with the program as if you had never left.


## License

MIT

