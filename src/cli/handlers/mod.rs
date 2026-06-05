mod community;
mod group;
mod recommend;
mod trash;

pub(super) use community::handle_community;
pub(super) use group::handle_group_command;
pub(super) use recommend::handle_recommend;
pub(super) use trash::handle_trash_command;
