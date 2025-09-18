use core::fmt::Write as _;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use async_trait::async_trait;
use matrix_sdk::{
    Client,
    room::{MessagesOptions, Room},
    ruma::{
        MilliSecondsSinceUnixEpoch, OwnedRoomId, OwnedUserId,
        events::{
            AnySyncMessageLikeEvent, AnySyncTimelineEvent,
            room::message::{
                MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
                SyncRoomMessageEvent,
            },
        },
        serde::Raw,
    },
};
use tracing::{info, warn};

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

const DEFAULT_SYSTEM_PROMPT: &str = r"
You are an AI assistant embedded in a casual group chat between friends.
Your job is to be another participant in the chat, not an outside narrator.

Behavior Rules
• Keep replies short, friendly, and clear so they fit into casual conversation.
• If the group asks for something creative, detailed, or story-like (e.g. a script, story, long explanation), reply with that — but stay natural and easy to follow.
• Only reply when directly invoked (tagged) or when it’s obvious someone is asking for help.
• Match the vibe: playful when friends are joking, straightforward when answering a question.
• Emojis or light humor are fine if they fit, but don’t overdo it.
• Important: Always reply in the style of a group chat message — like you’re sending the next line, not writing an essay about the group.

Example Contexts & Invocations

Homework Help
Lena: ugh this history question is killing me
Max: lol what is it
Lena: what year did WW2 end??
Sam: easy, ask @Claire
@Claire (AI): 1945 ✅ Germany surrendered in May, Japan in September.

Weekend Plans
Billy: what do we wanna do Saturday?
Jamie: bowling?
Sam: eh kinda mid
Billy: true… @Claire any ideas?
@Claire (AI): Late-night movie + pizza run 🍕🎬 or mini-golf if y’all want something active.

Random Debate
Jamie: wait is cereal soup?
Billy: nahhh it’s not soup
Sam: bro it’s literally stuff in liquid
Jamie: lmao ok @Claire settle this
@Claire (AI): Cereal’s not really soup — soup’s usually hot and savory. But if you wanna be chaotic you can call it “breakfast soup” 😅

Story / Longer Response
Maya: bruh I’m bored tell me a scary story
Alex: yeah @Claire give us something spooky
@Claire (AI): Ok 👻 once, in a tiny mountain town, there was a single streetlight that never turned off… [story continues]

⸻

Real Use Case

Here’s the real convo. They tagged you. You have to reply next.

(context grabbed from the chat)

