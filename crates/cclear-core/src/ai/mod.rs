//! AI 咨询：地址校验、脱敏、提问。
//!
//! 这是全项目唯一会联网的模块（见 AGENTS.md 规则 4），调用方只能是 Tauri 命令
//! `ai_ask_rule`。AI 的回答只用于展示，不参与任何清理判断。

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::model::{Recovery, Tier};
use crate::rules::catalog::Rule;
use crate::rules::resolve::{Candidate, CandidateStatus};
use crate::win::http;

/// 响应体上限：2 MiB。正常回答远小于这个值。
const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

/// user 消息的字符上限。放不下时从样例路径那一段开始丢弃。
const USER_MESSAGE_LIMIT: usize = 8000;

/// 锚点和样例路径的条数上限。
const ANCHOR_LIMIT: usize = 5;
const SAMPLE_LIMIT: usize = 10;

/// 判定依据和备注的条数上限。
const NOTE_LIMIT: usize = 10;

/// 用户补充问题的字符上限。
const QUESTION_LIMIT: usize = 500;

/// 服务返回错误时，展示错误说明的前多少字符。
const SERVICE_ERROR_LIMIT: usize = 300;

/// 接口路径。地址没写到这一段时由 `endpoint_url` 补上。
const ENDPOINT_SUFFIX: &str = "/chat/completions";

/// AI 咨询的错误。信息会直接显示给用户，因此不允许带出 Key。
#[derive(Debug, Error)]
pub enum AiError {
    #[error("AI 服务地址不合法：{0}")]
    InvalidUrl(String),

    #[error("还没有配置 AI 服务，请先在设置里填写服务地址和模型名")]
    NotConfigured,

    #[error("访问 AI 服务失败：{0}")]
    Network(String),

    #[error("{0}")]
    Service(String),

    #[error("AI 服务的响应格式不对：{0}")]
    BadResponse(String),

    #[error("AI 服务的响应超过 2 MiB，已中止")]
    ResponseTooLarge,
}

/// 一条对话消息。字段名沿用 OpenAI 兼容接口要求的 `role` / `content`。
#[derive(Serialize)]
pub struct ChatMessage {
    pub role: &'static str,
    pub content: String,
}

/// 一条规则的咨询上下文：规则本身的说明，加上本次重新评估的结果。
pub struct RuleContext {
    pub name: String,
    pub tier: &'static str,
    pub recovery: &'static str,
    pub description: String,
    pub warning: Option<String>,
    pub processes: Vec<String>,
    pub status: String,
    pub allocated: u64,
    pub files: u64,
    pub anchors: Vec<PathBuf>,
    pub samples: Vec<PathBuf>,
    pub evidence: Vec<String>,
    pub notes: Vec<String>,
    pub question: Option<String>,
}

impl RuleContext {
    /// 从规则目录和评估结果组装上下文。中文名在这里定，命令层不用再映射一遍。
    pub fn new(rule: &Rule, candidate: &Candidate, question: Option<String>) -> Self {
        Self {
            name: rule.name.to_string(),
            tier: tier_label(rule.tier),
            recovery: recovery_label(rule.recovery),
            description: rule.description.to_string(),
            warning: rule.warning.map(str::to_string),
            processes: rule.processes.iter().map(|name| name.to_string()).collect(),
            status: status_text(&candidate.status),
            allocated: candidate.size.allocated,
            files: candidate.size.files,
            anchors: candidate.anchors.clone(),
            samples: candidate.samples.clone(),
            evidence: candidate.evidence.clone(),
            notes: candidate.notes.clone(),
            question,
        }
    }
}

/// 校验并规范化用户填写的服务地址。
///
/// 只允许 https，或者主机是本机的 http（本地 Ollama 之类）：明文把 Key 发到公网
/// 是不可接受的。带用户信息、查询串或片段的地址会把请求带到别处，一律拒绝。
/// 地址没写到 `/chat/completions` 时自动补上。
pub fn endpoint_url(base: &str) -> Result<String, AiError> {
    let text = base.trim();
    if text.is_empty() {
        return Err(AiError::InvalidUrl("地址不能为空".to_string()));
    }
    if text.contains('@') || text.contains('?') || text.contains('#') {
        return Err(AiError::InvalidUrl(
            "地址里不能带用户名、查询串或 #".to_string(),
        ));
    }

    let Some((secure, rest)) = split_scheme(text) else {
        return Err(AiError::InvalidUrl(
            "地址必须以 https:// 或 http:// 开头".to_string(),
        ));
    };

    let Some(host) = host_of(rest) else {
        return Err(AiError::InvalidUrl("地址里没有有效的主机名".to_string()));
    };

    if !secure && !is_local_host(host) {
        return Err(AiError::InvalidUrl(
            "只有 https 地址，或者本机的 http 地址才允许使用".to_string(),
        ));
    }

    let scheme = if secure { "https://" } else { "http://" };
    let url = format!("{scheme}{}", rest.trim_end_matches('/'));

    if ends_with_suffix(&url, ENDPOINT_SUFFIX) {
        return Ok(url);
    }

    Ok(format!("{url}{ENDPOINT_SUFFIX}"))
}

