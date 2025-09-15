use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};
use std::io::IsTerminal;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use futures_util::StreamExt;
use matrix_sdk::{
    config::SyncSettings,
    matrix_auth::{MatrixSession, MatrixSessionTokens},
    ruma::{self, events::room::member::MembershipState},
    Client, SessionMeta,
};
use matrix_sdk::room::Room;
use ruma::events::room::member::StrippedRoomMemberEvent;
use ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent, ImageMessageEventContent, FileMessageEventContent, AudioMessageEventContent, VideoMessageEventContent};
use ruma::{OwnedRoomId, RoomId, RoomAliasId};
use ruma::events::key::verification::{
    request::ToDeviceKeyVerificationRequestEvent,
    start::ToDeviceKeyVerificationStartEvent,
};
use matrix_sdk::encryption::verification::{SasState, Verification, VerificationRequestState, VerificationRequest, SasVerification};
// use tokio::time::{timeout, Duration};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use serde_yaml;
// media helpers used via client.media()
use matrix_sdk::attachment::AttachmentConfig;
use mime::Mime;

mod commands;

#[derive(Parser, Debug)]
#[command(name = "matrix-ping-bot", version, about = "Simple Matrix ping bot with E2EE")]
struct Args {
    /// Homeserver base URL, e.g. https://matrix-client.matrix.org
    #[arg(long, env = "MATRIX_HOMESERVER")]
    homeserver: String,

    /// Username (localpart or full user ID)
    #[arg(long, env = "MATRIX_USERNAME")]
    username: String,

    /// Password (if omitted, will prompt if needed)
    #[arg(long, env = "MATRIX_PASSWORD")] 
    password: Option<String>,

    /// Directory for persistent state (encryption keys, sync cache)
    #[arg(long, env = "MATRIX_STORE", default_value = "./bot-store")] 
    store: PathBuf,

    /// JSON session file for access token/device info
    #[arg(long, env = "MATRIX_SESSION_FILE", default_value = "./session.json")]
    session_file: PathBuf,

    /// Device display name
    #[arg(long, env = "MATRIX_DEVICE_NAME", default_value = "matrix-ping-bot")] 
    device_name: String,

    /// Path to YAML config describing room clusters to relay between
    #[arg(long, env = "MATRIX_CONFIG", default_value = "./config.yaml")]
    config: PathBuf,

    /// Disable auto-joining rooms when invited
    #[arg(long)]
    no_autojoin: bool,

    /// Auto-accept and confirm SAS verifications (insecure for production)
    #[arg(long, env = "MATRIX_AUTO_VERIFY", default_value_t = true)]
    auto_verify: bool,

    /// Sync timeout in milliseconds
    #[arg(long, env = "MATRIX_SYNC_TIMEOUT_MS", default_value_t = 30000)]
    sync_timeout_ms: u64,

    /// Enable dev-mode behaviors (must also be enabled in config)
    #[arg(short = 'd', long = "dev")]
    dev: bool,

    /// Instance mode override via env/flag: "dev" or "prod"
    #[arg(long, env = "MATRIX_MODE")]
    mode: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedSession {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    user_id: String,
    device_id: String,
}

#[derive(Debug, Deserialize, Clone)]
struct BotConfig {
    clusters: Vec<RoomCluster>,
    #[serde(default)] reupload_media: Option<bool>,
    #[serde(default)] caption_media: Option<bool>,
    #[serde(default)] dev_mode: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
struct RoomCluster {
    name: Option<String>,
    rooms: Vec<String>,
    #[serde(default)] reupload_media: Option<bool>,
    #[serde(default)] caption_media: Option<bool>,
}

#[derive(Debug, Clone, Copy)]
struct RelayOptions { reupload_media: bool, caption_media: bool }

#[derive(Debug, Clone)]
struct RelayPlan {
    map: HashMap<OwnedRoomId, Vec<OwnedRoomId>>,
    opts: HashMap<OwnedRoomId, RelayOptions>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    // Load .env if present so clap can pick up env vars
    let _ = dotenvy::dotenv();
    let args = Args::parse();

