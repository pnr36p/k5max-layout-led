//! Link to the keyboard: SignalRGB protocol and connection handling.
//!
//! One thread owns the HID handle. The keyboard counts as connected only after
//! it answered a query, and any I/O error closes the handle and starts over.

use std::io;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::config::Frame;
use crate::log;

pub const PACKET_LEN: usize = 32;

const CMD_QMK_VERSION: u8 = 0x21;
const CMD_PROTOCOL_VERSION: u8 = 0x22;
const CMD_STREAM_RGB: u8 = 0x24;
const CMD_MODE_ENABLE: u8 = 0x25;
const CMD_MODE_DISABLE: u8 = 0x26;
const CMD_TOTAL_LEDS: u8 = 0x27;
const CMD_FIRMWARE_TYPE: u8 = 0x28;

const LEDS_PER_PACKET: usize = 9;
const REPLY_TIMEOUT: Duration = Duration::from_millis(250);

/// USB child interfaces show up one by one, so give them time to settle.
const SETTLE_DELAY: Duration = Duration::from_millis(300);
const RESUME_DELAY: Duration = Duration::from_secs(1);
const MIN_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(10);

pub trait Link {
    fn send(&mut self, packet: &[u8; PACKET_LEN]) -> io::Result<()>;
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<[u8; PACKET_LEN]>>;
    fn discard_input(&mut self);
}

fn command(id: u8) -> [u8; PACKET_LEN] {
    let mut packet = [0; PACKET_LEN];
    packet[0] = id;
    packet
}

