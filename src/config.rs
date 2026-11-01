//! `config.toml`: device description, key groups and per-layout colors.

use std::collections::HashMap;
use std::time::Duration;

use toml::{Table, Value};

pub const DEFAULT_CONFIG: &str = include_str!("../config.toml");

pub type Color = [u8; 3];
pub type Frame = Vec<Color>;

/// Placeholder in `keys` for an LED that no key name can address, such as underglow.
const UNNAMED: &str = "_";
/// The protocol addresses LEDs with one byte.
const MAX_LEDS: usize = 255;

#[derive(Clone, PartialEq, Eq)]
pub struct Device {
    pub vid: u16,
    pub pid: u16,
    /// Key name of every LED, in the firmware's LED order.
    pub keys: Vec<String>,
}

pub struct Layout {
    pub name: String,
    pub lang: u16,
    pub frame: Frame,
}

pub struct Config {
    pub device: Device,
    pub layouts: Vec<Layout>,
    pub unknown_layout: Option<usize>,
    pub heartbeat: Duration,
    pub restore_on_exit: bool,
    pub log: bool,
}

struct Leds {
    count: usize,
    by_name: HashMap<String, usize>,
}

impl Leds {
    fn new(keys: &[String]) -> Result<Self, String> {
        let mut by_name = HashMap::new();
        for (index, key) in keys.iter().enumerate().filter(|(_, key)| *key != UNNAMED) {
            if let Some(first) = by_name.insert(key.to_ascii_lowercase(), index) {
                return Err(format!(
                    "[device]: key '{key}' is listed twice (LED {first} and LED {index})"
                ));
            }
        }
        Ok(Self { count: keys.len(), by_name })
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.by_name.get(&name.to_ascii_lowercase()).copied()
    }
}

impl Config {
    pub fn layout_for(&self, lang: u16) -> Option<usize> {
        self.layouts
            .iter()
            .position(|layout| layout.lang == lang)
            .or(self.unknown_layout)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let root: Table = text.parse().map_err(|err| format!("{err}"))?;

        let device = parse_device(&root)?;
        let leds = Leds::new(&device.keys)?;
        let groups = parse_groups(section(&root, "groups")?, &leds)?;
        let empty = Table::new();
        let common = section(&root, "common")?.unwrap_or(&empty);

        let mut layouts = Vec::new();
        for (name, value) in section(&root, "layouts")?.unwrap_or(&empty) {
            let colors = value
                .as_table()
                .ok_or_else(|| format!("[layouts.{name}] must be a table"))?;
            let lang = colors
                .get("lang")
                .and_then(Value::as_integer)
                .and_then(|lang| u16::try_from(lang).ok())
                .ok_or_else(|| format!("[layouts.{name}]: missing or invalid 'lang'"))?;

            let mut frame = vec![[0; 3]; leds.count];
            apply(&mut frame, common, &leds, &groups).map_err(|err| format!("[common]: {err}"))?;
            apply(&mut frame, colors, &leds, &groups)
                .map_err(|err| format!("[layouts.{name}]: {err}"))?;
            layouts.push(Layout { name: name.clone(), lang, frame });
        }
        if layouts.is_empty() {
            return Err("no [layouts.*] sections".into());
        }

        let settings = section(&root, "settings")?.unwrap_or(&empty);
        let unknown_layout = match settings.get("unknown_layout") {
            None => None,
            Some(value) => {
                let name = value.as_str().ok_or("'unknown_layout' must be a string")?;
                let index = layouts.iter().position(|layout| layout.name == name);
                Some(index.ok_or_else(|| format!("'unknown_layout': no layout '{name}'"))?)
            }
        };
        let heartbeat = match settings.get("heartbeat_seconds") {
            None => 30,
            Some(value) => value
                .as_integer()
                .and_then(|seconds| u64::try_from(seconds).ok())
                .filter(|&seconds| seconds > 0)
                .ok_or("'heartbeat_seconds' must be a positive integer")?,
        };
        let restore_on_exit = match settings.get("restore_on_exit") {
            None => true,
            Some(value) => value.as_bool().ok_or("'restore_on_exit' must be true or false")?,
        };
        let log = match settings.get("log") {
            None => true,
            Some(value) => value.as_bool().ok_or("'log' must be true or false")?,
        };

        Ok(Self {
            device,
            layouts,
            unknown_layout,
            heartbeat: Duration::from_secs(heartbeat),
            restore_on_exit,
            log,
        })
    }
}