    fs::create_dir_all(&args.store).with_context(|| {
        format!("creating store directory at {}", args.store.display())
    })?;

    // Build client with SQLite store to persist E2EE state
    let client = Client::builder()
        .homeserver_url(&args.homeserver)
        .handle_refresh_tokens()
        .sqlite_store(&args.store, None)
        .build()
        .await
        .context("building matrix client")?;

    // Restore session if available; otherwise login
    if let Some(session) = load_session(&args.session_file)? {
        info!("Restoring session for {}", session.user_id);
        let matrix_session = MatrixSession {
            meta: SessionMeta {
                user_id: session.user_id.parse().context("invalid stored user_id")?,
                device_id: session.device_id.into(),
            },
            tokens: MatrixSessionTokens {
                access_token: session.access_token,
                refresh_token: session.refresh_token,
            },
        };
        client.restore_session(matrix_session).await.context("restoring session")?;
    } else {
        // Treat empty env/arg as missing; avoid prompting in non-interactive (Docker) mode.
        let password = match args.password.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(p) => p.to_owned(),
            None => {
                if !std::io::stdin().is_terminal() {
                    return Err(anyhow!(
                        "No MATRIX_PASSWORD provided and no stored session. In Docker/non-interactive mode, set MATRIX_PASSWORD env or mount an existing session at {}",
                        args.session_file.display()
                    ));
                }
                warn!("No password provided via --password or MATRIX_PASSWORD. Prompting...");
                rpassword::prompt_password("Matrix password: ")?
            }
        };

        info!("Logging in as {}", args.username);
        let response = client
            .matrix_auth()
            .login_username(&args.username, &password)
            .initial_device_display_name(&args.device_name)
            .request_refresh_token()
            .send()
            .await
            .context("login failed")?;

        // Save session for future runs
        let session = SavedSession {
            access_token: response.access_token.clone(),
            refresh_token: response.refresh_token.clone(),
            user_id: response.user_id.to_string(),
            device_id: response.device_id.to_string(),
        };
        save_session(&args.session_file, &session)?;
        info!(
            "Logged in: user={} device={}",
            session.user_id, session.device_id
        );
    }

    // Load relay configuration and resolve room IDs
    let config = load_config(&args.config)?;
    let env_dev = matches!(args.mode.as_deref(), Some(m) if m.eq_ignore_ascii_case("dev"));
    let dev_active = (args.dev || env_dev) && config.dev_mode.unwrap_or(false);
    let relay = Arc::new(resolve_relay_map(&client, &config).await?);
    // Build command registry
    let commands = Arc::new(commands::default_registry());

    // Auto-join handler for invites
    if !args.no_autojoin {
        let client_for_handler = client.clone();
        client.add_event_handler(move |ev: StrippedRoomMemberEvent, room: Room| {
            let client = client_for_handler.clone();
            async move {
                if ev.content.membership != MembershipState::Invite {
                    return;
                }
                let Some(own_id) = client.user_id() else { return; };
                if ev.state_key != own_id.as_str() {
                    return;
                }
                info!(room_id = %room.room_id(), "Auto-joining invited room");
                if let Err(e) = room.join().await {
                    warn!(error = %e, "Failed to accept invite");
                }
            }
        });
    }

