use std::fmt;

use anyhow::Context as _;
use async_trait::async_trait;
use cookie::Cookie;
use hmac::{Hmac, NewMac};
use http::header::COOKIE;
use jwt::VerifyWithKey;
use log::error;
use log::info;
use sha2::Sha256;
use tokio_tungstenite::tungstenite::handshake::server::Request;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::session::Handler;
use crate::session::Session;
use crate::session::State;

use super::Auth;

#[derive(Copy, Clone, fmt::Debug, PartialOrd, Ord, PartialEq, Eq, Hash)]
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

#[derive(fmt::Debug)]
pub struct Hanabi {
    id: UserId,
}

impl Hanabi {
    pub fn get_id(&self) -> UserId {
        self.id
    }
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
    async fn handle_open(&mut self) {
        info!("Welcome! {:?}", self.state.id);
    }

    async fn handle_close(&mut self) {
        info!("Later! {:?}", self.state.id);
    }

    async fn handle_message(&mut self, msg: Message) {
        info!("Got message {:?}", msg);
    }
}
