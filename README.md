# Linkrd

Linkrd synchronizes text clipboards over USB ACM and runs in the remote computer's
desktop user session. It is compatible with the existing Linkr text synchronization
protocol.

Only UTF-8 plain text is synchronized, including empty text. Support for images,
files, and chunked transfers is not yet implemented.
Clipboard change monitoring, periodic refresh, deduplication, loop prevention,
write acknowledgments, heartbeats, and USB reconnection are retained.
The program communicates with Linkr only over a serial port; Linkr handles the
browser's WebSocket connection.

## Installation and usage

Enable USB ACM on Linkr and connect it to the remote computer over USB. Run the
following command in the current desktop user session:

```sh
./linkrd install --autostart
```

On Windows, use `linkrd.exe install --autostart`. Installation starts the program
immediately in the background and enables automatic startup at the next login.
No desktop shortcut is created. The default installation path is
`~/.local/bin/linkrd` on Linux and
`%LOCALAPPDATA%\Programs\linkrd\linkrd.exe` on Windows. The installer configures
PATH for the current user; follow its output and open a new terminal.

```sh
linkrd status
linkrd pid
linkrd stop
linkrd start
linkrd autostart status
linkrd autostart disable
linkrd uninstall
```

Use `install --no-start` to install without starting the program. Run `linkrd stop`
before uninstalling. Uninstallation preserves logs.
Configuration, logs, and process locks all use the `linkrd` name.

By default, the serial port is selected using USB VID `1D6B` and PID `0104`.
You can also specify a device and save its startup arguments:

```sh
linkrd --list-ports
./linkrd install --autostart -- --port /dev/ttyACM0 --log-level debug
```

On Windows, a serial port argument might be `--port COM3`. On Linux, install and
run the program as the current desktop user, without sudo. If the serial port
belongs to `dialout` or `uucp` and your user lacks read/write access, ask an
administrator to add your user to the corresponding group. Log out of the desktop
and log back in before starting the program. By default, logs are stored under
`linkrd/logs` in the current user's local application data directory.

## Building and validation

Use cross to build for Windows x64, Linux x64, and Linux ARM64. Linux release
binaries are statically linked with musl:

```sh
cross build --locked --release --target x86_64-unknown-linux-musl
cross build --locked --release --target aarch64-unknown-linux-musl
cross build --locked --release --target x86_64-pc-windows-gnu
cross test --locked --target x86_64-unknown-linux-musl
cargo fmt --check
```

Build artifacts are located at `target/<target>/release/linkrd`, or `linkrd.exe`
on Windows. The generated `src/protocol/linkrd.protocol.v1.rs` is compiled directly;
building does not require protoc or code generation tools.

The Protobuf package name is `linkrd.protocol.v1`.
Linkrd does not store `.proto` files or build-time generation scripts. Protocol
definitions are used only in a temporary directory to generate the Rust code.

Text synchronization uses Protobuf, `ACMP` framing, and CRC32C, and is compatible
with Linkr. Removed media field numbers remain reserved, and the handshake
advertises only text read/write capabilities.
Unsupported messages are not treated as successful clipboard writes.
