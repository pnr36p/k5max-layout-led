//! Application logic shared by all platforms: the engine thread, the config and the
//! layout on display. A platform shell owns a `Controller`, feeds it events and draws
//! the result in its own tray or menu bar.

use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use std::{fs, io};

use crate::config::{Config, Input};
use crate::device::{self, Cmd, Engine, Link};
use crate::log;

pub struct Controller {
    commands: Sender<Cmd>,
    worker: Option<JoinHandle<()>>,
    config: Config,
    config_path: PathBuf,
    input: Option<Input>,
    layout: Option<usize>,
    connected: bool,
}

impl Controller {
    /// Starts the engine thread. `open` finds and opens the keyboard, `on_status` is
    /// called from that thread whenever the connection comes or goes.
    pub fn start<L, O, S>(config: Config, config_path: PathBuf, open: O, on_status: S) -> Self
    where
        L: Link + Send + 'static,
        O: FnMut() -> io::Result<L> + Send + 'static,
        S: FnMut(bool) + Send + 'static,
    {
        let (commands, receiver) = mpsc::channel();
        let engine = Engine::new(
            open,
            on_status,
            config.heartbeat,
            config.device.keys.len(),
            Instant::now(),
        );
        let worker = thread::spawn(move || device::run(engine, receiver));
        Self {
            commands,
            worker: Some(worker),
            config,
            config_path,
            input: None,
            layout: None,
            connected: false,
        }
    }

    #[cfg(test)]
    fn config(&self) -> &Config {
        &self.config
    }

    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    /// Tells the engine about a device arrival or removal, or a system resume.
    pub fn notify(&self, cmd: Cmd) {
        let _ = self.commands.send(cmd);
    }

    /// The platform saw the keyboard layout in use. Returns true when the layout on
    /// display changed, so the shell knows to redraw its status.
    pub fn input_changed(&mut self, input: Input) -> bool {
        if self.input.as_ref() == Some(&input) {
            return false;
        }
        log!("input {input}");
        let index = self.config.layout_for(&input);
        self.input = Some(input);

        let Some(index) = index else {
            log!("no layout configured for it, keeping the current colors");
            return false;
        };
        if self.layout == Some(index) {
            return false;
        }
        self.layout = Some(index);
        let layout = &self.config.layouts[index];
        log!("layout {}", layout.name);
        self.notify(Cmd::Frame(layout.frame.clone()));
        true
    }

    pub fn reload_config(&mut self) -> Result<(), String> {
        let text = fs::read_to_string(&self.config_path).map_err(|err| err.to_string())?;
        let config = Config::parse(&text)?;
        // The link was opened for this device and frames are sized for this key list.
        if config.device != self.config.device {
            return Err("[device] changed, restart the app to apply it".into());
        }
        self.config = config;
        log::set_enabled(self.config.log);
        log!("config reloaded");
        self.notify(Cmd::Heartbeat(self.config.heartbeat));
        self.layout = None;
        if let Some(input) = self.input.take() {
            self.input_changed(input);
        }
        Ok(())
    }

    pub fn status_text(&self) -> String {
        let link = if self.connected { "connected" } else { "not connected" };
        match self.layout {
            Some(index) => format!("K5 Max: {link}, {}", self.config.layouts[index].name),
            None => format!("K5 Max: {link}"),
        }
    }

    /// Stops the engine thread, returning the keyboard to its own effect if configured.
    pub fn stop(mut self) {
        let restore = self.config.restore_on_exit;
        self.notify(Cmd::Shutdown { restore });
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_CONFIG;
    use std::time::Duration;

    struct Absent;

    impl Link for Absent {
        fn send(&mut self, _: &[u8; device::PACKET_LEN]) -> io::Result<()> {
            Err(io::ErrorKind::NotConnected.into())
        }
        fn recv(&mut self, _: Duration) -> io::Result<Option<[u8; device::PACKET_LEN]>> {
            Err(io::ErrorKind::NotConnected.into())
        }
        fn discard_input(&mut self) {}
    }

    fn controller(text: &str, name: &str) -> (Controller, PathBuf) {
        let path = std::env::temp_dir().join(format!("k5max-test-{}-{name}.toml", std::process::id()));
        fs::write(&path, text).unwrap();
        let config = Config::parse(text).unwrap();
        let open = || Err::<Absent, _>(io::Error::from(io::ErrorKind::NotFound));
        (Controller::start(config, path.clone(), open, |_| {}), path)
    }

    fn lang(id: u16) -> Input {
        Input::Lang(id)
    }

    #[test]
    fn only_a_change_of_layout_is_reported() {
        let (mut controller, path) = controller(DEFAULT_CONFIG, "changes");
        assert!(controller.input_changed(lang(0x0409)));
        assert!(!controller.input_changed(lang(0x0409)));
        assert!(controller.input_changed(lang(0x0419)));
        // An unknown language falls back to US, which is a change from RU.
        assert!(controller.input_changed(lang(0x0407)));
        // Another language that also maps to US changes nothing on display.
        assert!(!controller.input_changed(lang(0x040C)));
        assert_eq!(controller.status_text(), "K5 Max: not connected, US");
        controller.stop();
        let _ = fs::remove_file(path);
    }

    #[test]
    fn language_without_a_layout_keeps_the_colors() {
        let text = DEFAULT_CONFIG.replace("unknown_layout = \"US\"", "");
        let (mut controller, path) = controller(&text, "unknown");
        assert!(controller.input_changed(lang(0x0419)));
        assert!(!controller.input_changed(lang(0x0407)));
        assert_eq!(controller.status_text(), "K5 Max: not connected, RU");
        controller.stop();
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reload_applies_new_colors_and_shows_the_layout_again() {
        let (mut controller, path) = controller(DEFAULT_CONFIG, "reload");
        controller.input_changed(lang(0x0419));
        fs::write(&path, DEFAULT_CONFIG.replace("[layouts.RU]", "[layouts.RUS]")).unwrap();
        controller.reload_config().unwrap();
        assert_eq!(controller.status_text(), "K5 Max: not connected, RUS");
        controller.stop();
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reload_refuses_a_different_device_and_keeps_the_old_config() {
        let (mut controller, path) = controller(DEFAULT_CONFIG, "device");
        controller.input_changed(lang(0x0409));
        fs::write(&path, DEFAULT_CONFIG.replace("pid = 0x0A50", "pid = 0x0A51")).unwrap();
        let err = controller.reload_config().err().unwrap();
        assert!(err.contains("restart"), "{err}");
        assert_eq!(controller.config().device.pid, 0x0A50);

        fs::write(&path, "[device\n").unwrap();
        assert!(controller.reload_config().is_err());
        assert_eq!(controller.status_text(), "K5 Max: not connected, US");
        controller.stop();
        let _ = fs::remove_file(path);
    }
}
