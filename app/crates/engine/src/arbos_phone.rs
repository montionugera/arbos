//! The phone client, re-exported so the engine test and the app share one path.
//!
//! The frames are the desktop RPC. This module does not add a second protocol.

pub use zeron_phone::{PhoneProject, connect, follow_main, list_projects, send_prompt};