→ YOUR REPLY GOES HERE
";

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
        struct Msg {
            role: String,
            content: String,
        }
        #[derive(serde::Serialize)]
        struct Body {
            model: String,
            messages: Vec<Msg>,
            max_tokens: Option<u32>,
        }

        let (args_no_log, log_to_room) = extract_log_flag(args);
        let prompt = args_no_log.trim();
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
        // Resolve API key with precedence:
        // 1) config.api_key
        // 2) config.api_key_env -> read that env var
        // 3) env.AI_API_KEY
        // 4) env.OPENAI_API_KEY
        let mut key_source = String::new();
        let api_key = if let Some(k) = str_conf(spec, "api_key") {
            key_source = "config.api_key".into();
            Some(k)
        } else if let Some(env_name) = str_conf(spec, "api_key_env") {
            let val = std::env::var(&env_name).ok();
            if val.is_some() {
                key_source = format!("env.{env_name}");
            }
            val
        } else if let Ok(k) = std::env::var("AI_API_KEY") {
            key_source = "env.AI_API_KEY".into();
            Some(k)
        } else if let Ok(k) = std::env::var("OPENAI_API_KEY") {
            key_source = "env.OPENAI_API_KEY".into();
            Some(k)
        } else {
            None
        };
        if api_key.is_none() {
            warn!(
                "AI request blocked: no API key set (config.api_key, config.api_key_env, AI_API_KEY, or OPENAI_API_KEY)"
            );
            return send_text(ctx, "AI key missing: set config.api_key or config.api_key_env, or AI_API_KEY/OPENAI_API_KEY env").await;
        }
        let api_key = api_key.unwrap();
        let url = format!("{}{}", api_base.trim_end_matches('/'), api_path);

        let name = spec
            .config
            .get("name")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .or_else(|| std::env::var("AI_NAME").ok())
            .unwrap_or_else(|| "Claire".to_owned());

        let system_prompt_base = spec
            .config
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SYSTEM_PROMPT)
            .to_owned();

        // Build system prompt with the chat context injected; clarify routing flags
        let mut system_prompt = format!(
            "Your name is {name}. People will tag you as @{name}.
Note: tokens like -d/--dev are routing flags; ignore them in content—they are not part of your name.
{system_prompt_base}",
        );
        let ctx_lines = read_last_history(&ctx.history_dir, &ctx.room.room_id().to_owned(), 11);
        // Do not rewrite the latest invocation; the current message was already recorded in history pre-routing
        let context_lines = ctx_lines.join("\n");
        if !context_lines.is_empty() {
            system_prompt =
                system_prompt.replacen("(context grabbed from the chat)", &context_lines, 1);
        }

        // Log request metadata (not the full content or secrets)
        let sys_preview = crate::tools::truncate(&system_prompt, 200);
        let user_preview = crate::tools::truncate(prompt, 120);
        info!(
            model = %model,
            url = %url,
            ctx_lines = %ctx_lines.len(),
            key_source = %key_source,
            sys_preview = %sys_preview,
            user_preview = %user_preview,
            "AI request prepared"
        );

        let body = Body {
            model: model.clone(),
            messages: vec![
                Msg {
                    role: "system".into(),
                    content: system_prompt.clone(),
                },
                Msg {
                    role: "user".into(),
                    content: prompt.to_owned(),
                },
            ],
            max_tokens: Some(512),
        };

        if log_to_room {
            let mut log_text = String::new();
            let _ = writeln!(log_text, "AI -log");
            let _ = writeln!(log_text, "model: {model}");
            let _ = writeln!(log_text, "url:   {url}");
            let _ = writeln!(log_text, "context_lines: {}", ctx_lines.len());
            let _ = writeln!(log_text, "-- system_prompt --\n{system_prompt}");
            let _ = writeln!(log_text, "-- user_prompt --\n{prompt}");
            // send as a separate message (with dev header if active)
            let _ = send_text(ctx, log_text).await;
        }
        let client = reqwest::Client::new();
        let started = std::time::Instant::now();
        let resp = client
            .post(&url)
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await;
        match resp {
            Ok(r) => {
                let elapsed_ms = started.elapsed().as_millis();
                if !r.status().is_success() {
                    let code = r.status();
                    let text = r.text().await.unwrap_or_default();
                    warn!(status = %code, elapsed_ms, body_preview = %truncate(&text, 200), "AI API returned error status");
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
                        info!(elapsed_ms, reply_preview = %truncate(&out, 160), "AI response ok");
                        // Build bolded prefix using the same Unicode math-bold as relay (no Markdown/HTML)
                        let header = if ctx.dev_active {
                            "=======DEV MODE=======\n"
                        } else {
                            ""
                        };
                        let prefix = format!("@{name}:");
                        let bold_prefix = to_bold(&prefix);
                        let text = format!("{header}{bold_prefix} {out}");
                        let content = RoomMessageEventContent::text_plain(text);
                        ctx.room.send(content).await.map(|_| ()).map_err(Into::into)
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to parse AI response JSON");
                        send_text(ctx, format!("Failed to parse AI response: {e}")).await
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "HTTP error calling AI API");
                send_text(ctx, format!("Failed to call AI API: {e}")).await
            }
        }
    }
}

fn history_path(history_dir: &Path, room_id: &OwnedRoomId) -> PathBuf {
    let mut name = room_id.as_str().to_owned();
    name = name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
    history_dir.join(format!("{name}.log"))
}

pub fn append_history_line(history_dir: &Path, room_id: &OwnedRoomId, line: &str) {
    let path = history_path(history_dir, room_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut buf = line.to_owned();
    buf.push('\n');
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, buf.as_bytes()));
}

fn read_last_history(history_dir: &Path, room_id: &OwnedRoomId, n: usize) -> Vec<String> {
    let path = history_path(history_dir, room_id);
    if let Ok(data) = std::fs::read_to_string(&path) {
        let lines: Vec<String> = data.lines().map(ToOwned::to_owned).collect();
        let len = lines.len();
        let start = len.saturating_sub(n);
        return lines[start..].to_vec();
    }
    Vec::new()
}

async fn history_line_from_raw(
    room: &Room,
    raw_event: Raw<AnySyncTimelineEvent>,
    name_cache: &mut HashMap<OwnedUserId, String>,
) -> Option<String> {
    let event = raw_event.deserialize().ok()?;
    let AnySyncTimelineEvent::MessageLike(message_like) = event else {
        return None;
    };
    let AnySyncMessageLikeEvent::RoomMessage(msg) = message_like else {
        return None;
    };
    let SyncRoomMessageEvent::Original(OriginalSyncRoomMessageEvent {
        sender,
        content,
        origin_server_ts,
        ..
    }) = msg
    else {
        return None;
    };

    let body = match &content.msgtype {
        MessageType::Text(inner) => Some(inner.body.as_str()),
        MessageType::Notice(inner) => Some(inner.body.as_str()),
        MessageType::Emote(inner) => Some(inner.body.as_str()),
        _ => None,
    }?;

    let sanitized = crate::tools::sanitize_line(body, 400);
    if sanitized.is_empty() {
        return None;
    }

    let timestamp = format_timestamp(Some(origin_server_ts));
    let sender_name = resolve_display_name(room, name_cache, &sender).await;
    Some(format!("[{timestamp}] {sender_name}:{sanitized}"))
}

