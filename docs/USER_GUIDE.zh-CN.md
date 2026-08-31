# Binport v0.2 用户手册

本文面向第一次使用 Binport 的开发者、运维人员和智能体开发者，覆盖从安装、
配置连接、构建工具箱，到远程执行、文件传输、端口转发、Fleet、离线分发和
故障排查的完整流程。

> Binport 是本地客户端。远端不需要安装 Binport、Agent、容器运行时、包管理器，
> 也不需要 root 权限。远端只需要 Linux、SSH 和 POSIX shell。

## 1. 核心概念

Binport 把三个原本分散的问题统一到一个命令行中：

1. **Host route**：如何到达目标主机——直连、ProxyJump、企业堡垒机或 exec-hop。
2. **Toolbox**：允许带到远端执行的便携工具及其版本、平台和 SHA-256。
3. **Remote cache**：远端按内容哈希缓存工具，首次上传，后续直接复用。

配置完成后，用户只需要记住主机别名：

```sh
binport prod-db rg 'timeout' /var/log
binport cp ./config.toml prod-db:/tmp/config.toml
binport tunnel 8080:127.0.0.1:3000 prod-db
```

## 2. 支持范围

| 项目 | v0.2 支持范围 |
|---|---|
| 本地客户端 | Linux amd64/arm64、macOS amd64/arm64、Windows amd64 |
| 远程目标 | Linux amd64、Linux arm64 |
| SSH 路径 | 直连、一跳 ProxyJump、应用层堡垒机、原生 Rust exec-hop |
| 认证 | SSH Agent（Unix）、未加密私钥、交互密码、Binport 专用 Key |
| 文件能力 | 上传、下载、远端到远端复制、删除文件和目录 |
| 网络能力 | 原生 `direct-tcpip`，或 exec-hop TCP relay 回退 |
| Fleet | SSH Host 前缀分组、并发执行、Doctor、Warm、Watch |
| Toolbox 分发 | 单文件、OCI image layout、OCI Registry/Harbor |

当前不支持任意层数的 ProxyJump、exec-hop 交互式 TTY、exec-hop Fleet、企业
堡垒机菜单自动录制回放、Registry 持久登录和自定义 CA。遇到不支持的路径时
Binport 会明确报错，不会静默调用外部 `ssh`。

## 3. 安装

### Linux 和 macOS

```sh
curl -fsSL https://raw.githubusercontent.com/mengshi02/binport/main/install.sh | sh
binport --version
```

安装脚本自动识别 amd64/arm64，验证 `SHA256SUMS`，且不使用 sudo。指定版本或
目录：

```sh
BINPORT_VERSION=v0.2.2 \
BINPORT_INSTALL_DIR="$HOME/bin" \
sh install.sh
```

### Windows PowerShell

```powershell
irm https://raw.githubusercontent.com/mengshi02/binport/main/install.ps1 | iex
binport --version
```

默认安装到 `%LOCALAPPDATA%\binport\bin`。Windows 客户端使用密码或
`IdentityFile` 私钥认证；SSH Agent 认证当前仅在 Unix 客户端提供。

### 从源码安装

```sh
cargo install --git https://github.com/mengshi02/binport --tag v0.2.2 --locked
```

## 4. 新机器首次配置

### 4.1 最简单的方式：自动向导

```sh
binport host add prod-db
```

向导首先询问最终目标的 `USER@HOST`，然后提供：

1. Direct SSH
2. SSH jump host
3. Enterprise bastion
4. Auto detect

如果不确定环境类型，选择 **Auto detect**。Binport 会先检查直连；直连不可用时，
再询问平时首先登录的入口机器，并分别检查：

- 入口主机是否可达、能否认证；
- 入口是否允许 `direct-tcpip`；
- 本地凭证能否通过入口认证目标；
- 目标凭证是否只存在于入口主机；
- 命令、文件流和 TCP relay 能力。

向导在写配置前展示探测结果和将要采用的策略。保存后检查：

```sh
binport host show prod-db
binport host test prod-db
```

### 4.2 直连 SSH

交互配置：

```sh
binport host add server-a
```

脚本化配置：

```sh
binport host add server-a deploy@192.0.2.15
binport host add server-a deploy@example.com --port 2222 --force
```

### 4.3 SSH Jump Host / ProxyJump

先配置入口，再配置目标：

```sh
binport host add jump-a ops@203.0.113.10
binport host test jump-a
binport host add prod-db root@10.0.0.52 --jump jump-a
binport host test prod-db
```

正常 ProxyJump 使用本地凭证分别认证入口和目标，并通过入口的原生
`direct-tcpip` 通道访问目标。

### 4.4 Exec-hop：目标凭证只在跳板机上

以下场景应使用 exec-hop：

