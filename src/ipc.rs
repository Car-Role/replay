//! Messages from other Replay processes (the library window, `replay --save`)
//! to the running recorder. Each platform delivers them its own way.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecorderMsg {
    /// The settings file changed.
    Reload,
    /// An update was downloaded and verified; offer the restart.
    UpdateStaged,
    /// Install the staged update and restart now.
    InstallUpdate,
    /// Save the game in front (`false`) or all games (`true`).
    Save(bool),
    /// Open the library window (a second start of the recorder).
    Show,
}

impl RecorderMsg {
    pub fn to_line(self) -> &'static str {
        match self {
            RecorderMsg::Reload => "reload",
            RecorderMsg::UpdateStaged => "update-staged",
            RecorderMsg::InstallUpdate => "install-update",
            RecorderMsg::Save(false) => "save",
            RecorderMsg::Save(true) => "save-all",
            RecorderMsg::Show => "show",
        }
    }

    pub fn from_line(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "reload" => RecorderMsg::Reload,
            "update-staged" => RecorderMsg::UpdateStaged,
            "install-update" => RecorderMsg::InstallUpdate,
            "save" => RecorderMsg::Save(false),
            "save-all" => RecorderMsg::Save(true),
            "show" => RecorderMsg::Show,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for m in [
            RecorderMsg::Reload,
            RecorderMsg::UpdateStaged,
            RecorderMsg::InstallUpdate,
            RecorderMsg::Save(false),
            RecorderMsg::Save(true),
            RecorderMsg::Show,
        ] {
            assert_eq!(RecorderMsg::from_line(m.to_line()), Some(m));
        }
        assert_eq!(RecorderMsg::from_line("nope"), None);
    }
}