/// 把用户目录和用户名替换掉。
///
/// 先按“分隔符可以是 `\` 或 `/`、大小写不敏感”把整条用户目录换成 `%USERPROFILE%`，
/// 再把单独出现的用户名换成 `<用户名>`。后者只认左右是分隔符、空白或引号的完整片段，
/// 否则 `Alicex` 这种同前缀的目录名也会被误改。
pub fn redact(text: &str, profile: &Path) -> String {
    let profile_text = profile.as_os_str().to_string_lossy();
    let trimmed = profile_text.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        return text.to_string();
    }

    let replaced = replace_ignoring_case_and_separators(text, trimmed, "%USERPROFILE%");

    let Some(user_name) = trimmed.rsplit(['\\', '/']).next() else {
        return replaced;
    };
    if user_name.is_empty() {
        return replaced;
    }

    replace_standalone(&replaced, user_name, "<用户名>")
}

/// 组装发给 AI 的对话。所有用户侧文本都先经过脱敏。
pub fn rule_messages(ctx: &RuleContext, profile: &Path) -> Vec<ChatMessage> {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("规则名称：{}", ctx.name));
    lines.push(format!("分级：{}", ctx.tier));
    lines.push(format!("恢复方式：{}", ctx.recovery));
    lines.push(format!("说明：{}", ctx.description));
    if let Some(warning) = &ctx.warning {
        lines.push(format!("注意事项：{warning}"));
    }
    if !ctx.processes.is_empty() {
        lines.push(format!("需要关闭的进程：{}", ctx.processes.join("、")));
    }
    lines.push(format!("当前状态：{}", ctx.status));
    lines.push(format!("占用大小：{}", format_size(ctx.allocated)));
    lines.push(format!("文件数：{}", ctx.files));

    let anchors = ctx
        .anchors
        .iter()
        .take(ANCHOR_LIMIT)
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<String>>();
    let samples = ctx
        .samples
        .iter()
        .take(SAMPLE_LIMIT)
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<String>>();
    let evidence = ctx
        .evidence
        .iter()
        .take(NOTE_LIMIT)
        .cloned()
        .collect::<Vec<String>>();
    let notes = ctx
        .notes
        .iter()
        .take(NOTE_LIMIT)
        .cloned()
        .collect::<Vec<String>>();
    let question = ctx
        .question
        .as_ref()
        .map(|question| question.chars().take(QUESTION_LIMIT).collect::<String>())
        .filter(|question| !question.trim().is_empty())
        .into_iter()
        .collect::<Vec<String>>();

    let mut text = redact(&lines.join("\n"), profile);

    // 预算分配顺序和显示顺序不同：用户补充的问题、判定依据、备注、锚点先占预算，
    // 样例路径排最后。超长时只会少发几条样例，不会吞掉问题和核心段落，总长也不会超上限。
    let mut used = text.chars().count();
    let question = fit_section(&mut used, "用户补充的问题", &question, profile);
    let evidence = fit_section(&mut used, "判定依据", &evidence, profile);
    let notes = fit_section(&mut used, "备注", &notes, profile);
    let anchors = fit_section(&mut used, "锚点", &anchors, profile);
    let samples = fit_section(&mut used, "样例路径", &samples, profile);

    for part in [anchors, samples, evidence, notes, question]
        .into_iter()
        .flatten()
    {
        text.push_str(&part);
    }

    vec![
        ChatMessage {
            role: "system",
            content: system_prompt(),
        },
        ChatMessage {
            role: "user",
            content: text,
        },
    ]
}