- 本地能登录入口，但没有目标私钥；
- 目标私钥、SSH config 或认证环境只存在于入口主机；
- 入口禁止 `direct-tcpip`，但允许执行命令；
- 需要在该限制下执行非交互工具、复制文件或代理 TCP 服务。

```sh
binport host add prod-db root@10.0.0.52 --jump jump-a --exec-hop
binport host show prod-db
binport prod-db rg --version
```

Binport 会把同版本的 `binport-hop` 临时部署到入口主机。Helper 使用入口已有的
SSH 配置和凭证连接目标，全程不调用外部 `ssh`。Helper 和工具都按 SHA-256
进入内容寻址缓存，不是需要安装和维护的常驻 Agent。

默认从同版本 GitHub Release 下载并校验 helper。离线或私有环境可以提供已验证
的二进制：

```sh
export BINPORT_HOP_BINARY=/secure/mirror/binport-hop-linux-amd64
binport prod-db rg --version
```

### 4.5 企业堡垒机

查看内置登录格式及证据状态：

```sh
binport bastion presets
```

使用模板配置：

```sh
binport host add worker-a root@192.0.2.52 \
  --bastion bastion.example.com \
  --bastion-user operator \
  --bastion-account root \
  --bastion-preset h3c-iware-slash
```

未内置的产品可以指定组合用户名模板：

```sh
binport host add worker-a root@192.0.2.52 \
  --bastion bastion.example.com \
  --bastion-user operator \
  --bastion-account root \
  --bastion-format '{user}/{host}/{account}'
```

安全探测已配置路由：

```sh
binport bastion probe worker-a
binport bastion probe worker-a --check-forwarding
```

`--check-forwarding` 会实际请求一次 `direct-tcpip`，可能被企业审计，因此默认
不执行。Preset 的状态表示证据强度，不保证某品牌所有版本都采用相同格式。

### 4.6 配置文件位置

Binport 管理 `~/.ssh/binport_config`，并在 `~/.ssh/config` 中加入：

```sshconfig
Include ~/.ssh/binport_config
```

因此 Binport 创建的普通 SSH/ProxyJump 别名也可被 `ssh`、`scp`、rsync 和
编辑器 SSH 插件使用。Binport 不覆盖手写 Host；`--force` 只能更新 Binport
自己管理的别名。

```sh
binport host ls
binport host show prod-db
binport host remove prod-db
```

## 5. 密码与免密认证

临时使用目标密码：

```sh
binport --password server-a rg --version
```

密码只保存在当前进程内存中。希望以后免输密码时，安装一把与现有 Key 隔离的
Binport 专用 Ed25519 Key：

```sh
binport auth setup server-a
binport auth status server-a
binport auth remove server-a
```

`auth setup` 会提示一次 SSH 密码、安装公钥并立即验证；重复执行不会重复追加。
`auth remove` 默认要求确认，也可以使用 `--yes`。ProxyJump 场景要求入口已经能
用 Key 或 Agent 登录；`--password` 不会自动同时应用于入口和目标。

## 6. 创建 Toolbox

### 6.1 Binfile

在项目目录创建 `Binfile`：

```dockerfile
TARGET linux/amd64
TARGET linux/arm64

TOOL rg@15.2.0
TOOL fd@10.4.2
TOOL jq@1.8.2
TOOL eza@0.23.5
TOOL bat@0.26.1
TOOL micro@2.0.14
```

`TARGET` 声明需要构建的平台，`TOOL` 从内置精选目录解析工具。查看目录、传统
命令对应关系和描述：

```sh
binport ls
binport fetch rg eza --target linux/amd64
binport fetch --all --target linux/arm64
```

### 6.2 加入自己的二进制

```dockerfile
TARGET linux/amd64
TARGET linux/arm64

COPY ./dist/mytool-amd64 mytool --target linux/amd64
COPY ./dist/mytool-arm64 mytool --target linux/arm64
```

`COPY` 输入变化后必须重新 `resolve`，从而把新的内容哈希写入 Lockfile。

### 6.3 Resolve、Build 与版本冻结

```sh
binport resolve .
binport build .
binport status .
```

建议提交 `Binfile` 和 `Binport.lock`，不要提交 `.binport/` 构建目录。Lockfile
冻结版本、平台、下载 URL、归档格式和 SHA-256；不要手工编辑它。

内置目录位于仓库根目录的 `catalog.yaml`，编译时嵌入 Binport，因此运行时
不依赖外置 catalog 文件。

## 7. 执行远程工具

通用语法：

```text
binport [全局选项] HOST TOOL [ARGS]...
```

示例：

```sh
binport server-a rg 'authentication timeout' /var/log
binport --verbose server-a rg --version
binport server-a eza /export
binport server-a edit /etc/myapp/config.toml
binport --tty server-a micro /etc/myapp/config.toml
```

