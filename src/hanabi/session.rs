use std::fmt;
use std::fmt::Write;

use anyhow::Context as _;
use async_trait::async_trait;
use cookie::Cookie;
use futures::SinkExt;
use futures::TryFutureExt;
use hmac::{Hmac, NewMac};
use http::header::COOKIE;
use jwt::VerifyWithKey;
use log::error;
use log::info;
use serde::Serialize;
use sha2::Sha256;
use tokio_tungstenite::tungstenite::handshake::server::Request;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::session::Handler;
use crate::session::Session;
use crate::session::State;

use super::Auth;

use super::Timestamp;

#[derive(Copy, Clone, fmt::Debug, PartialOrd, Ord, PartialEq, Eq, Hash, Serialize)]
pub struct UserId(u64);

impl UserId {
    fn new(id: u64) -> Self {
        UserId(id)
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "User:{}", self.0)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    #[error("missing session secret")]
    NoSecret,
    #[error("missing session cookie")]
    NoCookie,
    #[error("bad session secret")]
    BadSecret,
    #[error("bad session cookie")]
    BadCookie,
}

#[derive(Default, Serialize)]
struct UserSettings {}

#[derive(Serialize)]
struct UserMsg<S: Serialize> {
    #[serde(rename = "userID")]
    user_id: UserId,
    username: String,
    #[serde(flatten)]
    data: S,
}

#[derive(fmt::Debug)]
pub struct Hanabi {
    id: UserId,
}

impl Hanabi {
    pub fn get_id(&self) -> UserId {
        self.id
    }
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct WelcomeMsg {
    total_games: u32,
    muted: bool,
    first_time_user: bool,
    settings: UserSettings,
    friends: Vec<String>,
    at_ongoing_table: bool,
    random_table_name: String,
    shutting_down: bool,
    datetime_shutdown_init: Option<Timestamp>,
    maintenance_mode: bool,
}

#[derive(Serialize)]
struct ChatRoomFields {
    discord: bool,
    room: String,
}

#[derive(Clone, Serialize)]
#[serde(into = "ChatRoomFields")]
enum ChatRoom {
    Lobby,
    Table(String),
}

impl From<ChatRoom> for ChatRoomFields {
    fn from(kind: ChatRoom) -> Self {
        match kind {
            ChatRoom::Lobby => ChatRoomFields {
                discord: false,
                room: "lobby".into(),
            },
            ChatRoom::Table(table) => ChatRoomFields {
                discord: false,
                room: table,
            },
        }
    }
}

#[derive(Serialize)]
struct ChatKindFields {
    server: bool,
    who: String,
    recipient: String,
}

#[derive(Clone, Serialize)]
#[serde(into = "ChatKindFields")]
enum ChatKind {
    Server,
    Direct { from: String, to: String },
}

impl From<ChatKind> for ChatKindFields {
    fn from(kind: ChatKind) -> Self {
        match kind {
            ChatKind::Server => ChatKindFields {
                server: true,
                who: "".into(),
                recipient: "".into(),
            },
            ChatKind::Direct { from, to } => ChatKindFields {
                server: false,
                who: from,
                recipient: to,
            },
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatMsg {
    msg: String,
    datetime: Timestamp,
    #[serde(flatten)]
    room: ChatRoom,
    #[serde(flatten)]
    kind: ChatKind,
}

impl State for Hanabi {
    type Key = UserId;
    type Message = Message;

    fn from_request(req: &Request) -> anyhow::Result<Self> {
        let headers = req.headers();

        let secret = headers
            .get_all("x-session-secret")
            .iter()
            .last()
            .context(RequestError::NoSecret)?
            .as_bytes();

        let token = headers
            .get_all(COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|v| Cookie::parse(v).ok())
            .filter(|c| c.name() == "hanabi.sid")
            .last()
            .context(RequestError::NoCookie)?;

        let key: Hmac<Sha256> = Hmac::new_varkey(secret).or(Err(RequestError::BadSecret))?;
        let auth: Auth = token
            .value()
            .verify_with_key(&key)
            .context(RequestError::BadCookie)?;

        let state = Hanabi {
            id: UserId::new(auth.id),
        };

        Ok(state)
    }

    fn get_key(&self) -> Self::Key {
        self.id
    }
}

#[async_trait]
impl Handler for Session<Hanabi> {
    async fn handle_open(&mut self) -> anyhow::Result<()> {
        info!("Welcome! {:?}", self.state.id);

        self.emit_for_user(
            "welcome",
            WelcomeMsg {
                first_time_user: true,
                random_table_name: "correct horse battery".into(),
                ..Default::default()
            },
        )
        .await?;

        self.emit(
            "chat",
            ChatMsg {
                msg: "this is a test of the hanabi broadcast system".into(),
                datetime: Timestamp::now(),
                room: ChatRoom::Lobby,
                kind: ChatKind::Server,
            },
        )
        .await?;

        self.emit("userList", Vec::<String>::new()).await?;
        self.emit("tableList", Vec::<String>::new()).await?;
        self.emit("gameHistory", Vec::<String>::new()).await?;
        self.emit("gameHistoryFriends", Vec::<String>::new())
            .await?;

        Ok(())
    }

    async fn handle_close(&mut self) {
        info!("Later! {:?}", self.state.id);
    }

    async fn handle_message(&mut self, msg: Message) {
        info!("Got message {:?}", msg);
    }
}

impl Session<Hanabi> {
    async fn emit<S: Serialize>(&mut self, name: &str, data: S) -> anyhow::Result<()> {
        let mut s = String::new();
        write!(&mut s, "{} {}", name, serde_json::to_string(&data).unwrap())?;
        info!("Wrote message {}", s);
        self.send(Message::Text(s)).err_into().await
    }

    async fn emit_for_user<S: Serialize>(&mut self, name: &str, data: S) -> anyhow::Result<()> {
        let mut username = String::new();
        write!(&mut username, "{}", self.state.id)?;
        let msg = UserMsg {
            user_id: self.state.id,
            username,
            data,
        };

        self.emit(name, msg).await
    }
}
