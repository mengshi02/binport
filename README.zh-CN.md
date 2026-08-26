# binport

[English](README.md) | [简体中文](README.zh-CN.md)

**工具箱只构建一次，在任意 SSH 主机上直接运行，远端无需安装。**

![binport 终端演示](docs/demo.svg)

```console
$ binport build .
$ binport prod rg "authentication timeout" /var/log
```

`binport` 将常用命令行工具构建成可复现、可移植的工具箱，根据远程
Linux 主机的架构选择正确的二进制文件，并通过原生 Rust SSH 连接只传输
当前需要的工具。

远程主机不需要安装 `binport`、`rg`、守护进程、容器运行时或包管理器，
也不需要 root 权限。

## 安装

在 Linux 或 macOS 上下载安装最新版本：

```sh
curl -fsSL https://raw.githubusercontent.com/mengshi02/binport/main/install.sh | sh
```

安装脚本会自动识别 amd64/arm64，并使用 `SHA256SUMS` 校验下载文件，
整个过程不会调用 sudo。也可以指定版本和安装目录：

```sh
BINPORT_INSTALL_DIR="$HOME/bin" \
BINPORT_VERSION="v0.1.4" \
sh install.sh
```

Windows amd64 用户可以在 PowerShell 中运行：

```powershell
irm https://raw.githubusercontent.com/mengshi02/binport/main/install.ps1 | iex
```

默认安装到 `%LOCALAPPDATA%\binport\bin`。如果该目录尚未加入用户 PATH，
安装脚本会给出提示。

从源码安装：

```sh
cargo install --git https://github.com/mengshi02/binport --tag v0.1.4 --locked
```

## 一次密码，长期免密

如果服务器目前使用密码登录，让 binport 原生生成并安装一把独立的
Ed25519 Key：

```console
$ binport auth setup server-a
SSH password:
Passwordless authentication is ready for server-a

$ binport server-a rg --version
```

密码只用于本次连接，不会持久化。binport 专用私钥保存在当前平台的用户
配置目录中，与现有 SSH Key 隔离，并强制设置严格权限。重复 setup 不会
重复追加远端公钥。

```sh
binport auth status server-a
binport auth remove server-a
```

`remove` 会先精确删除远端对应的公钥行，再删除本地密钥。目前 auth 管理
只支持直连主机；普通 Key/密码认证仍然可以通过一跳 ProxyJump 使用。

## 五分钟上手

创建 `Binfile`：

```dockerfile
TARGET linux/amd64
TARGET linux/arm64

TOOL rg@15.2.0
TOOL fd@10.4.2
TOOL jq@1.8.2
```

解析并构建工具箱：

```console
$ binport resolve .
Resolved 6 artifacts into Binport.lock

$ binport build .
Toolbox built: 6 artifacts
Manifest: .binport/toolbox.json
```

将 `Binfile` 和 `Binport.lock` 提交到 Git。Lock 文件记录每个工具的准确
版本、平台、下载地址、归档格式和 SHA-256；当 Binfile 或本地 `COPY`
输入发生变化时，构建会要求重新 resolve。

配置一个普通 SSH 别名：

```sshconfig
Host server-a
    HostName 192.0.2.15
    User deploy
    IdentityFile ~/.ssh/id_ed25519
```

直接运行远程主机上原本不存在的工具：

```console
$ binport server-a rg "authentication timeout" /var/log
$ binport server-a btm               # 自动分配 PTY
$ binport --tty server-a TOOL ...    # 为其他交互工具强制分配 PTY
$ binport jump-a,server-a btm        # 临时经过 jump-a，在 server-a 上执行
/var/log/auth.log:42:authentication timeout upstream=identity
```

第一次运行会上传对应架构的工具；后续运行命中内容寻址缓存，不会重复传输。

## 核心命令

```text
binport resolve [PATH]             生成或更新 Binport.lock
binport auth setup HOST            安装 binport 专用免密 SSH Key
binport auth status HOST           验证专用 Key
binport auth remove HOST           删除本地及远端专用 Key
binport build [PATH]               构建 Binfile 中声明的工具箱
binport ls [PATH]                  列出工具
binport fetch TOOL...              预下载指定工具
binport fetch --all                预下载精选目录中的全部工具
binport status [PATH]              查看工具箱与缓存状态
binport clean                      清理本地下载缓存
binport export ops.toolbox         导出单文件离线工具箱
binport load ops.toolbox           导入离线工具箱
binport pack ops.oci               打包为 OCI image layout
binport unpack ops.oci             导入 OCI image layout
binport pull oci://HOST/REPO:TAG   从 OCI Registry 拉取工具箱
binport push oci://HOST/REPO:TAG   将工具箱推送到 OCI Registry
binport doctor HOST|@GROUP         检查连接、平台、延迟与缓存
binport warm HOST|@GROUP           提前将缺失工具传输到远端
binport plan HOST|@GROUP TOOL      离线预览主机、路由和工具选择
binport watch HOST TOOL            持续观察命令结果变化
binport cp 源 目标                  通过内置 SSH 复制文件（远端写作 HOST:PATH）
binport HOST TOOL [ARGS]...        在单台远程主机执行工具
binport @GROUP TOOL [ARGS]...      在一组主机上并发执行工具
```

工具列表会展示现代命令与传统命令的对应关系及用途。远程执行 `eza` 时默认
启用长格式和 ANSI 色彩；用户显式传入的布局、色彩参数优先：

