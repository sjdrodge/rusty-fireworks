use chrono::DateTime;
use chrono::Utc;
use serde::Serialize;

mod auth;
mod session;

pub use auth::Auth;
pub use session::Hanabi;

#[derive(Serialize)]
#[serde(transparent)]
struct Timestamp(DateTime<Utc>);

impl From<DateTime<Utc>> for Timestamp {
    fn from(datetime: DateTime<Utc>) -> Self {
        Timestamp(datetime)
    }
}

impl Timestamp {
    fn now() -> Self {
        Utc::now().into()
    }
}