/// Sends a query and returns the reply. Other input reports are skipped.
fn query<L: Link>(link: &mut L, id: u8) -> io::Result<Option<[u8; PACKET_LEN]>> {
    link.discard_input();
    link.send(&command(id))?;
    for _ in 0..4 {
        match link.recv(REPLY_TIMEOUT)? {
            Some(reply) if reply[0] == id => return Ok(Some(reply)),
            Some(_) => continue,
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// Round trip through the firmware. A successful write alone proves nothing.
fn verify<L: Link>(link: &mut L, led_count: usize) -> io::Result<()> {
    match query(link, CMD_TOTAL_LEDS)? {
        Some(reply) if reply[1] as usize == led_count => Ok(()),
        Some(reply) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("keyboard reports {} LEDs, config.toml lists {led_count}", reply[1]),
        )),
        None => Err(io::Error::new(io::ErrorKind::TimedOut, "no reply from keyboard")),
    }
}

fn paint<L: Link>(link: &mut L, frame: &Frame) -> io::Result<()> {
    link.send(&command(CMD_MODE_ENABLE))?;
    for (index, leds) in frame.chunks(LEDS_PER_PACKET).enumerate() {
        let mut packet = command(CMD_STREAM_RGB);
        packet[1] = (index * LEDS_PER_PACKET) as u8;
        packet[2] = leds.len() as u8;
        for (slot, color) in packet[3..].chunks_exact_mut(3).zip(leds) {
            slot.copy_from_slice(color);
        }
        link.send(&packet)?;
    }
    Ok(())
}

pub fn probe<L: Link>(link: &mut L, led_count: usize) -> io::Result<()> {
    for (name, id) in [
        ("QMK version", CMD_QMK_VERSION),
        ("protocol version", CMD_PROTOCOL_VERSION),
        ("total LEDs", CMD_TOTAL_LEDS),
        ("firmware type", CMD_FIRMWARE_TYPE),
    ] {
        match (query(link, id)?, id) {
            (Some(reply), CMD_TOTAL_LEDS) => {
                let verdict = if reply[1] as usize == led_count { "matches" } else { "DIFFERENT from" };
                println!("{name:18} {} ({verdict} the {led_count} keys in config.toml)", reply[1]);
            }
            (Some(reply), _) => println!("{name:18} {:02X?}", &reply[1..4]),
            (None, _) => println!("{name:18} no reply"),
        }
    }
    Ok(())
}

pub enum Cmd {
    Frame(Frame),
    Heartbeat(Duration),
    DevicesChanged,
    Resumed,
    Shutdown { restore: bool },
}

pub struct Engine<L, O, S> {
    open: O,
    on_status: S,
    link: Option<L>,
    frame: Option<Frame>,
    led_count: usize,
    dirty: bool,
    next_open: Instant,
    next_beat: Instant,
    backoff: Duration,
    heartbeat: Duration,
    last_error: String,
}

impl<L, O, S> Engine<L, O, S>
where
    L: Link,
    O: FnMut() -> io::Result<L>,
    S: FnMut(bool),
{
    pub fn new(open: O, on_status: S, heartbeat: Duration, led_count: usize, now: Instant) -> Self {
        Self {
            open,
            on_status,
            link: None,
            frame: None,
            led_count,
            dirty: true,
            next_open: now,
            next_beat: now,
            backoff: MIN_BACKOFF,
            heartbeat,
            last_error: String::new(),
        }
    }

    pub fn handle(&mut self, cmd: Cmd, now: Instant) {
        match cmd {
            Cmd::Frame(frame) => {
                self.frame = Some(frame);
                self.dirty = true;
            }
            Cmd::Heartbeat(interval) => {
                self.heartbeat = interval;
                self.next_beat = self.next_beat.min(now + interval);
            }
            Cmd::DevicesChanged => {
                if self.link.is_some() {
                    self.next_beat = self.next_beat.min(now + SETTLE_DELAY);
                } else {
                    self.next_open = now + SETTLE_DELAY;
                    self.backoff = MIN_BACKOFF;
                }
            }
            Cmd::Resumed => {
                self.drop_link();
                self.next_open = now + RESUME_DELAY;
                self.backoff = MIN_BACKOFF;
            }
            Cmd::Shutdown { restore } => {
                if let (true, Some(link)) = (restore, self.link.as_mut()) {
                    let _ = link.send(&command(CMD_MODE_DISABLE));
                }
                self.drop_link();
            }
        }
    }

    /// Does the work that is due and returns when to call again.
    pub fn step(&mut self, now: Instant) -> Instant {
        let result = if self.link.is_none() {
            if now >= self.next_open {
                self.connect(now)
            } else {
                Ok(())
            }
        } else if now >= self.next_beat {
            self.beat(now)
        } else {
            self.repaint()
        };
        if let Err(err) = result {
            self.fail(err, now);
        }
        if self.link.is_some() {
            self.next_beat
        } else {
            self.next_open
        }
    }

    fn connect(&mut self, now: Instant) -> io::Result<()> {
        let mut link = (self.open)()?;
        verify(&mut link, self.led_count)?;
        self.link = Some(link);
        self.dirty = true;
        self.repaint()?;
        self.backoff = MIN_BACKOFF;
        self.last_error.clear();
        self.next_beat = now + self.heartbeat;
        log!("keyboard connected");
        (self.on_status)(true);
        Ok(())
    }

    fn beat(&mut self, now: Instant) -> io::Result<()> {
        self.next_beat = now + self.heartbeat;
        if let Some(link) = self.link.as_mut() {
            verify(link, self.led_count)?;
        }
        // The firmware loses the picture on USB suspend, even without a re-enumeration.
        self.dirty = true;
        self.repaint()
    }

    fn repaint(&mut self) -> io::Result<()> {
        if let (true, Some(link), Some(frame)) = (self.dirty, self.link.as_mut(), &self.frame) {
            paint(link, frame)?;
            self.dirty = false;
        }
        Ok(())
    }

    fn fail(&mut self, err: io::Error, now: Instant) {
        let message = err.to_string();
        if message != self.last_error {
            log!("keyboard link failed: {message}");
            self.last_error = message;
        }
        self.drop_link();
        self.next_open = now + self.backoff;
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
    }

    fn drop_link(&mut self) {
        self.dirty = true;
        if self.link.take().is_some() {
            log!("keyboard disconnected");
            (self.on_status)(false);
        }
    }
}

pub fn run<L, O, S>(mut engine: Engine<L, O, S>, commands: Receiver<Cmd>)
where
    L: Link,
    O: FnMut() -> io::Result<L>,
    S: FnMut(bool),
{
    loop {
        let deadline = engine.step(Instant::now());
        let wait = deadline.saturating_duration_since(Instant::now());
        match commands.recv_timeout(wait) {
            Ok(Cmd::Shutdown { restore }) => {
                engine.handle(Cmd::Shutdown { restore }, Instant::now());
                return;
            }
            Ok(cmd) => engine.handle(cmd, Instant::now()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const BEAT: Duration = Duration::from_secs(30);
    const LEDS: usize = 108;

    /// A scriptable keyboard. Handles from before a `generation` bump are stale,
    /// like handles that survived a hibernation.
    #[derive(Default)]
    struct Keyboard {
        present: bool,
        generation: u32,
        silent: bool,
        packets: Vec<[u8; PACKET_LEN]>,
        pending_reply: Option<[u8; PACKET_LEN]>,
        opens: u32,
    }

    struct FakeLink {
        keyboard: Rc<RefCell<Keyboard>>,
        generation: u32,
    }

    impl Link for FakeLink {
        fn send(&mut self, packet: &[u8; PACKET_LEN]) -> io::Result<()> {
            let mut keyboard = self.keyboard.borrow_mut();
            if !keyboard.present || keyboard.generation != self.generation {
                return Err(io::ErrorKind::NotConnected.into());
            }
            keyboard.packets.push(*packet);
            if packet[0] == CMD_TOTAL_LEDS && !keyboard.silent {
                let mut reply = *packet;
                reply[1] = LEDS as u8;
                keyboard.pending_reply = Some(reply);
            }
            Ok(())
        }

        fn recv(&mut self, _: Duration) -> io::Result<Option<[u8; PACKET_LEN]>> {
            Ok(self.keyboard.borrow_mut().pending_reply.take())
        }

        fn discard_input(&mut self) {}
    }

    type Status = Rc<RefCell<Vec<bool>>>;

    fn engine(
        keyboard: &Rc<RefCell<Keyboard>>,
        status: &Status,
        now: Instant,
    ) -> Engine<FakeLink, impl FnMut() -> io::Result<FakeLink>, impl FnMut(bool)> {
        let (keyboard, status) = (keyboard.clone(), status.clone());
        Engine::new(
            move || {
                let mut state = keyboard.borrow_mut();
                state.opens += 1;
                if !state.present {
                    return Err(io::ErrorKind::NotFound.into());
                }
                let generation = state.generation;
                drop(state);
                Ok(FakeLink { keyboard: keyboard.clone(), generation })
            },
            move |connected| status.borrow_mut().push(connected),
            BEAT,
            LEDS,
            now,
        )
    }

    fn setup(present: bool) -> (Rc<RefCell<Keyboard>>, Status) {
        let keyboard = Keyboard { present, ..Default::default() };
        (Rc::new(RefCell::new(keyboard)), Rc::new(RefCell::new(Vec::new())))
    }

    fn frame(red: u8) -> Cmd {
        Cmd::Frame(vec![[red, 0, 0]; LEDS])
    }

    fn sent_frames(keyboard: &Rc<RefCell<Keyboard>>, red: u8) -> usize {
        let packets = &keyboard.borrow().packets;
        packets.iter().filter(|p| p[0] == CMD_STREAM_RGB && p[3] == red && p[1] == 99).count()
    }

    #[test]
    fn connect_verifies_then_paints_all_leds() {
        let (keyboard, status) = setup(true);
        let now = Instant::now();
        let mut engine = engine(&keyboard, &status, now);
        engine.handle(frame(7), now);
        engine.step(now);

        let state = keyboard.borrow();
        let ids: Vec<u8> = state.packets.iter().map(|p| p[0]).collect();
        assert_eq!(&ids[..2], [CMD_TOTAL_LEDS, CMD_MODE_ENABLE]);
        let stream: Vec<_> = state.packets.iter().filter(|p| p[0] == CMD_STREAM_RGB).collect();
        assert_eq!(stream.len(), 12);
        for (index, packet) in stream.iter().enumerate() {
            assert_eq!(packet[1] as usize, index * 9);
            assert_eq!(packet[2], 9);
            assert_eq!(packet[3 + 3 * 8], 7);
        }
        assert_eq!(*status.borrow(), [true]);
    }

    // The Node version declared CONNECTED here and then swallowed every error.
    #[test]
    fn handle_without_a_replying_keyboard_is_not_connected() {
        let (keyboard, status) = setup(true);
        keyboard.borrow_mut().silent = true;
        let now = Instant::now();
        let mut engine = engine(&keyboard, &status, now);
        engine.handle(frame(7), now);

        let mut at = now;
        let mut gaps = Vec::new();
        for _ in 0..8 {
            let next = engine.step(at);
            gaps.push(next - at);
            at = next;
        }
        assert!(status.borrow().is_empty());
        assert_eq!(sent_frames(&keyboard, 7), 0);
        assert_eq!(gaps[0], MIN_BACKOFF);
        assert_eq!(*gaps.last().unwrap(), MAX_BACKOFF);
        assert!(gaps.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn stale_handle_after_hibernation_recovers_and_repaints() {
        let (keyboard, status) = setup(true);
        let now = Instant::now();
        let mut engine = engine(&keyboard, &status, now);
        engine.handle(frame(7), now);
        engine.step(now);

        // The next layout change hits the stale handle.
        keyboard.borrow_mut().generation += 1;
        engine.handle(frame(9), now);
        let retry = engine.step(now);
        assert_eq!(*status.borrow(), [true, false]);
        assert_eq!(retry, now + MIN_BACKOFF);
        assert_eq!(sent_frames(&keyboard, 9), 0);

        engine.step(retry);
        assert_eq!(*status.borrow(), [true, false, true]);
        assert_eq!(sent_frames(&keyboard, 9), 1);
    }

    #[test]
    fn resume_reopens_and_repaints_without_any_error() {
        let (keyboard, status) = setup(true);
        let now = Instant::now();
        let mut engine = engine(&keyboard, &status, now);
        engine.handle(frame(7), now);
        engine.step(now);
        let opens = keyboard.borrow().opens;

        engine.handle(Cmd::Resumed, now);
        assert_eq!(engine.step(now), now + RESUME_DELAY);
        engine.step(now + RESUME_DELAY);
        assert_eq!(keyboard.borrow().opens, opens + 1);
        assert_eq!(sent_frames(&keyboard, 7), 2);
        assert_eq!(*status.borrow(), [true, false, true]);
    }

    #[test]
    fn heartbeat_notices_a_silent_link_and_repaints_a_healthy_one() {
        let (keyboard, status) = setup(true);
        let now = Instant::now();
        let mut engine = engine(&keyboard, &status, now);
        engine.handle(frame(7), now);
        let beat = engine.step(now);
        assert_eq!(beat, now + BEAT);

        engine.step(beat);
        assert_eq!(sent_frames(&keyboard, 7), 2);

        keyboard.borrow_mut().silent = true;
        let beat = beat + BEAT;
        engine.step(beat);
        assert_eq!(*status.borrow(), [true, false]);
    }

    #[test]
    fn device_arrival_cuts_the_backoff_short() {
        let (keyboard, status) = setup(false);
        let now = Instant::now();
        let mut engine = engine(&keyboard, &status, now);
        engine.handle(frame(7), now);

        let mut at = now;
        for _ in 0..6 {
            at = engine.step(at);
        }
        assert!(status.borrow().is_empty());

        keyboard.borrow_mut().present = true;
        engine.handle(Cmd::DevicesChanged, at);
        let next = engine.step(at);
        assert_eq!(next, at + SETTLE_DELAY);
        engine.step(next);
        assert_eq!(*status.borrow(), [true]);
        assert_eq!(sent_frames(&keyboard, 7), 1);
    }

    #[test]
    fn frame_changes_while_disconnected_paint_only_the_latest() {
        let (keyboard, status) = setup(false);
        let now = Instant::now();
        let mut engine = engine(&keyboard, &status, now);
        engine.handle(frame(1), now);
        let next = engine.step(now);
        engine.handle(frame(2), now);
        engine.handle(frame(3), now);

        keyboard.borrow_mut().present = true;
        engine.step(next);
        assert_eq!(sent_frames(&keyboard, 1) + sent_frames(&keyboard, 2), 0);
        assert_eq!(sent_frames(&keyboard, 3), 1);
    }

    #[test]
    fn shutdown_restores_the_keyboard_effect_on_request() {
        let (keyboard, status) = setup(true);
        let now = Instant::now();
        let mut engine = engine(&keyboard, &status, now);
        engine.step(now);
        engine.handle(Cmd::Shutdown { restore: true }, now);
        assert_eq!(keyboard.borrow().packets.last().unwrap()[0], CMD_MODE_DISABLE);
    }

    #[test]
    fn led_count_mismatch_is_reported() {
        let (keyboard, _) = setup(true);
        let mut link = FakeLink { keyboard, generation: 0 };
        let err = verify(&mut link, 87).err().unwrap().to_string();
        assert!(err.contains("108") && err.contains("87"), "{err}");
        assert!(verify(&mut link, LEDS).is_ok());
    }
}