第一次执行会探测远端 Linux/CPU，选择正确产物并上传；后续命中
`~/.cache/binport/<sha256>/`。退出状态、stdout 和 stderr 原样返回。

`eza` 默认启用长格式和 ANSI 色彩，用户显式参数优先。`btm`、`micro`、
`edit` 等交互工具会自动使用 PTY；其他工具可用 `--tty` 强制分配。

全局选项：

| 选项 | 作用 |
|---|---|
| `--password` | 交互读取 SSH 密码 |
| `-v, --verbose` | 显示平台、缓存和传输细节 |
| `--concurrency N` | Fleet 最大并发数，默认 10 |
| `--json` | 输出机器可读 JSON |
| `-t, --tty` | 为单主机命令分配交互终端 |

## 8. 文件复制与删除

远端路径写作 `HOST:PATH`：

```sh
# 本地 -> 远端
binport cp ./config.toml server-a:/tmp/config.toml

# 远端 -> 本地
binport cp server-a:/var/log/app.log ./app.log

# 远端 -> 远端；数据经过本地流式中转
binport cp server-a:/tmp/a.txt server-b:/tmp/a.txt

# 删除文件、目录
binport rm server-a:/tmp/a.txt
binport rm --recursive server-a:/tmp/old-output
binport rm --force server-a:/tmp/may-not-exist
```

文件使用固定 64 KiB 分块流式传输，内存不会随文件大小增长。交互终端显示
进度、速度和 ETA；重定向输出或 `--json` 时自动关闭动画。

`rm` 会拒绝 `/`、`~`、`..` 等危险目标；删除目录必须显式使用
`--recursive`。直连、ProxyJump 和 exec-hop 均可使用文件功能。

## 9. TCP Tunnel

语法：

```text
binport tunnel LOCAL_PORT:REMOTE_HOST:REMOTE_PORT [更多映射...] HOST
```

示例：

```sh
binport tunnel 8080:127.0.0.1:3000 prod-db
binport tunnel 8080:127.0.0.1:3000 15432:db.internal:5432 prod-db
```

本地访问 `http://127.0.0.1:8080`，流量经已保存的主机路由到目标网络。

- 普通 SSH/ProxyJump/堡垒机优先使用原生 `direct-tcpip`；
- exec-hop 路由使用入口 helper 的 TCP relay；
- 当前 exec-hop 每个本地连接建立一个 helper channel，并保留 TCP half-close；
- 目标服务只需对最终 SSH 节点可达，不必对本地网络开放。

## 10. Fleet、Doctor、Warm 与 Plan

Fleet 由 SSH config 中具体 Host 名的前缀组成。`@prod` 匹配 `prod-*`，`@all`
匹配全部具体主机：

```sh
binport @prod rg 'panic|fatal' /var/log
binport --concurrency 20 @prod rg --version
binport --json @prod rg panic /var/log/app.log
```

使用相同 ProxyJump 的主机会共享入口连接；单机失败不会中断其他主机。

执行前后的管理命令：

```sh
binport plan @prod rg       # 不连接，展示路由和候选产物
binport doctor @prod        # 检查连接、平台、延迟和远端缓存
binport warm @prod          # 预先上传完整工具箱
```

exec-hop 当前只支持单机非 TTY 执行，不支持 Fleet。

## 11. Watch

Watch 复用 SSH/ProxyJump 连接，默认只输出变化：

```sh
binport watch --interval 5 @prod rg 'panic|fatal' /var/log/app.log
binport watch --count 10 server-a rg timeout /var/log/app.log
binport watch --until-success @prod rg 'deployment complete' /var/log/deploy.log
binport watch --jsonl @prod rg panic /var/log/app.log
```

事件包括 `INITIAL`、`CHANGED`、`CLEARED`、`OFFLINE` 和 `RECOVERED`。
`--all` 也打印未变化快照；Watch 是事件流，因此使用 `--jsonl`，不使用全局
`--json`。

## 12. 离线环境

### 单文件导入导出

联网机器：

```sh
binport resolve .
binport build .
binport export ops.toolbox
```

把 `ops.toolbox` 带到隔离环境：

```sh
binport load ops.toolbox
binport status .
```

### OCI image layout

```sh
binport pack ops.oci
binport unpack ops.oci
```

### OCI Registry / Harbor

```sh
binport push oci://harbor.example.com/platform/ops:v1 \
  --username 'robot$binport' --registry-password

binport pull oci://harbor.example.com/platform/ops:v1 \
  --username 'robot$binport' --registry-password
```

Registry 密码由隐藏式提示读取，不写入配置。Blob 内容寻址，重复 push/pull 只
传输缺失层。`--plain-http` 只用于可信开发环境，生产环境不要关闭 TLS。

## 13. JSON 与自动化

以下管理命令支持全局 `--json`，适合 CI 和智能体解析：