fn section<'a>(root: &'a Table, name: &str) -> Result<Option<&'a Table>, String> {
    root.get(name)
        .map(|value| value.as_table().ok_or_else(|| format!("'{name}' must be a table")))
        .transpose()
}

fn parse_device(root: &Table) -> Result<Device, String> {
    let device = section(root, "device")?.ok_or("missing [device] section")?;
    let id = |field: &str| {
        device
            .get(field)
            .and_then(Value::as_integer)
            .and_then(|id| u16::try_from(id).ok())
            .ok_or_else(|| format!("[device]: '{field}' must be an integer from 0 to 0xFFFF"))
    };
    let keys: Vec<String> = device
        .get("keys")
        .and_then(Value::as_str)
        .ok_or("[device]: 'keys' must be a string of key names, one per LED")?
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if keys.is_empty() || keys.len() > MAX_LEDS {
        return Err(format!(
            "[device]: 'keys' lists {} LEDs, the protocol allows 1 to {MAX_LEDS}",
            keys.len()
        ));
    }
    Ok(Device { vid: id("vid")?, pid: id("pid")?, keys })
}

fn parse_groups(
    groups: Option<&Table>,
    leds: &Leds,
) -> Result<HashMap<String, Vec<usize>>, String> {
    let mut parsed = HashMap::new();
    for (name, value) in groups.into_iter().flatten() {
        let keys = value
            .as_str()
            .ok_or_else(|| format!("[groups]: '{name}' must be a string of key names"))?;
        let found = keys
            .split_whitespace()
            .map(|key| leds.find(key).ok_or_else(|| format!("[groups]: unknown key '{key}'")))
            .collect::<Result<_, _>>()?;
        parsed.insert(name.clone(), found);
    }
    Ok(parsed)
}

/// Applies `name = color` lines in file order. A name is a group or a single key.
fn apply(
    frame: &mut [Color],
    colors: &Table,
    leds: &Leds,
    groups: &HashMap<String, Vec<usize>>,
) -> Result<(), String> {
    for (name, value) in colors.iter().filter(|(name, _)| *name != "lang") {
        let color = parse_color(value).ok_or_else(|| format!("bad color for '{name}'"))?;
        if let Some(members) = groups.get(name) {
            for &led in members {
                frame[led] = color;
            }
        } else if let Some(led) = leds.find(name) {
            frame[led] = color;
        } else {
            return Err(format!("unknown group or key '{name}'"));
        }
    }
    Ok(())
}

