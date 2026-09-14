//! Control of Lian Li wireless fans through the Lian Li RF dongle.
//!
//! The dongle is a pair of USB devices, a transmitter and a receiver, that
//! carry 64-byte frames to and from the fan groups bound to them. This
//! crate builds those frames, parses the replies, keeps the groups fed with
//! the heartbeat the firmware expects, and exposes the whole loop through a
//! C interface so that a fan control host can drive it.

#![deny(missing_docs)]

pub mod device;
pub mod discovery;
pub mod dongle;
pub mod enumerate;
pub mod frame;
pub mod groups;
pub mod heartbeat;
pub mod process;
pub mod speed;
pub mod win;
