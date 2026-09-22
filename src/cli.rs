//! Command-line surface.
//!
//! Kept separate from `handlers/` so the shape of the CLI is readable in one
//! place. `use`/`unuse`/`off` print shell code for the caller to `eval`, which
//! is why the shell integration routes them through `__shell`.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "envpick",
    version,
    about = "profile 化的环境变量管理，带 TUI 与端到端加密同步",
    long_about = "用 profile 组织环境变量，按依赖组合激活，并可通过 pb.pka.moe \
                  在机器之间端到端加密同步。\n\n\
                  首次使用请把 shell 集成加入 rc 文件：\n  \
                  eval \"$(envpick init zsh)\""
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// 输出 shell 集成脚本（加入 rc 文件后即可使用 ep）
    Init {
        /// zsh 或 bash
        shell: String,
    },

    /// 激活 profile（需要 shell 集成；直接运行只会打印提示）
    Use {
        #[arg(required = true)]
        profiles: Vec<String>,
    },

    /// 撤销 profile
    Unuse {
        profiles: Vec<String>,
        /// 撤销全部（等同于 off）
        #[arg(long)]
        all: bool,
    },

    /// 撤销全部，把环境还原到激活之前
    Off,

    /// 列出所有 profile
    List,

    /// 查看某个 profile 解析后的变量
    Show {
        profile: String,
        /// 只看变量，不显示来源 profile
        #[arg(long)]
        plain: bool,
    },

    /// 查看当前激活状态与配置位置
    Status,

    /// 一致性检查（--fix 自动修复可修复的问题）
    Check {
        #[arg(long)]
        fix: bool,
    },

    /// 用 $EDITOR 打开 profiles.toml
    Edit,

    /// 新建一个空 profile
    New { name: String },

    /// 删除 profile
    Rm {
        name: String,
        /// 跳过确认
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// 设置变量：envpick set work EDITOR=nvim PAGER=less
    Set {
        profile: String,
        #[arg(required = true)]
        assignments: Vec<String>,
    },

    /// 删除变量：envpick unset work EDITOR
    Unset {
        profile: String,
        #[arg(required = true)]
        keys: Vec<String>,
    },

    /// 添加依赖：envpick require work corp-base
    Require {
        profile: String,
        #[arg(required = true)]
        deps: Vec<String>,
    },

    /// 移除依赖：envpick unrequire work corp-base
    Unrequire {
        profile: String,
        #[arg(required = true)]
        deps: Vec<String>,
    },

    /// 同步
    Sync {
        #[command(subcommand)]
        action: Option<SyncAction>,
        /// 冲突时保留本地（仅用于不带子命令的智能同步）
        #[arg(long, conflicts_with = "keep_remote")]
        keep_local: bool,
        /// 冲突时保留远端（仅用于不带子命令的智能同步）
        #[arg(long)]
        keep_remote: bool,
    },

    /// 打开 TUI
    Ui,

    /// 检查并安装最新版本（覆盖当前二进制）
    Update {
        /// 忽略版本比较，强制重装
        #[arg(long)]
        force: bool,
    },

    /// 内部命令：shell 集成用它取得可 eval 的片段
    #[command(name = "__shell", hide = true)]
    Shell { args: Vec<String> },
}

#[derive(Subcommand, Debug)]
pub enum SyncAction {
    /// 查看同步状态
    Status,
    /// 上传本地 profile 到远端（覆盖远端）
    Push,
    /// 用远端覆盖本地 profile
    Pull,
    /// 配置同步 ID 与密钥
    Init {
        /// 同步 ID；省略则随机生成
        id: Option<String>,
        /// 密钥短语；省略则保留已有的
        #[arg(long, conflicts_with = "key_stdin")]
        key: Option<String>,
        /// 从标准输入读一行作为密钥（避免留在 shell 历史里）
        #[arg(long)]
        key_stdin: bool,
    },
    /// 生成一个可用的随机同步 ID
    Genid,
    /// 打印可直读的浏览器链接
    Url,
    /// 删除远端 paste（本地 profile 保留）
    Delete {
        /// 跳过确认
        #[arg(long, short = 'y')]
        yes: bool,
    },
}
