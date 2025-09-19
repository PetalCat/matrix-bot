use core::fmt::Write as _;
use std::{
    borrow::ToOwned,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Once},
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

use plugin_core::factory::PluginFactory;
use plugin_core::{
    Plugin, PluginContext, PluginSpec, PluginTriggers, RoomMessageMeta, sanitize_line, send_text,
    str_config, truncate,
};

pub struct AiPlugin;

static HISTORY_BACKFILL_ONCE: Once = Once::new();

impl PluginFactory for AiPlugin {
    fn register_defaults(&self, specs: &mut Vec<PluginSpec>) {
        if let Some(spec) = specs.iter_mut().find(|t| t.id == "ai") {
            if !spec
                .triggers
                .commands
                .iter()
                .any(|cmd| cmd.eq_ignore_ascii_case("!ai"))
            {
                spec.triggers.commands.push("!ai".into());
            }
            if let Some(handle) = ai_env_handle() {
                if !spec
                    .triggers
                    .mentions
                    .iter()
                    .any(|mention| mention.eq_ignore_ascii_case(&handle))
                {
                    spec.triggers.mentions.push(handle);
                }
            }
        } else {
            let mut triggers = PluginTriggers {
                commands: vec!["!ai".into()],
                mentions: Vec::new(),
            };
            if let Some(handle) = ai_env_handle() {
                triggers.mentions.push(handle);
            }
            specs.push(PluginSpec {
                id: "ai".into(),
                enabled: true,
                dev_only: None,
                triggers,
                config: serde_yaml::Value::default(),
            });
        }
    }

    fn build(&self) -> Arc<dyn Plugin> {
        Arc::new(AiTool)
    }
}