```sh
binport ls
binport server-a eza /export
```

可以用内置的 `micro` 编辑远程文件（自动分配 PTY），也可以直接通过 Rust
SSH 通道复制普通文件，全程不启动外部 `ssh`/`scp` 进程：

```sh
binport server-a edit /etc/myapp/config.toml
binport cp ./config.toml server-a:/tmp/config.toml
binport cp server-a:/var/log/app.log ./app.log
binport cp server-a:/tmp/a.txt server-b:/tmp/a.txt
```

## Fleet 并发执行

binport 使用 SSH config 中的具体 Host 别名组成 Fleet。`@prod` 会选择
`prod-*` 主机：

```sshconfig
Host bastion
    HostName 203.0.113.10
    User ops

Host prod-api-01
    HostName 192.0.2.15
    User deploy
    ProxyJump bastion

Host prod-api-02
    HostName 192.0.2.16
    User deploy
    ProxyJump bastion
```

```console
$ binport @prod rg 'panic|fatal' /var/log
[prod-api-01] /var/log/app.log:88:fatal: database timeout
[prod-api-02] /var/log/app.log:51:panic: worker exited
```

并发数量由 `--concurrency` 控制。使用相同 ProxyJump 的 Fleet 会共享跳板机
连接；单台主机失败不会中断其他主机。`--json` 可供 CI 和智能体消费。

执行前可以完全离线地检查计划：

```console
$ binport plan @prod rg
HOST            DESTINATION              ROUTE
prod-api-01     deploy@192.0.2.15:22     bastion
prod-api-02     deploy@192.0.2.16:22     bastion

ARTIFACT        SIZE      REMOTE CACHE PATH
linux/amd64     5.2MiB    $HOME/.cache/binport/<sha256>/rg
linux/arm64     4.3MiB    $HOME/.cache/binport/<sha256>/rg
```

## Watch

Watch 会复用 SSH 和 ProxyJump 连接，只有结果发生变化时才输出事件：

```sh
binport watch --interval 5 @prod rg 'panic|fatal' /var/log/app.log
binport watch --until-success @prod rg 'deployment complete' /var/log/deploy.log
binport watch --jsonl @prod rg panic /var/log/app.log
```

事件包括 `INITIAL`、`CHANGED`、`CLEARED`、`OFFLINE` 和 `RECOVERED`。
`--jsonl` 每行输出一个独立事件，适合智能体和流式处理系统。

## 离线环境与 OCI Registry

工具箱可以导出为一个自包含文件，在隔离网络中导入：

```sh
binport export ops.toolbox
binport load ops.toolbox
```

也可以使用标准 OCI image layout，或推送到 GHCR、Harbor 等 Registry：

```sh
binport pack ops.oci
binport unpack ops.oci

binport push oci://harbor.example.com/platform/ops:v1 \
  --username 'robot$binport' --registry-password
binport pull oci://harbor.example.com/platform/ops:v1 \
  --username 'robot$binport' --registry-password
```

密码通过隐藏式交互提示读取，只保存在当前进程内存中。Registry Blob 使用
内容寻址缓存；重复 push 只上传缺失层，重复 pull 不会再次下载已有层。

## 远程执行原理

每次执行时，binport 会：

1. 从 SSH config、Agent 或 IdentityFile 解析连接信息；
2. 使用原生 Rust SSH 建立连接，不启动外部 `ssh` 或 `scp` 进程；
3. 在一次远程请求中识别 OS/CPU、选择工具并检查内容寻址缓存；
4. 缓存未命中时流式上传工具，通过临时文件和原子 rename 安装；
5. 执行工具，并原样返回 stdout、stderr 和退出状态。

远程缓存位于 `~/.cache/binport/<sha256>/`。远端只需要 POSIX shell，
不需要 binport Agent 或其他运行时。

## 当前范围

- 精选工具：`rg`、`fd`、`jq`、`eza`、`bat`、`dust`、`btm`（bottom）、
  `sd`、`delta`、`micro`。Linux amd64 产物均为静态版本；`eza` 和 `delta`
  的上游 arm64 产物依赖 glibc。
- 远程目标：Linux amd64、Linux arm64
- 本地客户端：Linux、macOS 的 amd64/arm64，以及 Windows amd64
- SSH 认证：Agent、未加密私钥、交互式密码、binport 管理的独立 Key
- 支持一跳 ProxyJump；暂不支持多级跳板链和交互式 PTY
- OCI 支持匿名和密码认证的 pull/push；暂不支持持久登录和自定义 CA

这是早期版本。如果你的场景需要新的工具、平台或 SSH 行为，欢迎提交 Issue。

## 安全

- 官方下载按版本和 SHA-256 固定
- SSH Server Key 使用 `known_hosts` 校验
- 远程上传使用严格 umask、临时文件和原子 rename
- 工具参数作为位置参数传递，不拼接为可插值的 Shell 片段
- 密码不会写入配置文件或磁盘

请按照 [SECURITY.md](SECURITY.md) 私密报告安全问题，不要公开提交漏洞细节。

## 参与贡献

贡献指南见 [CONTRIBUTING.md](CONTRIBUTING.md)，版本变化见
[CHANGELOG.md](CHANGELOG.md)，社区行为遵循
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)。

## License

MIT
