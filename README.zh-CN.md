# Linkrd

Linkrd 是通过 USB ACM 同步文本剪贴板的程序，运行在被控电脑的桌面用户会话中。
与现有 Linkr 文本同步协议兼容。

仅同步 UTF-8 纯文本，包含空文本；待实现供图片、文件或分块传输功能。
保留剪贴板变化监听、周期刷新、去重与防回环、写入确认、心跳和 USB 断线重连。
程序只通过串口与 Linkr 通信，浏览器的 WebSocket 连接由 Linkr 处理。

## 安装与使用

在 Linkr 中开启 USB ACM，将 USB 连接到被控电脑。在当前桌面用户会话中执行：

```sh
./linkrd install --autostart
```

Windows 使用 `linkrd.exe install --autostart`。安装后立即在后台启动，并注册下次登录时自动启动。
不创建桌面快捷方式。Linux 默认安装到 `~/.local/bin/linkrd`，Windows 默认安装到
`%LOCALAPPDATA%\Programs\linkrd\linkrd.exe`。安装器会为当前用户配置 PATH；按输出提示打开新终端。

```sh
linkrd status
linkrd pid
linkrd stop
linkrd start
linkrd autostart status
linkrd autostart disable
linkrd uninstall
```

`install --no-start` 仅安装。卸载前先执行 `linkrd stop`，卸载会保留日志。
配置、日志和进程锁均使用 `linkrd` 名称。

默认按 USB VID `1D6B`、PID `0104` 选择串口，也可以指定设备并保存启动参数：

```sh
linkrd --list-ports
./linkrd install --autostart -- --port /dev/ttyACM0 --log-level debug
```

Windows 串口参数示例为 `--port COM3`。Linux 使用当前桌面用户安装和运行，避免使用 sudo。
如果串口属于 `dialout` 或 `uucp` 且当前用户没有读写权限，请联系管理员加入对应组，
注销桌面并重新登录后再启动。日志默认位于当前用户本地应用数据目录下的 `linkrd/logs`。

## 构建与验证

使用 cross 构建 Windows x64、Linux x64 和 Linux ARM64。Linux 发行文件采用 musl 静态链接：

```sh
cross build --locked --release --target x86_64-unknown-linux-musl
cross build --locked --release --target aarch64-unknown-linux-musl
cross build --locked --release --target x86_64-pc-windows-gnu
cross test --locked --target x86_64-unknown-linux-musl
cargo fmt --check
```

产物位于 `target/<target>/release/linkrd`，Windows 为 `linkrd.exe`。
直接编译已生成的 `src/protocol/linkrd.protocol.v1.rs`，构建不依赖 protoc 或代码生成工具。

生成文件对应的 Protobuf 包名为 `linkrd.protocol.v1`。
linkrd 不保存 `.proto` 或构建时生成脚本；协议定义仅在临时目录中用于生成 Rust 代码。

文本同步使用 Protobuf、`ACMP` 帧格式及 CRC32C，与 Linkr 兼容。
删去的媒体字段编号保持保留状态，握手仅声明文本读写能力。
未支持的消息不会被当作成功的剪贴板写入。