    // Message handler: relay between configured room clusters, and keep existing commands
    let client_for_handler = client.clone();
    let relay_handler = relay.clone();
    let commands_handler = commands.clone();
    let dev_active_handler = dev_active;
    client.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, room: Room| {
        let client = client_for_handler.clone();
        let relay = relay_handler.clone();
        let commands = commands_handler.clone();
        let dev_active = dev_active_handler;
        async move {
            // Ignore own messages
            let Some(own_id) = client.user_id() else { return; };
            if ev.sender == own_id { return; }

            // Log incoming message details for diagnostics
            let msg_kind = match &ev.content.msgtype {
                MessageType::Text(_) => "text",
                MessageType::Notice(_) => "notice",
                MessageType::Emote(_) => "emote",
                MessageType::Image(_) => "image",
                MessageType::File(_) => "file",
                MessageType::Audio(_) => "audio",
                MessageType::Video(_) => "video",
                _ => "other",
            };
            let body_snippet: Option<String> = match &ev.content.msgtype {
                MessageType::Text(t) => Some(truncate(&t.body, 200)),
                MessageType::Notice(n) => Some(truncate(&n.body, 200)),
                MessageType::Emote(e) => Some(truncate(&e.body, 200)),
                _ => None,
            };
            info!(room_id = %room.room_id(), sender = %ev.sender, kind = %msg_kind, body = ?body_snippet, "Incoming message");

            // Plain text/notice messages; parse command if the body starts with '!'
            let body_opt = match &ev.content.msgtype {
                MessageType::Text(t) => Some(t.body.as_str()),
                MessageType::Notice(n) => Some(n.body.as_str()),
                _ => None,
            };
            if let Some(body) = body_opt.map(|b| b.trim()) {
                if body.starts_with('!') {
                    let mut parts = body.splitn(2, ' ');
                    let cmd = parts.next().unwrap_or("");
                    let args_raw = parts.next().unwrap_or("").trim();
                    if let Some(handler) = commands.get(cmd) {
                        let (args_clean, arg_dev_flag) = extract_dev_flag(args_raw);
                        // Enforce env selection via -d flag:
                        // - prod (dev_active=false) only handles commands WITHOUT -d
                        // - dev  (dev_active=true)  only handles commands WITH -d
                        if arg_dev_flag != dev_active {
                            info!(command = %cmd, dev_flag = arg_dev_flag, dev_active = dev_active, "Ignoring command due to env mismatch");
                        } else if handler.dev_only() && !dev_active {
                            info!(command = %cmd, "Ignoring dev-only command in prod mode");
                        } else {
                            let ctx = crate::commands::CommandContext {
                                client: client.clone(),
                                room: room.clone(),
                                sender: ev.sender.to_string(),
                                commands: commands.clone(),
                                dev_active,
                            };
                            if let Err(e) = handler.run(&ctx, &args_clean).await {
                                warn!(error = %e, command = %cmd, "Command failed");
                            }
                        }
                    }
                }
            }

            // Relay to rooms in the same cluster.
            // - For text/notice/emote: send as plain text "DisplayName: message".
            // - For other types: forward original content unchanged.
            let source_id = room.room_id().to_owned();
            if let Some(targets) = relay.map.get(&source_id).cloned() {
                let opts = relay.opts.get(&source_id).copied().unwrap_or(RelayOptions { reupload_media: true, caption_media: true });
                // Resolve sender display name in the source room
                let display_name = match room.get_member(&ev.sender).await {
                    Ok(Some(m)) => m
                        .display_name()
                        .map(|s| s.to_owned())
                        .unwrap_or_else(|| ev.sender.localpart().to_string()),
                    _ => ev.sender.localpart().to_string(),
                };
                let display_name_bold = to_bold(&display_name);

                let mut formatted_text: Option<String> = None;
                match &ev.content.msgtype {
                    MessageType::Text(t) => {
                        let (quoted, main) = split_reply_fallback(&t.body);
                        let mut out = String::new();
                        if let Some(q) = quoted { out.push_str(&format!("↪ {}\n", truncate(&q, 300))); }
                        out.push_str(&format!("{}: {}", display_name_bold, main.trim()));
                        formatted_text = Some(out);
                    }
                    MessageType::Notice(n) => {
                        let (quoted, main) = split_reply_fallback(&n.body);
                        let mut out = String::new();
                        if let Some(q) = quoted { out.push_str(&format!("↪ {}\n", truncate(&q, 300))); }
                        out.push_str(&format!("{}: {}", display_name_bold, main.trim()));
                        formatted_text = Some(out);
                    }
                    MessageType::Emote(e) => {
                        let (quoted, main) = split_reply_fallback(&e.body);
                        let mut out = String::new();
                        if let Some(q) = quoted { out.push_str(&format!("↪ {}\n", truncate(&q, 300))); }
                        out.push_str(&format!("{}: * {}", display_name_bold, main.trim()));
                        formatted_text = Some(out);
                    }
                    _ => {}
                }

                for target_id in targets {
                    if target_id == source_id { continue; }
                    if let Some(room_handle) = client.get_room(&target_id) {
                        let send_res = if let Some(text) = &formatted_text {
                            let content = RoomMessageEventContent::text_plain(text.clone());
                            room_handle.send(content).await
                        } else {
                            // Try download -> reupload -> send for common media types
                            match &ev.content.msgtype {
                                MessageType::Image(img) => {
                                    if opts.reupload_media {
                                        match reupload_image(&client, img).await {
                                            Ok((body, mime, data)) => send_attachment(&room_handle, &body, &mime, data).await,
                                            Err(e) => { warn!(error = %e, "Image reupload failed; forwarding original event"); room_handle.send(ev.content.clone()).await },
                                        }
                                    } else { room_handle.send(ev.content.clone()).await }
                                }
                                MessageType::File(file) => {
                                    if opts.reupload_media {
                                        match reupload_file(&client, file).await {
                                            Ok((body, mime, data)) => send_attachment(&room_handle, &body, &mime, data).await,
                                            Err(e) => { warn!(error = %e, "File reupload failed; forwarding original event"); room_handle.send(ev.content.clone()).await },
                                        }
                                    } else { room_handle.send(ev.content.clone()).await }
                                }
                                MessageType::Audio(audio) => {
                                    if opts.reupload_media {
                                        match reupload_audio(&client, audio).await {
                                            Ok((body, mime, data)) => send_attachment(&room_handle, &body, &mime, data).await,
                                            Err(e) => { warn!(error = %e, "Audio reupload failed; forwarding original event"); room_handle.send(ev.content.clone()).await },
                                        }
                                    } else { room_handle.send(ev.content.clone()).await }
                                }
                                MessageType::Video(video) => {
                                    if opts.reupload_media {
                                        match reupload_video(&client, video).await {
                                            Ok((body, mime, data)) => send_attachment(&room_handle, &body, &mime, data).await,
                                            Err(e) => { warn!(error = %e, "Video reupload failed; forwarding original event"); room_handle.send(ev.content.clone()).await },
                                        }
                                    } else { room_handle.send(ev.content.clone()).await }
                                }
                                _ => room_handle.send(ev.content.clone()).await,
                            }
                        };
                        match send_res {
                            Ok(_) => {
                                info!(from = %source_id, to = %target_id, sender = %ev.sender, "Relayed message");
                                if formatted_text.is_none() && opts.caption_media {
                                    let kind = match &ev.content.msgtype { MessageType::Image(_) => "image", MessageType::File(_) => "file", MessageType::Audio(_) => "audio", MessageType::Video(_) => "video", _ => "" };
                                    if !kind.is_empty() {
                                    let caption = format!("{}: sent a {}", display_name_bold, kind);
                                        let _ = room_handle.send(RoomMessageEventContent::text_plain(caption)).await;
                                    }
                                }
                            }
                            Err(e) => warn!(error = %e, from = %source_id, to = %target_id, "Failed to relay message"),
                        }
                    } else {
                        warn!(from = %source_id, to = %target_id, "No handle for target room; skipping relay");
                    }
                }
            } else {
                info!(room_id = %source_id, "No relay mapping for this room; not forwarding");
            }
        }
    });

