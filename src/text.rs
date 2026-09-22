//! All user-facing strings in one place, so wording stays consistent and can be
//! changed (or later translated) without hunting through the code.
//!
//! Borrowed from the Go `envpick`, which keeps its strings in typed structs.

pub mod errors {
    pub const NO_CONFIG_DIR: &str =
        "无法确定配置目录，请设置 XDG_CONFIG_HOME 或 ENVPICK_CONFIG_DIR";
    pub const PROFILE_NOT_FOUND: &str = "profile 不存在";
    pub const PROFILE_NAME_INVALID: &str = "profile 名不合法";
    pub const PROFILE_NAME_RESERVED: &str = "该名字是保留名，不能使用";
    pub const VAR_NAME_INVALID: &str = "环境变量名不合法（只允许 [A-Za-z_][A-Za-z0-9_]*）";
    pub const CYCLE: &str = "profile 依赖存在环";
    pub const SYNC_NOT_CONFIGURED: &str = "尚未配置同步，请先设置 sync_id 和 key";
    pub const SYNC_KEY_MISSING: &str = "缺少同步密钥，请设置 sync.key 或 ENVPICK_SYNC_KEY";
    pub const SYNC_ID_INVALID: &str =
        "sync_id 不合法：需 3-64 个字符，只允许字母、数字、'-' 和 '_'";
    pub const DECRYPT_FAILED: &str = "解密失败：密钥不匹配，或远端数据已损坏";
    pub const NOT_ENVPICK_DATA: &str = "远端数据不是本工具写入的格式";
    pub const REMOTE_WRONG_PASSWORD: &str =
        "远端拒绝：管理密码不正确（通常意味着密钥与创建时不一致）";
    pub const REMOTE_NAME_TAKEN: &str = "远端已存在同名 paste，且不属于当前密钥";
    pub const REMOTE_NOT_FOUND: &str = "远端不存在该 paste";
    pub const REMOTE_UNREADABLE: &str = "远端 paste 无法解密，已停止：密钥可能与该 paste 创建时用的不一致。\
若确认要覆盖它，用 `envpick sync push`";
    pub const NOT_TTY: &str = "TUI 需要交互式终端";
}

pub mod messages {
    pub const NEED_SHELL_INIT: &str = "当前 shell 未加载 envpick 集成，无法修改环境变量";
    pub const NEED_SHELL_INIT_HINT: &str = r#"请先执行：eval "$(envpick init zsh)"   # 或 bash"#;
    pub const UP_TO_DATE: &str = "已是最新，无需同步";
    pub const PUSHED: &str = "已推送到远端";
    pub const PULLED: &str = "已从远端拉取";
    pub const ACTIVATED: &str = "已激活";
    pub const DEACTIVATED: &str = "已撤销";
}

pub mod prompts {
    pub const CONFLICT_TITLE: &str = "检测到同步冲突";
    pub const CONFLICT_BODY: &str = "远端和本地都有改动。请选择要保留的版本（放弃的一方会丢失）。";
    pub const CONFIRM_DELETE: &str = "确认删除？此操作不可撤销";
}
