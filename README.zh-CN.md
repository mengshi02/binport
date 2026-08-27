# binport

[English](README.md) | [简体中文](README.zh-CN.md)

[![CI](https://github.com/mengshi02/binport/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/mengshi02/binport/actions/workflows/ci.yml)
[![Release](https://github.com/mengshi02/binport/actions/workflows/release.yml/badge.svg)](https://github.com/mengshi02/binport/actions/workflows/release.yml)
[![GitHub Release](https://img.shields.io/github/v/release/mengshi02/binport?display_name=tag&sort=semver)](https://github.com/mengshi02/binport/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/mengshi02/binport/total)](https://github.com/mengshi02/binport/releases)
[![License](https://img.shields.io/github/license/mengshi02/binport)](LICENSE)

[![Stars](https://img.shields.io/github/stars/mengshi02/binport?style=flat)](https://github.com/mengshi02/binport/stargazers)
[![Forks](https://img.shields.io/github/forks/mengshi02/binport?style=flat)](https://github.com/mengshi02/binport/forks)
[![Issues](https://img.shields.io/github/issues/mengshi02/binport)](https://github.com/mengshi02/binport/issues)
[![Pull Requests](https://img.shields.io/github/issues-pr/mengshi02/binport)](https://github.com/mengshi02/binport/pulls)
[![Contributors](https://img.shields.io/github/contributors/mengshi02/binport)](https://github.com/mengshi02/binport/graphs/contributors)
[![Last Commit](https://img.shields.io/github/last-commit/mengshi02/binport/main)](https://github.com/mengshi02/binport/commits/main)

![Rust](https://img.shields.io/badge/Rust-2024_Edition-000000?logo=rust)
![Linux](https://img.shields.io/badge/Linux-amd64%20%7C%20arm64-FCC624?logo=linux&logoColor=black)
![macOS](https://img.shields.io/badge/macOS-amd64%20%7C%20arm64-000000?logo=apple)
![Windows](https://img.shields.io/badge/Windows-amd64-0078D4?logo=windows)
![Native SSH](https://img.shields.io/badge/SSH-native_Rust-4D4D4D?logo=gnubash&logoColor=white)
![No Agent](https://img.shields.io/badge/remote_agent-not_required-2ea44f)
![Repository Size](https://img.shields.io/github/repo-size/mengshi02/binport)
![Code Size](https://img.shields.io/github/languages/code-size/mengshi02/binport)

**工具箱只构建一次，在任意 SSH 主机上直接运行，远端无需安装。**

> **带着自己的工具，穿过堡垒机，直接工作。** binport 使用原生 Rust SSH
> 穿过直连 SSH、ProxyJump 和应用层企业堡垒机，远端无需安装 Agent。

```console
$ binport bastion probe worker-a
Connection:     supported
Exec:           supported
$ binport worker-a rg "authentication timeout" /var/log
/var/log/auth.log: authentication timeout upstream=identity
```

![binport 终端演示](docs/demo.svg)

```console
$ binport build .
$ binport prod rg "authentication timeout" /var/log
```

`binport` 是面向 SSH 主机、跳板机和企业堡垒机的无代理远程工具箱。它将
常用命令行工具构建成可复现、可移植的工具箱，根据远程 Linux 主机的架构
选择正确的二进制文件，并通过原生 Rust SSH 连接只传输当前需要的工具。

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
BINPORT_VERSION="v0.1.5" \
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
cargo install --git https://github.com/mengshi02/binport --tag v0.1.5 --locked
```

## 不用手写 SSH Config

### 堡垒机兼容模板

查看 binport 内置的堡垒机登录格式及其证据状态：

```console
$ binport bastion presets
PRESET              FORMAT                    PRODUCT      STATUS
h3c-iware-slash     {user}/{host}/{account}   H3C i-Ware   deployment-verified
huawei-cbh-at       {user}@{account}@{host}   Huawei CBH   vendor-documented
jumpserver-koko-at  {user}@{account}@{host}   JumpServer   community-reported
oneidentity-sps-inband {account}@{host}         One Identity vendor-documented
wallix-bastion-shell   {account}@{host}:SSH:{user} WALLIX   vendor-documented
cyberark-psmp-at       {user}@{account}@{host} CyberArk     community-reported
```

配置时使用模板名称，不再需要记忆厂商的组合用户名格式：

```console
$ binport host add worker-a root@192.0.2.52 \
    --bastion bastion.example.com \
    --bastion-user operator \
    --bastion-account root \
    --bastion-preset h3c-iware-slash
$ binport worker-a rg --version
```

`h3c-iware-slash` 已在一个 H3C i-Ware 部署中验证，但不代表所有 H3C
版本都采用相同格式。尚未内置的产品仍可通过 `--bastion-format` 自定义
`{user}`、`{host}` 和 `{account}` 的排列方式。

安全检测一条已配置路由的实际能力，不遍历用户名格式，也不猜测凭据：

```console
$ binport bastion probe worker-a
Bastion capability report
  Host:           worker-a
  Preset:         h3c-iware-slash
  Connection:     supported (93 ms)
  Exec:           supported (18 ms)
  direct-tcpip:   not-checked
```

增加 `--check-forwarding` 后才会主动发送一次 `direct-tcpip` 能力请求。该检测
默认关闭，因为企业堡垒机可能记录或拒绝端口转发请求。

通过已配置的直连、ProxyJump 或堡垒机路由转发本地 TCP 端口，全程不启动外部
`ssh` 进程：

```console
$ binport tunnel 8080:127.0.0.1:3000 worker-a
Tunneling 127.0.0.1:8080 -> 127.0.0.1:3000
```

该功能要求 SSH 服务或堡垒机策略允许 `direct-tcpip`。

通过简洁命令添加直连主机和一跳路由，不必手写 `ProxyJump`：

```sh
binport host add jumpserver-51 root@203.0.113.10
binport host add app-01 root@10.0.0.52 --jump jumpserver-51

binport host ls
binport host show app-01
binport host test app-01
```

binport 使用标准 SSH 语法写入 `~/.ssh/binport_config`，并仅在
`~/.ssh/config` 中加入一行 `Include ~/.ssh/binport_config`。因此同一别名也
可以用于 `ssh`、`scp`、rsync 和编辑器的 SSH 插件。已有的手写 Host 不会被
覆盖；只有更新 binport 已管理的别名时才使用 `--force`。

```sh
binport host remove app-01
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

`remove` 会先精确删除远端对应的公钥行，再删除本地密钥。认证安装、状态检查
和删除均支持一跳 ProxyJump；跳板机本身需要已经可以通过 Key 或 Agent 登录。

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

精选目录的数据维护在 [`catalog.yaml`](catalog.yaml)，包括命令对应关系、
版本、平台、下载地址、归档格式和 SHA-256。加载时会严格校验，并在编译时嵌入
binport，所以安装结果仍是单一二进制，无网络、无外置 catalog 文件也能工作；
每个项目实际使用的解析结果仍由 `Binport.lock` 冻结。

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
binport host add|ls|show|test|remove 管理 SSH 主机和跳板路由
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
binport rm HOST:PATH               删除远程文件（目录需要 `-r`）
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
binport rm server-a:/tmp/a.txt
binport rm --recursive server-a:/tmp/old-output
```

复制采用固定大小的分块流式传输；交互式终端会显示字节数、速度和 ETA。目录下载
和首次上传远程工具复用同一套进度组件。输出被重定向或使用 `--json` 时会自动
关闭动画，避免污染脚本输出。

`rm` 只接受 `HOST:PATH`，会拒绝 `/`、`~`、`..` 等根目录、主目录或路径穿越
形式；删除目录必须显式传入 `--recursive`，`--force` 可忽略目标不存在。

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

## 故障排查

先执行能够识别路由的连接检查，再打开详细输出：

```sh
binport host show server-a
binport host test server-a
binport --verbose server-a rg --version
```

### Key authentication failed

安装或检查 binport 专用 Key，也可以仅对目标机临时输入一次密码：

```sh
binport auth setup server-a
binport auth status server-a
binport --password server-a rg --version
```

ProxyJump 场景下，跳板机必须已经能够使用自己的 Key 或 SSH Agent 登录；
`--password` 只属于目标机，不会用于跳板机。先运行
`binport host test jump` 单独检查跳板机。

### 未知主机密钥或主机密钥校验失败

binport 使用 OpenSSH 默认的 `known_hosts`，不会静默关闭校验。通过可信渠道
核对指纹，先用 OpenSSH 连接一次写入记录，然后重试：

```sh
ssh server-a
binport host test server-a
```

### ProxyJump 失败或超时

分别检查每一段连接，再确认最终解析出的路由：

```sh
binport host test jump
binport host show server-a
binport host test server-a
```

当前只支持一层跳板；逗号分隔的多级链或嵌套 ProxyJump 会明确报错，不会只执行
一部分路由。

### 复制或工具上传中断

上传使用权限受限的临时文件和原子 rename。中断不会用残缺内容覆盖目标文件，
直接重新执行原命令即可；`binport cp` 失败后也会删除本地中转文件。再次执行
工具时，要么命中经过校验的内容寻址缓存，要么重新上传完整二进制。

### SHA-256 不匹配或 Lockfile 失效

不要手工修复 `Binport.lock`。恢复声明的源，重新解析并构建：

```sh
binport resolve .
binport clean
binport fetch --all
binport build .
```

清理后重新下载仍然校验失败，说明上游产物可能发生了意外变化；不要绕过校验，
应停止使用并提交 Issue。

### Alpine 或其他 musl 主机执行失败

大部分精选 Linux 产物是静态二进制，但当前 `eza`、`delta` 的 arm64 构建依赖
glibc。可以通过 `COPY` 提供 musl 二进制；`binport doctor server-a` 会报告
选择的平台和缓存状态。

提交问题时，请附上 `binport --version`、去除敏感信息后的失败命令，以及
`--verbose` 或 `--json` 输出。不要提交密码、私钥、Registry Token 或私网地址。

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
- 支持一跳 ProxyJump、应用层堡垒机模板、原生本地 TCP Tunnel 和交互式
  PTY；暂不支持多级跳板链
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
