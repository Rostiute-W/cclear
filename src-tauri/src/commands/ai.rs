//! AI 咨询命令：设置读写与单条规则的提问。
//!
//! 提问只读，不拿执行锁，也不会改变任何清理相关的状态。

use std::time::SystemTime;

use cclear_core::ai::{self, AiError, RuleContext};
use cclear_core::quarantine::{AiSettings, KeyUpdate, load_ai_settings, save_ai_settings};
use cclear_core::rules::catalog::find;
use cclear_core::rules::resolve::{RootMap, evaluate};
use cclear_core::win::process::running_processes;
use cclear_core::win::{KnownFolder, known_folder};
use serde::Serialize;

/// API Key 的长度上限。正常 Key 只有几十个字符，超长基本是粘贴错了。
const API_KEY_LIMIT: usize = 4096;

/// AI 设置卡：Key 只暴露“有没有配置”。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsDto {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub has_key: bool,
}

/// 一次提问的结果。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiAnswerDto {
    pub answer: String,
    pub model: String,
    /// 实际发出去的 user 消息（已脱敏），界面用它让用户看到发了什么。
    pub sent: String,
}

#[tauri::command]
pub fn ai_settings_get() -> Result<AiSettingsDto, String> {
    load_ai_settings()
        .map(settings_dto)
        .map_err(|error| format!("读取 AI 设置失败：{error}"))
}

#[tauri::command]
pub fn ai_settings_save(
    base_url: String,
    model: String,
    api_key: Option<String>,
) -> Result<AiSettingsDto, String> {
    // 保存时就校验地址：填错了不该等到点“问 AI”才发现。
    ai::endpoint_url(&base_url).map_err(|error| error.to_string())?;

    let model = model.trim().to_string();
    if model.is_empty() {
        return Err("模型名不能为空".to_string());
    }

    let update = key_update(api_key)?;
    save_ai_settings(base_url.trim(), &model, update)
        .map_err(|error| format!("保存 AI 设置失败：{error}"))?;

    load_ai_settings()
        .map(settings_dto)
        .map_err(|error| format!("读取 AI 设置失败：{error}"))
}

#[tauri::command]
pub async fn ai_ask_rule(rule_id: String, question: Option<String>) -> Result<AiAnswerDto, String> {
    // 要重新评估规则并等网络返回，放到阻塞线程池里执行。
    tauri::async_runtime::spawn_blocking(move || ask_rule(&rule_id, question))
        .await
        .map_err(|error| format!("提问失败：{error}"))?
}

/// 重新评估一条规则，把脱敏后的信息发给用户配置的 AI 服务。
fn ask_rule(rule_id: &str, question: Option<String>) -> Result<AiAnswerDto, String> {
    let rule = find(rule_id).ok_or_else(|| "这个规则不存在，请刷新页面".to_string())?;

    // 规则信息一律现场重新评估：前端只给了规则 ID 和可选问题。
    let roots = RootMap::system().map_err(|error| error.to_string())?;
    let procs = running_processes();
    let candidate = evaluate(rule, &roots, &procs, SystemTime::now());

    // 脱敏依据取真实的用户目录，路径里的用户名不会发出去。
    let profile = known_folder(KnownFolder::Profile).map_err(|_| "无法定位用户目录".to_string())?;

    let settings = load_ai_settings().map_err(|error| format!("读取 AI 设置失败：{error}"))?;
    let (Some(base_url), Some(model)) = (settings.base_url.clone(), settings.model.clone()) else {
        return Err(AiError::NotConfigured.to_string());
    };

    let endpoint = ai::endpoint_url(&base_url).map_err(|error| error.to_string())?;
    let api_key = settings
        .api_key()
        .map_err(|error| format!("无法解开已保存的 API Key：{error}"))?;

    let context = RuleContext::new(rule, &candidate, question);
    let messages = ai::rule_messages(&context, &profile);
    let answer =
        ai::ask(&endpoint, &model, &api_key, &messages).map_err(|error| error.to_string())?;

    // user 消息就是实际发出去的内容，界面拿它给用户看清楚。
    let sent = messages
        .get(1)
        .map(|message| message.content.clone())
        .unwrap_or_default();

    Ok(AiAnswerDto {
        answer,
        model,
        sent,
    })
}

/// 前端传来的 Key：None 保留原值，空串清除，其他值加密保存。
fn key_update(api_key: Option<String>) -> Result<KeyUpdate, String> {
    let Some(api_key) = api_key else {
        return Ok(KeyUpdate::Keep);
    };

    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Ok(KeyUpdate::Clear);
    }
    if api_key.chars().count() > API_KEY_LIMIT {
        return Err("API Key 太长，请重新复制".to_string());
    }

    Ok(KeyUpdate::Set(api_key.to_string()))
}

fn settings_dto(settings: AiSettings) -> AiSettingsDto {
    let has_key = settings.has_key();

    AiSettingsDto {
        base_url: settings.base_url,
        model: settings.model,
        has_key,
    }
}
