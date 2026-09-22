# envpick

用 profile 组织环境变量，按依赖组合激活，并通过 [pb.pka.moe](https://pb.pka.moe)
在机器之间做**端到端加密**同步。带一个 ratatui 写的 TUI。

- 二进制叫 `envpick`，shell 里注册的简写命令是 **`ep`**
- 同步只需要在配置里填**同步 ID + 密钥**两样东西
- 服务端全程只拿得到密文：加密密钥在 HKDF 域分离后**从不离开本机**

```
$ ep use work
已激活: global, work
另含依赖: corp-base

$ ep show work
# work
# requires: corp-base
# 激活顺序: corp-base -> work
EDITOR='nvim'  # corp-base
HTTPS_PROXY='http://proxy.corp:7890'  # work
PAGER='less'  # corp-base
http_proxy='http://proxy.corp:7890'  # work

$ ep off
已撤销
```

注意 `ep use work` 说「另含依赖」：`work` 自己没定义 `EDITOR`，是依赖
`corp-base` 带进来的。`ENVPICK_ACTIVE` 里只记用户点名的 profile，但依赖确实改了
你的环境，所以 `ep list` 会把它们标成「激活中（依赖）」而不是装作没激活。

---

## 安装

一行装最新版（预编译二进制，装到 `~/.local/bin`）：

```sh
curl -fsSL https://raw.githubusercontent.com/prnake/envpick/main/install.sh | bash
```

或者从源码编译：

```sh
git clone https://github.com/prnake/envpick && cd envpick
./install.sh                    # 编译并装到 ~/.local/bin
# 或 cargo install --path .     # 装到 ~/.cargo/bin
```

`PREFIX=/usr/local/bin ./install.sh` 可以装到别处。下载安装会**强制校验
SHA256**：装进去的东西每次开 shell 都会跑，缺校验和就宁可失败，不装没校验过的
二进制。脚本只装二进制，不改你的 rc 文件。

### 升级

```sh
envpick update          # 检查并安装最新 release（覆盖当前二进制）
envpick update --force  # 忽略版本比较，强制重装
```

另外，任何命令启动时如果发现新版，会在 stderr 打一行提示。这个检查按天缓存
（`~/Library/Caches/envpick/update-check`），失败时一小时后再试 —— 离线过的机器
不该整天以为自己是最新的。`ENVPICK_NO_UPDATE_CHECK=1` 可以完全关掉。

> 更新只走 `envpick update` 这一条路，不会自动发生。一个会在你没看着的时候
> 重写自己的工具，比一个稍微旧一点的工具危险得多。

## Shell 集成

`use` / `unuse` / `off` 要修改的是**当前 shell** 的环境，而子进程改不了父进程的环境，
所以这三个命令由 shell 函数接住、`eval` 二进制的输出。把下面这行加进 rc 文件：

```sh
# ~/.zshrc（bash 用 ~/.bashrc）
eval "$(envpick init zsh)"
```

它做两件事：定义 `ep` 函数，以及**在新 shell 里自动激活 `global`**（以及
`settings.toml` 里 `default_profiles` 列出的 profile）。

验证：

```sh
ep list                 # 能列出 profile 就说明集成生效了
ep use work && env | grep EDITOR
ep off                  # 还原
```

`ep` 的退出码就是二进制本身的退出码，`ep use typo` 会以非零退出 —— 所以
`ep use prod || echo 失败` 和 `set -e` 脚本都能正常工作。这一点需要函数里多写两行
才能成立：`use` 类命令的报错走 stderr、stdout 是空的，而 `eval ""` 是**成功**的。

> 没加载集成就直接敲 `envpick use work` 时，程序会检测到自己被人类直接调用
> （而不是被 shell 函数 `eval`），打印一行怎么加载集成的提示，而不是默默什么都不做。

## 快速开始

```sh
ep new corp-base
ep set corp-base EDITOR=nvim PAGER=less

ep new work
ep set work http_proxy=http://proxy.corp:7890 HTTPS_PROXY=http://proxy.corp:7890
ep require work corp-base       # 激活 work 时先激活 corp-base

ep use work
ep status
```

`profiles.toml` 长这样 —— 也可以直接用 `ep edit` 打开编辑：

```toml
[profiles.corp-base.vars]
EDITOR = "nvim"
PAGER  = "less"

# 只有 requires 非空的 profile 才会有这个裸表头；
# 否则序列化器只写 [profiles.<名字>.vars]
[profiles.work]
requires = ["corp-base"]

[profiles.work.vars]
http_proxy  = "http://proxy.corp:7890"
HTTPS_PROXY = "http://proxy.corp:7890"
```

依赖按拓扑序激活，**后激活的覆盖先激活的**；`ep show work` 会把每个变量的最终来源
标出来，`ep require` 会拒绝任何造成环的依赖（并报出环的路径），`ep check` 会报出
悬空依赖。

> 手写这个文件时，写错的键会**直接报错**而不是被忽略 —— 比如把 `requires` 写成
> `require`，或者放进 `[profiles.work.vars]` 里面。这一点是刻意的：一个看起来正确、
> 实际被静默丢弃的配置比一个明确的报错难查得多。

## TUI

```sh
ep ui
```

四个视图，`Tab` / `Shift-Tab` 切换，`?` 呼出帮助：

| 视图 | 能做什么 |
|---|---|
| **Profiles** | 列表 + 详情，新增 / 删除 / 改依赖 |
| **Editor** | 变量表格增删改，`requires` 列表 |
| **Sync** | 同步状态，推送 / 拉取 / 智能同步 / 生成浏览器直读链接 |
| **Settings** | sync_id、密钥（掩码输入）、endpoint、过期时间 |

两点需要说明：

- **TUI 不能激活 profile**。道理和上面一样，子进程改不了父进程的环境。所以详情页里
  显示的是可以直接复制执行的 `ep use <name>` 命令，而不是假装按个键就激活了。
- **冲突不替你做决定**。两边都改过时会弹出左右对比，由你选保留哪一边。

## 同步

同步用 [pb.pka.moe](https://pb.pka.moe)（[pastebin-worker](https://github.com/SharzyL/pastebin-worker)，
跑在 Cloudflare Workers 上）。它**不是** PocketBase，只是一组简单的 HTTP 接口，
客户端把加密后的文档 PUT 上去，用 `sync_id` 当 paste 名，用从密钥派生的密码占位。

### 配置

```sh
ep sync genid                       # 生成一个随机同步 ID
ep sync init <那个 ID> --key-stdin  # 从标准输入读密钥（不留在 shell 历史里）
ep sync push                        # 第一次推送，创建远端 paste
```

另一台机器上执行同样的 `ep sync init <同一个 ID>`，输入**同一个密钥**，然后：

```sh
ep sync pull
```

日常只要 `ep sync`（不带子命令），它会自己判断该推还是该拉：

```
本地脏  = hash(本地文档) != 上次同步时的 hash
远端变  = hash(远端文档) != 同一个 hash

两者皆否 -> 已是最新
仅本地脏 -> push
仅远端变 -> pull
两者皆是 -> 冲突，等你决定（退出码 2）
```

两边比的都是自己手里的**内容**，所以机器之间时钟不一致不会误判。
用内容而不是版本号也有一层原因：解决冲突时选择「保留本地」的两台机器
可能各自算出同一个版本号，落后的一方会读回自己的数字、以为已经同步；
内容不会这样撞车。

（顺带一提，pb.pka.moe 并没有可用的元数据接口，`/m/<name>` 返回的就是
paste 正文，所以「远端变没变」这条信息本来也只能从正文里读。）

远端存在但**解不开**（密钥不对，或者那个名字被别人的 paste 占了）时，
状态是 `远端无法解密` 而不是「冲突」，`ep sync` 会拒绝执行。
解不开的东西没法恢复，所以它绝不会被自动覆盖；确实要覆盖就明说：

```sh
ep sync push      # 命令行里的出口，不加确认
```

TUI 里按 `p` 也会先弹一次确认再覆盖 —— 那一下按下去毁掉的是从没读到过的东西，
和「推送我的改动」不是同一件事。其余状态下 `p` 仍是按一下就推。

### 密钥放在哪

`~/.config/envpick/settings.toml`，文件权限 `0600`：

```toml
device_id = "a1b2c3d4e5f60718"

[sync]
endpoint = "https://pb.pka.moe"
sync_id  = "kQ3vX8mNp2LrT7wZbY4s"
key      = "我的密钥短语"
expire   = "90d"
```

CI 里可以用环境变量覆盖文件里的密钥，**环境变量优先**：

```sh
ENVPICK_SYNC_KEY=... ep sync
```

**`ep sync pull` 会重写 `profiles.toml`**：内容是解析后重新生成的，所以手写的
注释和空行会丢，键的顺序也会变成排好序的。profile 和变量本身一个不少，
但如果你在 `profiles.toml` 里写了注释，pull 之后得重新写一遍。

### 加密是怎么做的

```
ikm   = 密钥短语
salt  = sync_id
k_enc = HKDF-SHA256(ikm, salt, "envpick/v1/encryption")       -> 32B，AES-256-GCM 密钥
k_pw  = HKDF-SHA256(ikm, salt, "envpick/v1/manage-password")  -> 32B，paste 管理密码

上传内容 = base64variant( iv[12] || AES-256-GCM(k_enc, iv, "ENVPICK1\n" + JSON文档) )
```

几个刻意的选择：

- **域分离**。pastebin 的 `PUT` / `DELETE` 要在 URL 里带上管理密码，所以 `k_pw` 对
  服务端是可见的。它由 HKDF 从同一把密钥独立派生，服务端拿到 `k_pw` 也**推不出 `k_enc`**，
  因此推不出明文。这就是「配置里只填同步 ID + 密钥」能成立、而服务端拿不到明文的原因。
- **`ENVPICK1\n` magic**。用来区分「密钥不对」和「这压根不是本工具写的数据」，
  报错信息才有意义。
- **`base64variant`**（标准 base64 把 `/` 换成 `_`、去掉 `=` 填充）是为了对齐
  pastebin 网页版的加解密实现，这样浏览器能直接读：

  ```sh
  ep sync url
  # https://pb.pka.moe/d/~ep-<sync_id>#<base64variant(k_enc)>
  ```

  `#` 后面是加密密钥。**浏览器不会把 fragment 发给服务器**，所以密钥不会进服务端日志；
  但拿到整个链接的人就能解密内容，别公开分享。
- **`sync_id` 就是 paste 名**（加 `ep-` 前缀）。名字被猜到的后果只是暴露密文长度；
  管理密码由密钥派生，别人既覆盖不了也删不掉。

### 冲突与过期

两边都改过时，命令行会打印两侧的摘要并以退出码 `2` 结束，**不会**静默选一边：

```sh
ep sync                    # 看到冲突提示
ep sync --keep-local       # 保留本地，覆盖远端
ep sync --keep-remote      # 保留远端，覆盖本地
ep ui                      # 或者在 TUI 里逐项对比再决定
```

paste 是会过期的（上游默认 7 天，最长 90 天）。每次 push 都会续期，
`ep sync status` 在剩余时间不足 25% 时提醒——因为过期的失败方式是**静默的**：
下一次 push 只会新建一个 paste，历史就没了。

提醒用的是**服务端回执里写的**实际寿命，不是我们请求的那个值：
上游会把自己的上限卡在请求值上，所以请求 90 天未必真的拿到 90 天。
回执里没说寿命时就不提醒，而不是拿一个猜的数字吓人。

```sh
ep sync status
ep sync delete --yes       # 删掉远端 paste（本地 profile 保留）
```

## 命令一览

```
ep init <zsh|bash>              输出 shell 集成脚本
ep use <profile>...             激活（需要 shell 集成）
ep unuse <profile>... | --all   撤销指定 / 全部
ep off                          撤销全部，还原环境

ep list                         列出所有 profile
ep show <profile> [--plain]     查看解析后的变量与来源
ep status                       当前激活状态与配置位置
ep check [--fix]                一致性检查（环、悬空依赖）

ep new <name>                   新建空 profile
ep rm <name> [--yes]            删除（有依赖者时需要 --yes，随后清理悬空依赖）
ep set <profile> K=V [K=V...]   设置变量
ep unset <profile> K [K...]     删除变量
ep require <profile> <dep>...   添加依赖（造成环时拒绝）
ep unrequire <profile> <dep>... 移除依赖
ep edit                         用 $EDITOR 打开 profiles.toml

ep sync [status|push|pull|init|genid|url|delete]
ep sync --keep-local / --keep-remote
ep ui                           打开 TUI
ep update [--force]             升级到最新 release
```

`ep update` 要用完整命令名 `envpick update` —— shell 集成里的 `ep` 只特判
`use` / `unuse` / `off` 三个动词，其余原样转发给二进制，所以两个名字都能用。

## 文件位置

| 路径 | 内容 | 是否同步 |
|---|---|---|
| `~/.config/envpick/settings.toml` | 设备 ID、同步 ID、**密钥**、远端状态 | **否** |
| `~/.config/envpick/profiles.toml` | profile 定义 | 是（加密后） |
| `~/.config/envpick/profiles.toml.tmp` | 原子写的临时文件，进程在中途被杀时可能留下 | — |
| `~/Library/Caches/envpick/update-check` | 版本检查的缓存（两行：时间戳、tag） | — |

配置目录遵循 `$ENVPICK_CONFIG_DIR` → `$XDG_CONFIG_HOME/envpick` → 平台默认目录。
`profiles.toml`（唯一装着你手写内容的文件）用「临时文件 + rename」写入，
写到一半崩了也不会截断你的 profile。看到 `.tmp` 残留说明上次写入被中断，
`profiles.toml` 本身仍是完整的。

版本检查的缓存**刻意不放在配置目录**：配置目录是会被同步、会被备份的东西，
而缓存两样都不是。

## 激活是怎么还原的

激活信息记在**当前 shell 的变量里**，不落盘。这样两个终端各用各的 profile 时不会互相踩：

| 变量 | 作用 |
|---|---|
| `ENVPICK_ACTIVE` | 已激活的 profile 名（用户点名的那些，不含自动带进来的依赖） |
| `ENVPICK_SAVED_<变量名>` | 已经改过这个变量了（导出，供下次调用读回） |
| `ENVPICK_ORIG_<变量名>` / `ENVPICK_HAD_<变量名>` | 原值 / 原本是否存在 |

`ENVPICK_ACTIVE` 只记「你点名的」，因为依赖是每次激活时按当前配置重新展开的。
但依赖**确实改了你的环境**，所以 `ep list` 会把它们显示成「激活中（依赖）」、
`ep status` 会单列一行「另含依赖」—— 否则列表会告诉你 `corp-base` 没激活，
而它的 `EDITOR` 正在你 shell 里生效。这一点只影响显示，不影响上面那张表的分工。

**还原的依据是「实际改过哪些变量」，而不是「当时激活了哪些 profile」。** 这区别很重要：
如果按 profile 现算，那么在你 `ep use work`（顺带激活了依赖 `base`）之后，只要中间
把 `ep unrequire work base` 或 `ep rm base` 做了，再 `ep off` 时重新展开 `work` 就不会
再提到 `base` 的变量 —— 它们会被永久留在你的 shell 里。按 `ENVPICK_SAVED_*` 还原则不受影响，
连「profile 已被删除」的情况也能正确回滚。

撤销时按记录精确还原：变量激活前有值就恢复原值，**激活前不存在就 `unset`**，
而不是留下一个空字符串。`ep off` 表示「全部关掉」，包括 `global`。

## 开发

```sh
cargo test              # 单元 + 集成测试（离线，含一个手写的 mock HTTP server）
cargo build --release
```

测试里没有真实网络请求。`src/sync/mock.rs` 是一个手写的极小 TCP HTTP server，
覆盖创建 / 更新 / 密码错 / 404 / 重名等路径。