/// 在剩余预算内逐条加入段落，成功后把用掉的字符数记到 `used` 上。
///
/// 一条都放不下时整段省略：宁可少发样例，也不要让整条消息超过上限。
fn fit_section(used: &mut usize, label: &str, values: &[String], profile: &Path) -> Option<String> {
    if values.is_empty() {
        return None;
    }

    let budget = USER_MESSAGE_LIMIT.saturating_sub(*used);
    let mut text = format!("\n{label}：");
    let mut length = text.chars().count();
    if length > budget {
        return None;
    }

    let mut added = 0;
    for value in values {
        let item = format!("\n- {}", redact(value, profile));
        let item_length = item.chars().count();
        if length + item_length > budget {
            break;
        }

        text.push_str(&item);
        length += item_length;
        added += 1;
    }

    if added == 0 {
        return None;
    }

    *used += length;
    Some(text)
}

/// 发一次请求，取回回答。Key 只在这个函数里以明文形式短暂存在。
pub fn ask(
    endpoint: &str,
    model: &str,
    api_key: &str,
    messages: &[ChatMessage],
) -> Result<String, AiError> {
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "temperature": 0.2,
        "stream": false,
    });
    let body = serde_json::to_vec(&body)
        .map_err(|error| AiError::BadResponse(format!("请求内容无法序列化：{error}")))?;

    let authorization = format!("Bearer {api_key}");
    let mut headers = vec![("Content-Type", "application/json")];
    if !api_key.is_empty() {
        headers.push(("Authorization", authorization.as_str()));
    }

    let response = match http::post_json(endpoint, &headers, &body, RESPONSE_LIMIT) {
        Ok(response) => response,
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            return Err(AiError::ResponseTooLarge);
        }
        Err(error) => return Err(AiError::Network(error.to_string())),
    };

    if !(200..300).contains(&response.status) {
        return Err(AiError::Service(service_error_message(
            response.status,
            &response.body,
        )));
    }

    let value: serde_json::Value = serde_json::from_slice(&response.body)
        .map_err(|_| AiError::BadResponse("返回内容不是 JSON".to_string()))?;

    value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            AiError::BadResponse("返回内容里没有 choices[0].message.content".to_string())
        })
}

/// 固定不变的 system 消息。
fn system_prompt() -> String {
    "你是 Windows C 盘清理顾问。根据给出的信息判断这些文件的用途和清理后会有什么影响。\
信息不足就直说“无法判断”，不要编造。\
请严格按下面四段回答：\n【这是什么】\n【清理后会怎样】\n\
【建议】只能是“建议清理”“可以清理，但要注意……”“不建议清理”“无法判断”之一\n【理由】\n\
总长度不超过 400 字。"
        .to_string()
}

/// 服务返回非 2xx 时的提示：状态码加上服务给的说明，取不到说明就只报状态码。
fn service_error_message(status: u32, body: &[u8]) -> String {
    let detail = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(|message| message.as_str())
                .map(str::to_string)
        });

    match detail {
        Some(detail) => format!(
            "AI 服务返回错误（状态码 {status}）：{}",
            detail.chars().take(SERVICE_ERROR_LIMIT).collect::<String>()
        ),
        None => format!("AI 服务返回错误（状态码 {status}）"),
    }
}

/// 拆出协议和剩下的部分。协议大小写不敏感。
fn split_scheme(text: &str) -> Option<(bool, &str)> {
    for (prefix, secure) in [("https://", true), ("http://", false)] {
        if let Some(head) = text.get(..prefix.len())
            && crate::win::equals_ignore_case(head, prefix)
        {
            return Some((secure, &text[prefix.len()..]));
        }
    }

    None
}

/// 从协议之后的部分取出主机名。主机为空，或者方括号 IPv6 的 `]` 后面不是空、`:端口` 时返回 None。
fn host_of(rest: &str) -> Option<&str> {
    let authority = rest.split('/').next().unwrap_or_default();

    let Some(inside) = authority.strip_prefix('[') else {
        let host = authority.split(':').next().unwrap_or_default();
        return if host.is_empty() { None } else { Some(host) };
    };

    // `]` 后面只允许空或 `:端口`：否则 `[::1]evil.com` 这种也会被当成主机名。
    let (host, tail) = inside.split_once(']')?;
    if host.is_empty() {
        return None;
    }
    if !tail.is_empty() {
        let port = tail.strip_prefix(':')?;
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
    }

    Some(host)
}

/// 主机是不是本机。
fn is_local_host(host: &str) -> bool {
    ["localhost", "127.0.0.1", "::1"]
        .iter()
        .any(|local| crate::win::equals_ignore_case(host, local))
}

