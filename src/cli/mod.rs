mod command_enums;
mod dispatch;
mod handlers;
mod helpers;

pub use command_enums::{
    AdminCommands, Cli, Commands, GroupCommands, RecommendCommands, TrashCommands,
};
pub use dispatch::run;
