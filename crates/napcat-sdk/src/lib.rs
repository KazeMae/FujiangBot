//! NapCat forward-WebSocket client.
//!
//! Layout mirrors [node-napcat-ts](https://github.com/HkTeamX/node-napcat-ts):
//! client / events / structs / typed API. Target protocol: NapCat 4.18.19.

pub mod api;
pub mod client;
pub mod cq;
pub mod error;
pub mod events;
pub mod structs;
pub mod types;

pub use client::{ClientConfig, NapcatClient};
pub use error::{Error, Result};
pub use events::{Event, EventKind, Sender};
pub use structs::{RecvSegment, SendSegment, Structs};
pub use types::{
    FileResult, LoginInfo, SendGroupMsg, SendPrivateMsg, SendResult, SetFriendAddRequest,
    SetGroupAddRequest,
};