/// `text` 是否以 `suffix` 结尾，大小写不敏感。
fn ends_with_suffix(text: &str, suffix: &str) -> bool {
    text.get(text.len().saturating_sub(suffix.len())..)
        .is_some_and(|tail| crate::win::equals_ignore_case(tail, suffix))
}

/// 把 `needle` 全部换成 `replacement`：比较时不区分大小写，`needle` 里的 `\` 也能匹配 `/`。
fn replace_ignoring_case_and_separators(text: &str, needle: &str, replacement: &str) -> String {
    let needle: Vec<char> = needle.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;

    while index < text.len() {
        if let Some(end) = match_needle(text, index, &needle) {
            out.push_str(replacement);
            index = end;
            continue;
        }

        let current = text[index..]
            .chars()
            .next()
            .expect("index 始终落在字符边界上");
        out.push(current);
        index += current.len_utf8();
    }

    out
}

/// 把独立出现的 `name` 换成 `replacement`。左右必须是边界，`Alicex` 这类不算。
fn replace_standalone(text: &str, name: &str, replacement: &str) -> String {
    let needle: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;

    while index < text.len() {
        let starts_here = index == 0 || text[..index].chars().next_back().is_some_and(is_boundary);
        if starts_here && let Some(end) = match_needle(text, index, &needle) {
            let ends_here =
                end == text.len() || text[end..].chars().next().is_some_and(is_boundary);
            if ends_here {
                out.push_str(replacement);
                index = end;
                continue;
            }
        }

        let current = text[index..]
            .chars()
            .next()
            .expect("index 始终落在字符边界上");
        out.push(current);
        index += current.len_utf8();
    }

    out
}

/// 从 `start`（字符边界）开始按上面的比较规则匹配 `needle`，命中时返回结束位置。
fn match_needle(text: &str, start: usize, needle: &[char]) -> Option<usize> {
    let mut chars = text[start..].char_indices();
    let mut end = start;

    for expected in needle {
        let (offset, actual) = chars.next()?;
        let matched = if *expected == '\\' || *expected == '/' {
            actual == '\\' || actual == '/'
        } else {
            crate::win::char_equals_ignore_case(*expected, actual)
        };
        if !matched {
            return None;
        }
        end = start + offset + actual.len_utf8();
    }

    Some(end)
}

/// 用户名替换时的边界：除 ASCII 字母、ASCII 数字、`_`、`-` 以外的任何字符都算边界
///（包括 `.`、中英文标点、汉字、空白、引号与分隔符），避免误伤 `Alicex`、`Alice_1`、`Alice-PC`
/// 这类标识符，同时让紧邻中文或标点的用户名也能正常脱敏。
fn is_boundary(current: char) -> bool {
    !current.is_ascii_alphanumeric() && current != '_' && current != '-'
}

/// 占用大小的展示写法，统一用二进制单位。
fn format_size(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    if bytes >= GIB {
        format!("{:.2} GB", bytes as f64 / GIB as f64)
    } else {
        format!("{:.2} MB", bytes as f64 / MIB as f64)
    }
}

/// 规则分级的中文名。
fn tier_label(tier: Tier) -> &'static str {
    match tier {
        Tier::Safe => "安全",
        Tier::Caution => "谨慎",
        Tier::Leftover => "残留",
        Tier::Manual => "手动（仅分析）",
        Tier::Forbidden => "禁区",
    }
}

/// 恢复方式的中文名。
fn recovery_label(recovery: Recovery) -> &'static str {
    match recovery {
        Recovery::Regenerable => "可再生：应用会自己重建",
        Recovery::IrreversibleFile => "不可恢复：文件被永久删除",
        Recovery::Quarantine => "先移入隔离区，可以还原",
        Recovery::IrreversibleCommand => "官方命令，不可撤销",
        Recovery::None => "不提供清理，只做展示",
    }
}

/// 评估状态的中文名，有补充说明时一并带上。
fn status_text(status: &CandidateStatus) -> String {
    let label = match status {
        CandidateStatus::Ready => "可以清理",
        CandidateStatus::NotPresent => "目录不存在或没装这个应用",
        CandidateStatus::ProcessRunning(_) => "相关应用正在运行",
        CandidateStatus::LayoutMismatch(_) => "目录布局和预期不符",
        CandidateStatus::PhaseLocked(_) => "对应阶段还没开放",
        CandidateStatus::Forbidden(_) => "落在禁区",
        CandidateStatus::Insufficient(_) => "证据不足，只做分析",
    };

    match status.detail() {
        Some(detail) => format!("{label}（{detail}）"),
        None => label.to_string(),
    }
}
