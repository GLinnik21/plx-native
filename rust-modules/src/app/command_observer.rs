//! Passive command observations. No observer decides admission or executes a command.
use super::*;
use plx_machine::machine::AppCommandOutcome;

pub(super) struct Observation {
    #[cfg(test)]
    record: Record,
}

#[cfg(test)]
enum Record {
    None,
    Label(String),
    Library(MachineId, LibraryReq, ReturnState<u32, PageMemory>),
    Home(MachineId, HomeReq, ReturnState<u32, PageMemory>),
}

impl Observation {
    pub(super) fn begin(bridge: &mut Bridge, command: &plx_screens::registry::AppCommand) -> Self {
        #[cfg(not(test))]
        { let _ = (bridge, command); Self {} }
        #[cfg(test)]
        {
            let record = match &command.effect {
                Ok(ExternalAppFx::Loop(request)) => {
                    bridge.retained.observed_loop_commands.push(*request); Record::None
                }
                Ok(ExternalAppFx::Player(request)) => {
                    bridge.retained.observed_player_commands.push(request.clone()); Record::None
                }
                Ok(ExternalAppFx::ItemMenu(request)) => {
                    bridge.retained.observed_item_menu_commands.push(plx_screens::registry::ItemMenuReq {
                        act: request.act.clone(), sid: request.sid, item: request.item.clone(),
                        season: request.season.clone(), loaded_episode: request.loaded_episode, from_home: request.from_home,
                    }); Record::None
                }
                Ok(ExternalAppFx::Watchlist(request)) => Record::Label(format!("watchlist:{request:?}")),
                Ok(ExternalAppFx::Search(request)) => Record::Label(format!("search:{request:?}")),
                Ok(ExternalAppFx::Content(request)) => Record::Label(super::super::content::content_command_label(request).into()),
                Ok(ExternalAppFx::Library(request)) => Record::Library(command.from, request.clone(), command.return_state.clone()),
                Ok(ExternalAppFx::Home(request)) => Record::Home(command.from, request.clone(), command.return_state.clone()),
                Ok(ExternalAppFx::SessionEffect(_) | ExternalAppFx::Preferences(_) | ExternalAppFx::Catalog(_)) | Err(_) => Record::None,
            };
            Self { record }
        }
    }

    pub(super) fn finish(self, bridge: &mut Bridge, outcome: AppCommandOutcome) {
        #[cfg(not(test))]
        { let _ = (self, bridge, outcome); }
        #[cfg(test)]
        if outcome == AppCommandOutcome::Completed {
            match self.record {
                Record::None => {}
                Record::Label(label) => bridge.retained.completed_screen_commands.push(label),
                Record::Library(from, request, ret) => {
                    if !matches!(request, LibraryReq::PublishShelves { .. }) {
                        bridge.retained.completed_screen_commands.push(format!("library:{request:?}"));
                    }
                    bridge.retained.observed_library_commands.push((from, request, ret));
                }
                Record::Home(from, request, ret) => {
                    bridge.retained.completed_screen_commands.push(if matches!(request, HomeReq::Detail { .. }) {
                        "home:Detail".into()
                    } else { format!("home:{request:?}") });
                    bridge.retained.observed_home_commands.push((from, request, ret));
                }
            }
        }
    }
}