    // Emoji SAS verification handlers (print emojis to console). If auto_verify is true,
    // we will auto-confirm once emojis are shown.
    let auto_confirm = args.auto_verify;
    client.add_event_handler(move |ev: ToDeviceKeyVerificationRequestEvent, client: Client| {
        let client2 = client.clone();
        async move {
            info!(user = %ev.sender, flow = %ev.content.transaction_id, "Received verification request");
            if let Some(req) = client.encryption().get_verification_request(&ev.sender, &ev.content.transaction_id).await {
                tokio::spawn(handle_verification_request(client2, req, auto_confirm));
            } else {
                warn!(user = %ev.sender, flow = %ev.content.transaction_id, "No verification request found");
            }
        }
    });

    client.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, client: Client| {
        let client2 = client.clone();
        async move {
            if let MessageType::VerificationRequest(_) = &ev.content.msgtype {
                info!(user = %ev.sender, event = %ev.event_id, "Received in-room verification request");
                if let Some(req) = client.encryption().get_verification_request(&ev.sender, &ev.event_id).await {
                    tokio::spawn(handle_verification_request(client2, req, auto_confirm));
                }
            }
        }
    });

    client.add_event_handler(move |ev: ToDeviceKeyVerificationStartEvent, client: Client| {
        let client2 = client.clone();
        async move {
            info!(user = %ev.sender, flow = %ev.content.transaction_id, "Received verification start");
            if let Some(verification) = client.encryption().get_verification(&ev.sender, ev.content.transaction_id.as_str()).await {
                if let Verification::SasV1(sas) = verification {
                    tokio::spawn(handle_sas(client2, sas, auto_confirm));
                }
            }
        }
    });
    // End emoji SAS handlers


    // Start syncing with configured timeout
    info!(timeout_ms = args.sync_timeout_ms, "Starting sync… Press Ctrl+C to stop.");
    let settings = SyncSettings::new().timeout(std::time::Duration::from_millis(args.sync_timeout_ms));
    client
        .sync(settings)
        .await
        .map_err(|e| anyhow!("sync terminated: {e}"))
}

fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info,matrix_sdk=info".to_owned());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn load_config(path: &PathBuf) -> Result<BotConfig> {
    if !path.exists() {
        return Err(anyhow!(
            "config file not found at {}. Create one or set --config",
            path.display()
        ));
    }
    let yaml = fs::read_to_string(path)
        .with_context(|| format!("reading config file at {}", path.display()))?;
    let cfg: BotConfig = serde_yaml::from_str(&yaml).context("parsing YAML config")?;
    Ok(cfg)
}

async fn resolve_relay_map(client: &Client, cfg: &BotConfig) -> Result<RelayPlan> {
    let mut map: HashMap<OwnedRoomId, Vec<OwnedRoomId>> = HashMap::new();
    let mut opts: HashMap<OwnedRoomId, RelayOptions> = HashMap::new();

    for cluster in &cfg.clusters {
        // Resolve each string to a room ID. Support either !room:server IDs or #alias:server
        let mut resolved: Vec<OwnedRoomId> = Vec::new();
        for room_ref in &cluster.rooms {
            if let Ok(id) = RoomId::parse(room_ref) {
                resolved.push(id.to_owned());
                continue;
            }
            if room_ref.starts_with('#') {
                match RoomAliasId::parse(room_ref) {
                    Ok(alias) => match client.resolve_room_alias(&alias).await {
                        Ok(resp) => resolved.push(resp.room_id.to_owned()),
                        Err(e) => {
                            warn!(alias = %room_ref, error = %e, "Failed to resolve room alias; skipping");
                            continue;
                        }
                    },
                    Err(_) => {
                        warn!(alias = %room_ref, "Invalid room alias; skipping");
                        continue;
                    }
                }
            } else {
                warn!(room = %room_ref, "Invalid room reference (expect !room_id or #alias); skipping");
            }
        }

        // Resolve options with precedence: cluster overrides -> cfg defaults -> hard defaults
        let reupload = cluster.reupload_media.or(cfg.reupload_media).unwrap_or(true);
        let caption = cluster.caption_media.or(cfg.caption_media).unwrap_or(true);

        // For each room in the cluster, set its peers and options
        for r in &resolved {
            let peers: Vec<OwnedRoomId> = resolved.iter().filter(|x| *x != r).cloned().collect();
            map.entry(r.clone())
                .and_modify(|v| {
                    // merge peers (dedup naive)
                    for p in &peers { if !v.contains(p) { v.push(p.clone()); } }
                })
                .or_insert(peers);
            opts.insert(r.clone(), RelayOptions { reupload_media: reupload, caption_media: caption });
        }
    }

    info!(clusters = cfg.clusters.len(), rooms = map.len(), "Loaded relay mapping");
    for (from, peers) in &map {
        let peer_list = peers.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ");
        info!(from = %from, peers = %peer_list, "Relay mapping entry");
    }
    Ok(RelayPlan { map, opts })
}

