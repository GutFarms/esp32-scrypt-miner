//! On-device GUI screens and navigation (ESP32-2432S028 / CYD).

/// Screens shown on the LCD during normal operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuiScreen {
    /// Live hashrate / shares dashboard.
    Mining,
    /// Saved address / password / stratum.
    Config,
    /// WiFi + stratum status.
    Radio,
    /// Soft menu: change credentials, back to mining.
    Menu,
}

impl GuiScreen {
    pub const ALL: [GuiScreen; 4] = [
        GuiScreen::Mining,
        GuiScreen::Config,
        GuiScreen::Radio,
        GuiScreen::Menu,
    ];

    pub fn title(self) -> &'static str {
        match self {
            GuiScreen::Mining => "MINE",
            GuiScreen::Config => "CONF",
            GuiScreen::Radio => "RADIO",
            GuiScreen::Menu => "MENU",
        }
    }

    pub fn next(self) -> Self {
        match self {
            GuiScreen::Mining => GuiScreen::Config,
            GuiScreen::Config => GuiScreen::Radio,
            GuiScreen::Radio => GuiScreen::Menu,
            GuiScreen::Menu => GuiScreen::Mining,
        }
    }
}

/// Highlighted menu row on the Menu screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    ChangeCredentials,
    BackToMining,
}

impl MenuItem {
    pub const ALL: [MenuItem; 2] = [MenuItem::ChangeCredentials, MenuItem::BackToMining];

    pub fn label(self) -> &'static str {
        match self {
            MenuItem::ChangeCredentials => "Change credentials",
            MenuItem::BackToMining => "Back to mining",
        }
    }

    pub fn next(self) -> Self {
        match self {
            MenuItem::ChangeCredentials => MenuItem::BackToMining,
            MenuItem::BackToMining => MenuItem::ChangeCredentials,
        }
    }
}

/// High-level GUI state driven by the BOOT / custom buttons.
#[derive(Clone, Debug)]
pub struct GuiState {
    pub screen: GuiScreen,
    pub menu: MenuItem,
    /// True when the change-credentials action was selected.
    pub request_change: bool,
}

impl Default for GuiState {
    fn default() -> Self {
        Self {
            screen: GuiScreen::Mining,
            menu: MenuItem::ChangeCredentials,
            request_change: false,
        }
    }
}

impl GuiState {
    /// Short BOOT press: next screen, or advance menu highlight.
    pub fn on_boot_short_press(&mut self) {
        match self.screen {
            GuiScreen::Menu => self.menu = self.menu.next(),
            other => self.screen = other.next(),
        }
    }

    /// Action (BOOT long-press on CYD): activate menu item, or jump to menu.
    pub fn on_action_press(&mut self) {
        match self.screen {
            GuiScreen::Menu => match self.menu {
                MenuItem::ChangeCredentials => self.request_change = true,
                MenuItem::BackToMining => self.screen = GuiScreen::Mining,
            },
            _ => self.screen = GuiScreen::Menu,
        }
    }

    pub fn take_change_request(&mut self) -> bool {
        let v = self.request_change;
        self.request_change = false;
        v
    }

    /// Jump to a tab by index (0=MINE … 3=MENU) for touch strip.
    pub fn set_tab(&mut self, index: usize) {
        self.screen = GuiScreen::ALL[index.min(GuiScreen::ALL.len() - 1)];
    }

    pub fn select_menu_row(&mut self, index: usize) {
        self.screen = GuiScreen::Menu;
        self.menu = MenuItem::ALL[index.min(MenuItem::ALL.len() - 1)];
    }

    pub fn activate_menu(&mut self) {
        match self.menu {
            MenuItem::ChangeCredentials => self.request_change = true,
            MenuItem::BackToMining => self.screen = GuiScreen::Mining,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_cycles_screens_and_menu_activates_change() {
        let mut gui = GuiState::default();
        assert_eq!(gui.screen, GuiScreen::Mining);
        gui.on_boot_short_press();
        assert_eq!(gui.screen, GuiScreen::Config);
        gui.on_boot_short_press();
        assert_eq!(gui.screen, GuiScreen::Radio);
        gui.on_boot_short_press();
        assert_eq!(gui.screen, GuiScreen::Menu);
        gui.on_action_press();
        assert!(gui.take_change_request());
        assert!(!gui.take_change_request());
    }
}
