mod command_enums;
mod dispatch;
mod handlers;
mod helpers;

pub use command_enums::{Cli, Commands, GroupCommands, RecommendCommands, TrashCommands};
pub use dispatch::run;