async fn handle_verification_request(client: Client, request: VerificationRequest, auto_confirm: bool) {
    info!(user = %request.other_user_id(), "Accepting verification request");
    if let Err(e) = request.accept().await {
        warn!(error = %e, "Failed to accept verification request");
        return;
    }
    let mut stream = request.changes();
    while let Some(state) = stream.next().await {
        match state {
            VerificationRequestState::Transitioned { verification } => {
                if let Some(sas) = verification.sas() {
                    tokio::spawn(handle_sas(client.clone(), sas, auto_confirm));
                }
                break;
            }
            VerificationRequestState::Cancelled(info) => {
                warn!(reason = %info.reason(), "Verification cancelled (request stage)");
                break;
            }
            VerificationRequestState::Done => {
                info!("Verification already done at request stage");
                break;
            }
            _ => {}
        }
    }
}

async fn handle_sas(_client: Client, sas: SasVerification, auto_confirm: bool) {
    info!(user = %sas.other_device().user_id(), device = %sas.other_device().device_id(), "Starting SAS verification");
    if let Err(e) = sas.accept().await { warn!(error = %e, "Failed to accept SAS"); return; }

    let mut stream = sas.changes();
    while let Some(state) = stream.next().await {
        match state.clone() {
            SasState::KeysExchanged { emojis, .. } => {
                if let Some(e) = emojis {
                    let emoji_string = e.emojis.iter().map(|em| em.symbol).collect::<Vec<_>>().join(" ");
                    let descriptions = e.emojis.iter().map(|em| em.description).collect::<Vec<_>>().join(" ");
                    println!("SAS emojis: {emoji_string}\nSAS names:  {descriptions}");
                    if auto_confirm {
                        if let Err(e) = sas.confirm().await { warn!(error = %e, "Failed to confirm SAS"); }
                    }
                }
            }
            SasState::Done { .. } => {
                info!("Verification completed");
                break;
            }
            SasState::Cancelled(info) => {
                warn!(reason = %info.reason(), "Verification cancelled (SAS stage)");
                break;
            }
            _ => {}
        }
    }
}

fn load_session(path: &PathBuf) -> Result<Option<SavedSession>> {
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(path)
        .with_context(|| format!("reading session file at {}", path.display()))?;
    let session: SavedSession = serde_json::from_str(&data).context("parsing session JSON")?;
    Ok(Some(session))
}

