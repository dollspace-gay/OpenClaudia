pub mod acp;
pub mod auth;
pub mod config_cmd;
pub mod doctor;
pub mod init;
pub mod loop_cmd;
pub mod start;

pub(crate) fn provider_api_key_env_var(provider_name: &str) -> &'static str {
    match provider_name {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "google" | "gemini" => "GOOGLE_API_KEY",
        "zai" | "glm" | "zhipu" => "ZAI_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "qwen" | "alibaba" => "QWEN_API_KEY",
        "kimi" | "moonshot" => "KIMI_API_KEY or MOONSHOT_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        _ => "API_KEY",
    }
}

pub(crate) fn can_start_without_api_key(provider_name: &str) -> bool {
    provider_name.eq_ignore_ascii_case("anthropic")
        || openclaudia::config::is_local_provider_name(provider_name)
}
