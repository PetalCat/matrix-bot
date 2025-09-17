use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::tools::{Tool, ToolContext, ToolSpec, ToolTriggers, send_text, str_conf, truncate};

pub fn register_defaults(specs: &mut Vec<ToolSpec>) {
    if !specs.iter().any(|t| t.id == "ai") {
        specs.push(ToolSpec {
            id: "ai".into(),
            enabled: true,
            dev_only: Some(true),
            triggers: ToolTriggers {
                commands: vec!["!ai".into()],
                mentions: vec![],
            },
            config: serde_yaml::Value::default(),
        });
    }
}

pub fn build() -> Arc<dyn Tool> {
    Arc::new(AiTool)
}

pub struct AiTool;

#[async_trait]
impl Tool for AiTool {
    fn id(&self) -> &'static str {
        "ai"
    }
    fn help(&self) -> &'static str {
        "Ask the AI: !ai <prompt> (dev only)"
    }
    fn dev_only(&self) -> bool {
        true
    }
    async fn run(&self, ctx: &ToolContext, args: &str, spec: &ToolSpec) -> Result<()> {
        #[derive(serde::Deserialize)]
        struct ChoiceMsg {
            content: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct Choice {
            message: ChoiceMsg,
        }
        #[derive(serde::Deserialize)]
        struct ChatResp {
            choices: Vec<Choice>,
        }
        #[derive(serde::Serialize)]
        struct Msg<'a> {
            role: &'a str,
            content: &'a str,
        }
        #[derive(serde::Serialize)]
        struct Body<'a> {
            model: &'a str,
            messages: Vec<Msg<'a>>,
            max_tokens: Option<u32>,
        }

        let prompt = args.trim();
        if prompt.is_empty() {
            return send_text(ctx, "Usage: !ai <prompt>").await;
        }

        let api_base = str_conf(spec, "api_base")
            .or_else(|| std::env::var("AI_API_BASE").ok())
            .unwrap_or_else(|| "https://api.openai.com".to_owned());
        let api_path = str_conf(spec, "api_path")
            .or_else(|| std::env::var("AI_API_PATH").ok())
            .unwrap_or_else(|| "/v1/chat/completions".to_owned());
        let model = str_conf(spec, "model")
            .or_else(|| std::env::var("AI_MODEL").ok())
            .unwrap_or_else(|| "gpt-4o-mini".to_owned());
        let api_key = str_conf(spec, "api_key")
            .or_else(|| std::env::var("AI_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok());
        if api_key.is_none() {
            return send_text(ctx, "AI_API_KEY (or OPENAI_API_KEY) not set").await;
        }
        let api_key = api_key.unwrap();
        let url = format!("{}{}", api_base.trim_end_matches('/'), api_path);

        let body = Body {
            model: &model,
            messages: vec![Msg {
                role: "user",
                content: prompt,
            }],
            max_tokens: Some(512),
        };
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await;
        match resp {
            Ok(r) => {
                if !r.status().is_success() {
                    let code = r.status();
                    let text = r.text().await.unwrap_or_default();
                    return send_text(ctx, format!("AI error: {}\n{}", code, truncate(&text, 400)))
                        .await;
                }
                match r.json::<ChatResp>().await {
                    Ok(p) => {
                        let out = p
                            .choices
                            .first()
                            .and_then(|c| c.message.content.as_ref())
                            .map(|s| s.trim().to_owned())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "<no content>".to_owned());
                        send_text(ctx, out).await
                    }
                    Err(e) => send_text(ctx, format!("Failed to parse AI response: {e}")).await,
                }
            }
            Err(e) => send_text(ctx, format!("Failed to call AI API: {e}")).await,
        }
    }
}