fn save_session(path: &PathBuf, session: &SavedSession) -> Result<()> {
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    let data = serde_json::to_string_pretty(session)?;
    fs::write(path, data).with_context(|| format!("writing session file at {}", path.display()))?;
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in s.chars().take(max) { out.push(ch); }
    out
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

// Best-effort parser for Matrix fallback reply bodies. Many clients send a quoted
// block (lines starting with "> ") followed by a blank line, then the reply.
// Returns (Some(quote_snippet), main_text) if a quote is detected.
fn split_reply_fallback(body: &str) -> (Option<String>, String) {
    // Find first empty line separator
    if let Some(sep_idx) = body.find("\n\n") {
        let (quoted_block, rest) = body.split_at(sep_idx);
        // rest starts with two newlines
        let main = rest.trim_start_matches('\n').trim_start_matches('\n').to_string();
        // Collect quoted lines without leading "> "
        let mut quoted_lines = Vec::new();
        for line in quoted_block.lines() {
            if let Some(stripped) = line.strip_prefix("> ") {
                quoted_lines.push(stripped.to_string());
            } else if line.starts_with('>') {
                let s = line.trim_start_matches('>').trim_start();
                quoted_lines.push(s.to_string());
            }
        }
        if !quoted_lines.is_empty() {
            let quoted = quoted_lines.join(" ");
            return (Some(quoted.trim().to_string()), main);
        }
    }
    (None, body.to_string())
}

fn parse_mime(opt: Option<&str>) -> Mime {
    opt.and_then(|s| s.parse::<Mime>().ok()).unwrap_or(mime::APPLICATION_OCTET_STREAM)
}

fn extract_dev_flag(args: &str) -> (String, bool) {
    let mut dev = false;
    let mut kept: Vec<&str> = Vec::new();
    for tok in args.split_whitespace() {
        if tok == "-d" || tok == "--dev" { dev = true; } else { kept.push(tok); }
    }
    (kept.join(" "), dev)
}

async fn reupload_image(client: &Client, img: &ImageMessageEventContent) -> Result<(String, Mime, Vec<u8>)> {
    let body = img.body.clone();
    let mime = parse_mime(img.info.as_ref().and_then(|i| i.mimetype.as_deref()));
    let data_opt = client.media().get_file(img.clone(), true).await.context("downloading image")?;
    let data = data_opt.ok_or_else(|| anyhow!("image bytes missing"))?;
    Ok((body, mime, data))
}

async fn reupload_file(client: &Client, file: &FileMessageEventContent) -> Result<(String, Mime, Vec<u8>)> {
    let body = file.body.clone();
    let mime = parse_mime(file.info.as_ref().and_then(|i| i.mimetype.as_deref()));
    let data_opt = client.media().get_file(file.clone(), true).await.context("downloading file")?;
    let data = data_opt.ok_or_else(|| anyhow!("file bytes missing"))?;
    Ok((body, mime, data))
}

async fn reupload_audio(client: &Client, audio: &AudioMessageEventContent) -> Result<(String, Mime, Vec<u8>)> {
    let body = audio.body.clone();
    let mime = parse_mime(audio.info.as_ref().and_then(|i| i.mimetype.as_deref()));
    let data_opt = client.media().get_file(audio.clone(), true).await.context("downloading audio")?;
    let data = data_opt.ok_or_else(|| anyhow!("audio bytes missing"))?;
    Ok((body, mime, data))
}

async fn reupload_video(client: &Client, video: &VideoMessageEventContent) -> Result<(String, Mime, Vec<u8>)> {
    let body = video.body.clone();
    let mime = parse_mime(video.info.as_ref().and_then(|i| i.mimetype.as_deref()));
    let data_opt = client.media().get_file(video.clone(), true).await.context("downloading video")?;
    let data = data_opt.ok_or_else(|| anyhow!("video bytes missing"))?;
    Ok((body, mime, data))
}

async fn send_attachment(room: &Room, body: &str, mime: &Mime, data: Vec<u8>) -> matrix_sdk::Result<ruma::api::client::message::send_message_event::v3::Response> {
    let config = AttachmentConfig::new();
    room.send_attachment(body, &mime.clone(), data, config).await
}