```sh
binport --json host ls
binport --json host show prod-db
binport --json host test prod-db
binport --json doctor @prod
binport --json plan @prod rg
```

远程 Fleet 命令也可输出 JSON。Watch 使用 `--jsonl`，每行一个完整事件，便于
持续消费。脚本应以进程退出状态判断成功，不要只匹配人类可读文本。

## 14. 安全模型

- 发布包和精选工具都验证 SHA-256；
- SSH Server Key 使用标准 `known_hosts` 校验，不自动关闭验证；
- 密码、私钥和 Registry Token 不写入项目配置；
- 远端上传先写权限受限的临时文件，再原子 rename；
- 工具参数作为位置参数传递，不拼接为可插值 shell 源码；
- 远端缓存按内容哈希隔离；
- `rm` 对根目录、主目录和路径穿越进行防护；
- 企业堡垒机转发探测需要显式 `--check-forwarding`。

不要把真实密码、私钥、Token、生产 IP 或脱敏前的诊断日志提交到 Issue。
安全漏洞请按 [SECURITY.md](../SECURITY.md) 私密报告。

## 15. 常见故障

### `Key authentication failed`

```sh
binport auth setup server-a
binport auth status server-a
binport --password server-a rg --version
```

ProxyJump 应先检查入口：

```sh
binport host test jump-a
binport host show server-a
binport host test server-a
```

### 未知主机密钥

Binport 不会绕过 `known_hosts`。通过可信渠道核对指纹后，先用 OpenSSH 建立
一次连接，或按组织规范写入 `known_hosts`：

```sh
ssh server-a
binport host test server-a
```

### 入口禁止 `direct-tcpip`

如果入口允许执行命令，且能从入口使用 SSH 凭证访问目标，将路由配置为
exec-hop：

```sh
binport host add prod-db root@10.0.0.52 --jump jump-a --exec-hop --force
```

企业策略同时禁止端口转发和远程 exec 时，客户端工具没有通用绕过方式，需要
管理员调整策略。

### 工具上传或复制中断

直接重试。临时文件不会覆盖完整目标，内容缓存也会重新校验。需要彻底重建时：

```sh
binport clean
binport resolve .
binport build .
```

### SHA-256 或 Lockfile 错误

不要手改 Lockfile，也不要跳过校验：

```sh
binport resolve .
binport clean
binport fetch --all
binport build .
```

仍失败通常表示上游产物变化，应停止使用并提交 Issue。

### Alpine/musl 执行失败

部分上游 arm64 产物依赖 glibc。使用 `COPY` 提供对应 musl 静态二进制，并
重新 resolve/build。先用 `binport doctor HOST` 确认平台与产物选择。

## 16. 命令速查

| 命令 | 用途 |
|---|---|
| `binport host add NAME [DEST]` | 向导或非交互添加主机 |
| `binport host ls/show/test/remove` | 管理和检测主机路由 |
| `binport bastion presets/probe` | 查看模板、检测堡垒机能力 |
| `binport auth setup/status/remove` | 管理 Binport 专用 Key |
| `binport resolve [PATH]` | 生成或更新 `Binport.lock` |
| `binport build [PATH]` | 构建工具箱 |
| `binport ls/status [PATH]` | 查看声明、构建和缓存状态 |
| `binport fetch TOOL...` | 预下载工具 |
| `binport clean` | 清理下载缓存 |
| `binport HOST TOOL [ARGS]...` | 单机执行 |
| `binport @GROUP TOOL [ARGS]...` | Fleet 并发执行 |
| `binport cp SOURCE DEST` | 本地/远端文件复制 |
| `binport rm [-rf] HOST:PATH` | 删除远端文件或目录 |
| `binport tunnel SPEC... HOST` | 转发一个或多个 TCP 端口 |
| `binport plan TARGET TOOL` | 离线预览执行计划 |
| `binport doctor TARGET` | 检查连接、平台与缓存 |
| `binport warm TARGET` | 预热完整工具箱 |
| `binport watch ...` | 持续观察命令结果变化 |
| `binport export/load FILE` | 单文件离线分发 |
| `binport pack/unpack FILE` | OCI layout 离线分发 |
| `binport push/pull oci://...` | Registry/Harbor 分发 |

所有命令的即时参数说明以当前二进制为准：

```sh
binport --help
binport host add --help
binport watch --help
```

## 17. 获取帮助

提交问题前请附上：

- `binport --version`；
- 去除敏感信息后的完整命令；
- `binport host show HOST` 和 `binport host test HOST` 的脱敏输出；
- 必要时附 `--verbose` 或 `--json` 输出；
- 本地系统、远端 Linux 架构和连接路径类型。

项目入口：[README](../README.zh-CN.md) · [问题反馈](https://github.com/mengshi02/binport/issues) ·
[贡献指南](../CONTRIBUTING.md) · [版本记录](../CHANGELOG.md)