fn parse_color(value: &Value) -> Option<Color> {
    match value {
        Value::Array(items) => {
            let parts: Vec<u8> = items
                .iter()
                .map(|item| item.as_integer().and_then(|c| u8::try_from(c).ok()))
                .collect::<Option<_>>()?;
            parts.try_into().ok()
        }
        Value::String(text) => {
            let hex = text.strip_prefix('#').filter(|hex| hex.len() == 6)?;
            let rgb = u32::from_str_radix(hex, 16).ok()?;
            Some([(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8])
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    fn hex(frame: &[Color]) -> Vec<String> {
        frame.iter().map(|c| format!("{:02x}{:02x}{:02x}", c[0], c[1], c[2])).collect()
    }

    /// A three key keyboard with the given body after the device section.
    fn tiny(body: &str) -> Result<Config, String> {
        Config::parse(&format!("[device]\nvid = 1\npid = 2\nkeys = 'Q W E'\n{body}"))
    }

    // Golden frames were generated from the LED logic and config.json of the Node version.
    #[test]
    fn default_config_matches_node_version() {
        let config = Config::parse(DEFAULT_CONFIG).unwrap();
        let us = config.layout_for(0x0409).unwrap();
        let ru = config.layout_for(0x0419).unwrap();
        assert_eq!(hex(&config.layouts[us].frame), golden(include_str!("../testdata/golden_US.txt")));
        assert_eq!(hex(&config.layouts[ru].frame), golden(include_str!("../testdata/golden_RU.txt")));
    }

    #[test]
    fn default_config_describes_the_k5_max() {
        let device = Config::parse(DEFAULT_CONFIG).unwrap().device;
        assert_eq!((device.vid, device.pid, device.keys.len()), (0x3434, 0x0A50, 108));
    }

    #[test]
    fn log_is_on_unless_switched_off() {
        assert!(tiny("[layouts.X]
lang = 1
").unwrap().log);
        assert!(!tiny("[settings]
log = false
[layouts.X]
lang = 1
").unwrap().log);
        let err = tiny("[settings]
log = 'no'
[layouts.X]
lang = 1
").err().unwrap();
        assert!(err.contains("'log'"), "{err}");
    }

    #[test]
    fn unknown_language_uses_fallback() {
        let config = Config::parse(DEFAULT_CONFIG).unwrap();
        assert_eq!(config.layouts[config.layout_for(0x0407).unwrap()].name, "US");
    }

    #[test]
    fn later_lines_override_earlier_ones() {
        let config =
            tiny("[groups]\nall = 'Q W'\n[layouts.X]\nlang = 1\nall = '#010203'\nw = [9, 9, 9]\n")
                .unwrap();
        let frame = &config.layouts[0].frame;
        assert_eq!(frame, &[[1, 2, 3], [9, 9, 9], [0, 0, 0]]);
        assert_eq!(config.layout_for(2), None);
    }

    #[test]
    fn frame_length_follows_the_key_list() {
        let config = tiny("[layouts.X]\nlang = 1\n").unwrap();
        assert_eq!(config.layouts[0].frame.len(), 3);
    }

    #[test]
    fn unnamed_leds_take_a_slot_but_cannot_be_addressed() {
        let text = "[device]\nvid = 1\npid = 2\nkeys = '_ Q _ W'\n[layouts.X]\nlang = 1\nW = [5, 5, 5]\n";
        let config = Config::parse(text).unwrap();
        assert_eq!(config.layouts[0].frame, [[0; 3], [0; 3], [0; 3], [5, 5, 5]]);
        let err = Config::parse(&text.replace("W = ", "_ = ")).err().unwrap();
        assert!(err.contains("'_'"), "{err}");
    }

    #[test]
    fn errors_name_the_problem() {
        let err = tiny("[groups]\na = 'NOPE'\n[layouts.X]\nlang = 1\n").err().unwrap();
        assert!(err.contains("NOPE"), "{err}");
        let err = tiny("[layouts.X]\nlang = 1\nNOPE = [1, 2, 3]\n").err().unwrap();
        assert!(err.contains("NOPE") && err.contains("layouts.X"), "{err}");
    }

    #[test]
    fn device_section_is_validated() {
        let layouts = "[layouts.X]\nlang = 1\n";
        let err = Config::parse(layouts).err().unwrap();
        assert!(err.contains("[device]"), "{err}");

        let err = Config::parse(&format!("[device]\nvid = 1\nkeys = 'Q'\n{layouts}")).err().unwrap();
        assert!(err.contains("pid"), "{err}");

        let err = Config::parse(&format!("[device]\nvid = 1\npid = 2\nkeys = 'Q q'\n{layouts}"))
            .err()
            .unwrap();
        assert!(err.contains("twice"), "{err}");

        let many = vec!["K"; 300].join(" ");
        let err = Config::parse(&format!("[device]\nvid = 1\npid = 2\nkeys = '{many}'\n{layouts}"))
            .err()
            .unwrap();
        assert!(err.contains("300"), "{err}");
    }
}
