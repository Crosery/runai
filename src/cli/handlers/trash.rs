use crate::cli::command_enums::TrashCommands;
use crate::cli::helpers::find_trash_id_by_query;
use crate::core::manager::SkillManager;
use anyhow::Result;

pub(in crate::cli) fn handle_trash_command(
    mgr: &SkillManager,
    command: TrashCommands,
) -> Result<()> {
    match command {
        TrashCommands::List => {
            use crate::core::resource::format_time_ago;

            let entries = mgr.list_trash()?;
            if entries.is_empty() {
                println!("Trash is empty.");
            } else {
                for entry in &entries {
                    let deleted = format_time_ago(Some(entry.deleted_at));
                    println!(
                        "  [{}] {} — {} ({})",
                        entry.kind.as_str(),
                        entry.id,
                        entry.name,
                        deleted
                    );
                }
                println!("\nTotal: {} trashed resources", entries.len());
            }
            Ok(())
        }
        TrashCommands::Restore { query } => {
            let trash_id = find_trash_id_by_query(mgr, &query)?;
            mgr.restore_from_trash(&trash_id)?;
            println!("Restored '{query}'");
            Ok(())
        }
        TrashCommands::Purge { query } => {
            let trash_id = find_trash_id_by_query(mgr, &query)?;
            mgr.purge_trash(&trash_id)?;
            println!("Permanently deleted '{query}'");
            Ok(())
        }
        TrashCommands::Empty => {
            let count = mgr.empty_trash()?;
            println!("Emptied trash ({count} items)");
            Ok(())
        }
    }
}
