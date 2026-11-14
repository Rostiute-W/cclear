//! 工具缓存目录的官方配置方式。
//!
//! 每个工具用哪几项配置、没配置时默认在哪、用哪条命令验证、哪些进程会写缓存，
//! 全部集中在这张表里。迁移流程只按表工作，不针对某个工具写特例。

use crate::model::KnownRoot;

/// 工具的标识。命令参数、迁移记录和日志都用这里的字符串。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolId {
    Uv,
    Pip,
    Npm,
    NuGet,
    Gradle,
}

impl ToolId {
    /// 命令参数和记录里用的名字。
    pub fn key(self) -> &'static str {
        match self {
            ToolId::Uv => "uv",
            ToolId::Pip => "pip",
            ToolId::Npm => "npm",
            ToolId::NuGet => "nuget",
            ToolId::Gradle => "gradle",
        }
    }

    /// 按名字解析。前端只传这个名字，解析不出来说明传了无效值。
    pub fn parse(text: &str) -> Option<Self> {
        TOOLS.iter().map(|spec| spec.id).find(|id| id.key() == text)
    }
}

/// 一项要迁移的配置：一个环境变量（npm 是 npmrc 里的 `cache`）对应一个目录。
pub struct CacheEntry {
    /// 用户环境变量名。None 表示 npm 的 npmrc 配置，用 `npm config set` 写。
    pub env_var: Option<&'static str>,
    /// 目标卷下的目录名，最终路径是 `<卷>:\CClearData\<它>`。
    pub dir_name: &'static str,
    /// 没有配置时工具的默认位置。
    pub default_root: KnownRoot,
    pub default_rel: &'static str,
}

/// 一个工具的迁移配置。
pub struct ToolSpec {
    pub id: ToolId,
    pub label: &'static str,
    /// 改配置和验证要用的工具名，按 PATH 查找。
    pub program: &'static str,
    /// 查询生效路径的验证命令。None 表示只能检查目录（Gradle 没有查询命令）。
    pub validate_args: Option<&'static [&'static str]>,
    /// 会写这些缓存的进程，运行时界面要警告。
    pub processes: &'static [&'static str],
    pub entries: &'static [CacheEntry],
    /// 需要用户特别注意的地方。
    pub warning: Option<&'static str>,
}

/// 工具表。顺序就是界面表格的顺序。
static TOOLS: &[ToolSpec] = &[
    ToolSpec {
        id: ToolId::Uv,
        label: "uv",
        program: "uv",
        validate_args: Some(&["cache", "dir"]),
        processes: &["uv.exe"],
        entries: &[CacheEntry {
            env_var: Some("UV_CACHE_DIR"),
            dir_name: "uv",
            default_root: KnownRoot::LocalAppData,
            default_rel: "uv\\cache",
        }],
        warning: None,
    },
    ToolSpec {
        id: ToolId::Pip,
        label: "pip",
        program: "pip",
        validate_args: Some(&["cache", "dir"]),
        processes: &["python.exe", "pythonw.exe", "pip.exe"],
        entries: &[CacheEntry {
            env_var: Some("PIP_CACHE_DIR"),
            dir_name: "pip",
            default_root: KnownRoot::LocalAppData,
            default_rel: "pip\\Cache",
        }],
        warning: None,
    },
    ToolSpec {
        id: ToolId::Npm,
        label: "npm",
        program: "npm",
        validate_args: Some(&["config", "get", "cache"]),
        processes: &["node.exe", "npm.exe"],
        entries: &[CacheEntry {
            // npm 的缓存位置存在用户级 npmrc 里，没有对应的用户环境变量。
            env_var: None,
            dir_name: "npm",
            default_root: KnownRoot::RoamingAppData,
            default_rel: "npm-cache",
        }],
        warning: None,
    },
    ToolSpec {
        id: ToolId::NuGet,
        label: "NuGet",
        program: "dotnet",
        validate_args: Some(&["nuget", "locals", "all", "--list"]),
        processes: &["dotnet.exe", "MSBuild.exe"],
        entries: &[
            CacheEntry {
                env_var: Some("NUGET_PACKAGES"),
                dir_name: "nuget-packages",
                default_root: KnownRoot::Profile,
                default_rel: ".nuget\\packages",
            },
            CacheEntry {
                env_var: Some("NUGET_HTTP_CACHE_PATH"),
                dir_name: "nuget-http-cache",
                default_root: KnownRoot::LocalAppData,
                default_rel: "NuGet\\v3-cache",
            },
        ],
        warning: None,
    },
    ToolSpec {
        id: ToolId::Gradle,
        label: "Gradle",
        program: "gradle",
        // Gradle 没有查询自己目录的命令：`GRADLE_USER_HOME` 会整体接管 `.gradle`，
        // 包括配置和 wrapper，验证只能确认目标目录确实存在。
        validate_args: None,
        processes: &["java.exe", "javaw.exe", "gradle.exe"],
        entries: &[CacheEntry {
            env_var: Some("GRADLE_USER_HOME"),
            dir_name: "gradle",
            default_root: KnownRoot::Profile,
            default_rel: ".gradle",
        }],
        warning: Some(
            "GRADLE_USER_HOME 会接管整个 .gradle 目录：里面的配置、wrapper 和构建缓存一起搬走，\
             迁移后所有项目都用新位置，请确认目标盘空间足够",
        ),
    },
];

/// 全部工具。
pub fn all() -> &'static [ToolSpec] {
    TOOLS
}

/// 按标识取配置。表里没有说明调用方传了无效的工具。
pub fn spec(id: ToolId) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|spec| spec.id == id)
}