async fn resolve_display_name(
    room: &Room,
    cache: &mut HashMap<OwnedUserId, String>,
    user_id: &OwnedUserId,
) -> String {
    if let Some(name) = cache.get(user_id) {
        return name.clone();
    }
    let display = match room.get_member(user_id).await {
        Ok(Some(member)) => member
            .display_name()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| user_id.localpart().to_owned()),
        _ => user_id.localpart().to_owned(),
    };
    cache.insert(user_id.clone(), display.clone());
    display
}

fn format_timestamp(ts: Option<MilliSecondsSinceUnixEpoch>) -> String {
    if let Some(ts) = ts {
        if let Some(formatted) = timestamp_to_rfc3339(ts) {
            return formatted;
        }
    }
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

fn timestamp_to_rfc3339(ts: MilliSecondsSinceUnixEpoch) -> Option<String> {
    let millis = i128::from(ts.get());
    let nanos = millis.checked_mul(1_000_000)?;
    let dt = time::OffsetDateTime::from_unix_timestamp_nanos(nanos).ok()?;
    dt.format(&time::format_description::well_known::Rfc3339)
        .ok()
}

pub async fn backfill_all(client: Client, history_dir: PathBuf, limit: usize) {
    if limit == 0 {
        info!(dir = %history_dir.display(), "AI backfill skipped because limit is zero");
        return;
    }

    let rooms = client.joined_rooms();
    info!(rooms = rooms.len(), limit, dir = %history_dir.display(), "AI backfill start");

    for room in rooms {
        let room_id = room.room_id().to_owned();
        let mut from_token = room.last_prev_batch();
        if from_token.is_none() {
            info!(room = %room_id, "AI backfill: no prev_batch token; starting from timeline end");
        }

        let mut remaining = limit;
        let mut total_appended = 0usize;
        let mut page_counter = 0usize;
        let mut name_cache: HashMap<OwnedUserId, String> = HashMap::new();

        while remaining > 0 {
            page_counter += 1;
            let batch = remaining.min(50);
            let mut options = MessagesOptions::backward();
            options.from = from_token.clone();
            options.limit = (batch as u32).into();

            let response = match room.messages(options).await {
                Ok(res) => res,
                Err(err) => {
                    warn!(room = %room_id, error = %err, "AI backfill: room/messages request failed");
                    break;
                }
            };

            let next_token = response.end.clone();
            if response.chunk.is_empty() {
                info!(room = %room_id, pages = page_counter, fetched = total_appended, "AI backfill: empty chunk returned");
                break;
            }

            let mut appended_this_page = 0usize;
            for timeline_event in response.chunk.into_iter().rev() {
                if remaining == 0 {
                    break;
                }
                if let Some(line) =
                    history_line_from_raw(&room, timeline_event.into_raw(), &mut name_cache).await
                {
                    append_history_line(&history_dir, &room_id, &line);
                    appended_this_page += 1;
                    total_appended += 1;
                    remaining = remaining.saturating_sub(1);
                }
            }

            info!(
                room = %room_id,
                page = page_counter,
                appended = appended_this_page,
                total = total_appended,
                remaining,
                "AI backfill page complete"
            );

            if remaining == 0 {
                break;
            }

            let Some(token) = next_token else { break };
            if token.is_empty() || from_token.as_deref() == Some(token.as_str()) {
                break;
            }
            from_token = Some(token);
        }

        info!(room = %room_id, fetched = total_appended, "AI backfill room done");
    }

    info!("AI backfill complete");
}

// context cleansing helper removed; we now use exact history lines

fn extract_log_flag(args: &str) -> (String, bool) {
    let mut out: Vec<&str> = Vec::new();
    let mut flag = false;
    for t in args.split_whitespace() {
        if t == "-log" || t == "--log" {
            flag = true;
        } else {
            out.push(t);
        }
    }
    (out.join(" "), flag)
}

fn to_bold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' => char::from_u32('𝐀' as u32 + (c as u32 - 'A' as u32)).unwrap_or(c),
            'a'..='z' => char::from_u32('𝐚' as u32 + (c as u32 - 'a' as u32)).unwrap_or(c),
            '0'..='9' => char::from_u32('𝟎' as u32 + (c as u32 - '0' as u32)).unwrap_or(c),
            _ => c,
        })
        .collect()
}