const DEFAULT_SYSTEM_PROMPT: &'static str = r"
You are an AI assistant embedded in a casual group chat between friends.
Your job is to be another participant in the chat, not an outside narrator.
Ignore any routing prefixes like !dev.command or @dev.name; they are just delivery hints.

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
impl Plugin for AiTool {
    fn id(&self) -> &'static str {
        "ai"
    }
    fn help(&self) -> &'static str {
        "Ask the AI: !ai <prompt>"
    }
    fn wants_own_messages(&self) -> bool {
        true
    }

    async fn on_room_message(
        &self,
        ctx: &PluginContext,
        event: &OriginalSyncRoomMessageEvent,
        spec: &PluginSpec,
        meta: &RoomMessageMeta<'_>,
    ) -> Result<()> {
        trigger_backfill(ctx, spec);

        let Some(body) = message_body(&event.content.msgtype) else {
            return Ok(());
        };

        record_history(ctx, event, body).await;

        if meta.triggered_plugins.contains(self.id()) {
            return Ok(());
        }

        let Some(own_id) = ctx.client.user_id() else {
            return Ok(());
        };
        if event.sender == own_id {
            return Ok(());
        }

        if body.trim().is_empty() {
            return Ok(());
        }

        let body_lc = body.to_lowercase();
        for handle in fallback_handles(ctx, spec) {
            if body_lc.contains(&handle) {
                info!(plugin = %self.id(), handle, "Fallback mention matched; delegating to run()");
                if let Err(err) = self.run(ctx, body, spec).await {
                    warn!(error = %err, plugin = %self.id(), "AI fallback run failed");
                }
                break;
            }
        }

        Ok(())
    }
    async fn run(&self, ctx: &PluginContext, args: &str, spec: &PluginSpec) -> Result<()> {
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

        let api_base = str_config(spec, "api_base")
            .or_else(|| std::env::var("AI_API_BASE").ok())
            .unwrap_or_else(|| "https://api.openai.com".to_owned());
        let api_path = str_config(spec, "api_path")
            .or_else(|| std::env::var("AI_API_PATH").ok())
            .unwrap_or_else(|| "/v1/chat/completions".to_owned());
        let model = str_config(spec, "model")
            .or_else(|| std::env::var("AI_MODEL").ok())
            .unwrap_or_else(|| "gpt-4o-mini".to_owned());
        // Resolve API key with precedence:
        // 1) config.api_key
        // 2) config.api_key_env -> read that env var
        // 3) env.AI_API_KEY
        // 4) env.OPENAI_API_KEY
        let mut key_source = String::new();
        let api_key = if let Some(k) = str_config(spec, "api_key") {
            key_source = "config.api_key".into();
            Some(k)
        } else if let Some(env_name) = str_config(spec, "api_key_env") {
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

        let name = ai_name(spec);

        let system_prompt_base = spec
            .config
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SYSTEM_PROMPT)
            .to_owned();

        // Build system prompt with the chat context injected; clarify routing flags
        let mut system_prompt = format!(
            "Your name is {name}. People will tag you as @{name}.
Routing prefixes like !dev.command or @dev.name are delivery hints; ignore them when referring to yourself or others.
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
        let sys_preview = truncate(&system_prompt, 200);
        let user_preview = truncate(prompt, 120);
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
                        let prefix = if ctx.dev_active {
                            if let Some(dev_id) = ctx.dev_id.as_deref() {
                                format!("@{dev_id}.{name}:")
                            } else {
                                format!("@{name}:")
                            }
                        } else {
                            format!("@{name}:")
                        };
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

fn ai_env_handle() -> Option<String> {
    std::env::var("AI_HANDLE").ok().map(|raw| {
        if raw.starts_with('@') {
            raw
        } else {
            format!("@{raw}")
        }
    })
}

fn ai_name(spec: &PluginSpec) -> String {
    spec.config
        .get("name")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("AI_NAME").ok())
        .unwrap_or_else(|| "Claire".to_owned())
}

fn message_body(msgtype: &MessageType) -> Option<&str> {
    match msgtype {
        MessageType::Text(inner) => Some(inner.body.as_str()),
        MessageType::Notice(inner) => Some(inner.body.as_str()),
        MessageType::Emote(inner) => Some(inner.body.as_str()),
        _ => None,
    }
}

async fn record_history(ctx: &PluginContext, event: &OriginalSyncRoomMessageEvent, body: &str) {
    let sanitized = sanitize_line(body, 400);
    if sanitized.is_empty() {
        return;
    }

    let sender_name = match ctx.room.get_member(&event.sender).await {
        Ok(Some(member)) => member
            .display_name()
            .map_or_else(|| event.sender.localpart().to_owned(), ToOwned::to_owned),
        _ => event.sender.localpart().to_owned(),
    };
    let timestamp = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
    let line = format!("[{timestamp}] {sender_name}:{sanitized}");
    let room_id = ctx.room.room_id().to_owned();
    append_history_line(ctx.history_dir.as_ref().as_path(), &room_id, &line);
}

fn fallback_handles(ctx: &PluginContext, spec: &PluginSpec) -> Vec<String> {
    let mut handles: Vec<String> = Vec::new();
    if let Some(handle) = ai_env_handle() {
        handles.push(handle.to_lowercase());
    }

    let name = ai_name(spec).to_lowercase();
    if ctx.dev_active {
        if let Some(dev_id) = ctx.dev_id.as_deref() {
            handles.push(format!("@{}.{}", dev_id.to_lowercase(), name));
        }
    } else {
        handles.push(format!("@{}", name));
    }

    handles.sort();
    handles.dedup();
    handles
}

fn trigger_backfill(ctx: &PluginContext, spec: &PluginSpec) {
    let enable = spec
        .config
        .get("history_backfill_on_start")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if !enable {
        return;
    }
    let limit = spec
        .config
        .get("history_backfill_lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(50);
    let client = ctx.client.clone();
    let history_dir = ctx.history_dir.as_ref().clone();
    HISTORY_BACKFILL_ONCE.call_once(|| {
        tokio::spawn(async move {
            backfill_all(client, history_dir, limit).await;
        });
    });
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
        MessageType::Audio(_)
        | MessageType::File(_)
        | MessageType::Image(_)
        | MessageType::Location(_)
        | MessageType::ServerNotice(_)
        | MessageType::Video(_)
        | MessageType::VerificationRequest(_)
        | _ => None,
    }?;

    let sanitized = sanitize_line(body, 400);
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
            .map_or_else(|| user_id.localpart().to_owned(), ToOwned::to_owned),
        _ => user_id.localpart().to_owned(),
    };
    cache.insert(user_id.clone(), display.clone());
    display
}

fn format_timestamp(ts: Option<MilliSecondsSinceUnixEpoch>) -> String {
    if let Some(ts) = ts
        && let Some(formatted) = timestamp_to_rfc3339(ts)
    {
        return formatted;
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

pub async fn backfill_all(client: Client, history_dir: PathBuf, limit: u64) {
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
            options.from.clone_from(&from_token);
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
