//! The system-wide shortcut that brings the window to the front from any app: what it
//! can't be, holding it with macOS, and what a press does.
//!
//! macOS gives it out through Carbon's `RegisterEventHotKey`, which needs no permission
//! and keeps the keys from the app in front. An `NSEvent` global monitor would need
//! Accessibility and still let that app see them; an event tap needs Input Monitoring.

use gpui_kit::*;

use crate::config::AppSettings;

/// Whether the app can have a system-wide shortcut. Elsewhere the settings don't offer
/// one, and nothing is ever registered.
pub(crate) const GLOBAL_SHORTCUT: bool = cfg!(target_os = "macos");

/// Why a keystroke can't be the system-wide shortcut.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(target_os = "macos"),
    expect(dead_code, reason = "only macOS's hot keys give the rest")
)]
pub(crate) enum Refusal {
    /// It has none of ⌘, ⌃ and ⌥, so while it is taken it couldn't be typed in any
    /// other app. F-keys are the exception.
    NoModifier,
    /// The app binds it itself, in some part of the window, and would stop seeing it.
    InApp,
    /// One of macOS's own shortcuts: turned on in System Settings, or one it keeps, like
    /// ⌘Tab. Named where it is one macOS starts with.
    System(Option<&'static str>),
    /// Another app holds it already. macOS doesn't say which.
    Taken,
    /// macOS turned it down for another reason, with this status. macOS 15.0 and 15.1
    /// did so for ⌥ or ⌥⇧ alone.
    Refused(i32),
    /// No key on the current keyboard layout types it.
    NoKey,
}

/// What a shortcut takes from other apps that it is still allowed to, with a warning.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shadows {
    /// ⌘ and one key, with ⇧ or without, as most apps' menu items are.
    Menus,
    /// ⌥ and one key that types, with ⇧ or without, which most layouts type a character
    /// with: ⌥L is @ on a German one.
    Typing,
}

pub(crate) fn shadows(keystroke: &Keystroke) -> Option<Shadows> {
    let modifiers = keystroke.modifiers;
    if modifiers.control || is_function_key(&keystroke.key) {
        return None;
    }
    // gpui names the keys that don't type, such as "left", and gives the rest as what
    // they type.
    let types = keystroke.key.chars().count() == 1 || keystroke.key == "space";
    match (modifiers.platform, modifiers.alt) {
        (true, false) => Some(Shadows::Menus),
        (false, true) if types => Some(Shadows::Typing),
        _ => None,
    }
}

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('f')
        .and_then(|number| number.parse::<u8>().ok())
        .is_some_and(|number| (1..=20).contains(&number))
}

/// The shortcut the settings hold, as gpui writes a keystroke. None for text that isn't
/// one, such as a file edited by hand.
pub(crate) fn parse_shortcut(text: &str) -> Option<Keystroke> {
    let mut keystroke = Keystroke::parse(text).ok()?;
    // What the keys type plays no part in the shortcut.
    keystroke.key_char = None;
    Some(keystroke)
}

/// Holds `keystroke` with macOS as the shortcut, if it can be one.
fn hold(keystroke: &Keystroke, cx: &App) -> Result<Registration, Refusal> {
    let modifiers = keystroke.modifiers;
    if !(modifiers.platform || modifiers.control || modifiers.alt)
        && !is_function_key(&keystroke.key)
    {
        return Err(Refusal::NoModifier);
    }
    // In any context, and as the first keys of a longer binding too, as the shortcut
    // would take the keys from all of them.
    let keymap = cx.key_bindings();
    if keymap.borrow().bindings().any(|binding| {
        binding
            .match_keystrokes(std::slice::from_ref(keystroke))
            .is_some()
    }) {
        return Err(Refusal::InApp);
    }
    #[cfg(target_os = "macos")]
    {
        carbon::hold(keystroke)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(Refusal::Refused(0))
    }
}

#[cfg(target_os = "macos")]
use carbon::Registration;

/// Never made where there is no shortcut to hold.
#[cfg(not(target_os = "macos"))]
enum Registration {}

/// The shortcut as macOS holds it for the app, and why the one the settings name isn't
/// held, if it isn't.
#[derive(Default)]
pub(crate) struct GlobalShortcut {
    /// Held until dropped, which gives it back.
    held: Option<(Keystroke, Registration)>,
    /// The shortcut the settings name and macOS wouldn't give the app, such as one
    /// another app took before this one started.
    pub(crate) failed: Option<(Keystroke, Refusal)>,
    /// Let go of while the settings record a new one, so pressing it records it rather
    /// than bringing the window to the front.
    paused: bool,
}

impl GlobalShortcut {
    /// Holds the shortcut the settings name, while they have it on, and lets go of any
    /// other. `again` asks once more for one macOS turned down, or for one whose key
    /// may have moved with the keyboard layout.
    pub(crate) fn sync(&mut self, again: bool, cx: &App) {
        let settings = cx.global::<AppSettings>();
        let wanted = settings
            .bring_to_front
            .then_some(settings.bring_to_front_shortcut.as_deref())
            .flatten()
            .and_then(parse_shortcut)
            .filter(|_| !self.paused);
        let held = self.held.as_ref().map(|(keystroke, _)| keystroke);
        let failed = self.failed.as_ref().map(|(keystroke, _)| keystroke);
        if !again && wanted.is_some() && (held == wanted.as_ref() || failed == wanted.as_ref()) {
            return;
        }
        // Given back first, as macOS refuses a shortcut the app holds already.
        self.held = None;
        self.failed = None;
        let Some(keystroke) = wanted else {
            return;
        };
        match hold(&keystroke, cx) {
            Ok(registration) => self.held = Some((keystroke, registration)),
            Err(refusal) => self.failed = Some((keystroke, refusal)),
        }
    }

    /// Lets go of the shortcut until `resume`.
    pub(crate) fn pause(&mut self) {
        self.paused = true;
        self.held = None;
    }

    pub(crate) fn resume(&mut self, cx: &App) {
        self.paused = false;
        self.sync(false, cx);
    }

    /// Holds `keystroke` as the shortcut if it can be, before the settings are changed to
    /// name it.
    pub(crate) fn take(&mut self, keystroke: Keystroke, cx: &App) -> Result<(), Refusal> {
        self.held = None;
        let registration = hold(&keystroke, cx)?;
        self.held = Some((keystroke, registration));
        self.failed = None;
        self.paused = false;
        Ok(())
    }
}

/// Each press of the shortcut, from the moment it is first held. None where there is
/// no shortcut, or if this was asked for before.
pub(crate) fn presses() -> Option<smol::channel::Receiver<()>> {
    #[cfg(target_os = "macos")]
    {
        carbon::listen()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// What a press does: shows the window in front of every other, from behind them, from
/// the Dock or from hidden, on whichever desktop macOS switches to for it. With `hide`
/// and the window in front already, hides the app instead.
pub(crate) fn bring_to_front(hide: bool, window: &mut Window, cx: &mut App) {
    if hide && window.is_window_active() {
        cx.hide();
        return;
    }
    #[cfg(target_os = "macos")]
    carbon::unhide(window);
    cx.activate(true);
    window.activate_window();
}

#[cfg(target_os = "macos")]
mod carbon {
    //! Carbon's hot keys, the keyboard layout they are read through, and macOS's own
    //! shortcuts. None of it is in the objc2 crates, which cover Objective-C frameworks.

    use std::ffi::c_void;
    use std::ptr::{null, null_mut};
    use std::sync::OnceLock;

    use gpui_kit::{Keystroke, Window};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSView};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::Refusal;

    type OSStatus = i32;
    type CFTypeRef = *const c_void;

    #[repr(C)]
    struct EventTypeSpec {
        event_class: u32,
        event_kind: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct EventHotKeyID {
        signature: u32,
        id: u32,
    }

    type EventHandler = extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> OSStatus;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn GetApplicationEventTarget() -> *mut c_void;
        fn InstallEventHandler(
            target: *mut c_void,
            handler: EventHandler,
            type_count: usize,
            types: *const EventTypeSpec,
            user_data: *mut c_void,
            out_handler: *mut *mut c_void,
        ) -> OSStatus;
        fn GetEventParameter(
            event: *mut c_void,
            name: u32,
            desired_type: u32,
            actual_type: *mut u32,
            buffer_size: usize,
            actual_size: *mut usize,
            data: *mut c_void,
        ) -> OSStatus;
        fn RegisterEventHotKey(
            key_code: u32,
            modifiers: u32,
            id: EventHotKeyID,
            target: *mut c_void,
            options: u32,
            out_hot_key: *mut *mut c_void,
        ) -> OSStatus;
        fn UnregisterEventHotKey(hot_key: *mut c_void) -> OSStatus;
        fn CopySymbolicHotKeys(out_hot_keys: *mut CFTypeRef) -> OSStatus;

        fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
        fn TISGetInputSourceProperty(source: *mut c_void, key: CFTypeRef) -> CFTypeRef;
        static kTISPropertyUnicodeKeyLayoutData: CFTypeRef;
        fn LMGetKbdType() -> u8;
        fn UCKeyTranslate(
            layout: *const c_void,
            key_code: u16,
            key_action: u16,
            modifier_state: u32,
            keyboard_type: u32,
            options: u32,
            dead_key_state: *mut u32,
            max_length: usize,
            actual_length: *mut usize,
            chars: *mut u16,
        ) -> OSStatus;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
        fn CFArrayGetCount(array: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
        fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFNumberGetTypeID() -> usize;
        fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> u8;
        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(boolean: CFTypeRef) -> u8;
        fn CFStringCreateWithBytes(
            allocator: CFTypeRef,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            external: u8,
        ) -> CFTypeRef;
    }

    const fn four_chars(code: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*code)
    }

    const EVENT_CLASS_KEYBOARD: u32 = four_chars(b"keyb");
    const EVENT_HOT_KEY_PRESSED: u32 = 5;
    const EVENT_PARAM_DIRECT_OBJECT: u32 = four_chars(b"----");
    const TYPE_EVENT_HOT_KEY_ID: u32 = four_chars(b"hkid");
    const EVENT_NOT_HANDLED: OSStatus = -9874;
    const EVENT_HOT_KEY_EXISTS: OSStatus = -9878;
    /// Refuses a shortcut another app holds, rather than both apps getting each press.
    const HOT_KEY_EXCLUSIVE: u32 = 1;
    /// Marks the app's own hot keys, as every app's come to the same event handler chain.
    const SIGNATURE: u32 = four_chars(b"ACPc");

    // Carbon's modifier bits, as `RegisterEventHotKey` and `CopySymbolicHotKeys` use them.
    const CMD: u32 = 1 << 8;
    const SHIFT: u32 = 1 << 9;
    const OPTION: u32 = 1 << 11;
    const CONTROL: u32 = 1 << 12;
    /// The four a shortcut is told apart by. macOS's own shortcuts on arrow and F-keys
    /// carry the fn bit too.
    const MODIFIERS: u32 = CMD | SHIFT | OPTION | CONTROL;

    static PRESSES: OnceLock<smol::channel::Sender<()>> = OnceLock::new();

    /// Has every press of the app's hot keys sent on the channel returned. The handler is
    /// installed once for the app, before any hot key is registered.
    pub(super) fn listen() -> Option<smol::channel::Receiver<()>> {
        let (sender, presses) = smol::channel::unbounded();
        PRESSES.set(sender).ok()?;
        let pressed = EventTypeSpec {
            event_class: EVENT_CLASS_KEYBOARD,
            event_kind: EVENT_HOT_KEY_PRESSED,
        };
        let mut handler = null_mut();
        // SAFETY: called on the main thread, where gpui runs AppKit's event loop, which
        // hands Carbon its events.
        let status = unsafe {
            InstallEventHandler(
                GetApplicationEventTarget(),
                on_hot_key,
                1,
                &pressed,
                null_mut(),
                &mut handler,
            )
        };
        if status != 0 {
            eprintln!("couldn't listen for the system-wide shortcut: error {status}");
            return None;
        }
        Some(presses)
    }

    extern "C" fn on_hot_key(_: *mut c_void, event: *mut c_void, _: *mut c_void) -> OSStatus {
        let mut id = EventHotKeyID::default();
        // SAFETY: `event` is the hot key event Carbon is handing out, and `id` is the
        // size given.
        let status = unsafe {
            GetEventParameter(
                event,
                EVENT_PARAM_DIRECT_OBJECT,
                TYPE_EVENT_HOT_KEY_ID,
                null_mut(),
                size_of::<EventHotKeyID>(),
                null_mut(),
                (&raw mut id).cast(),
            )
        };
        if status != 0 || id.signature != SIGNATURE {
            return EVENT_NOT_HANDLED;
        }
        if let Some(presses) = PRESSES.get() {
            let _ = presses.try_send(());
        }
        0
    }

    /// A hot key the app holds, given back when dropped.
    pub(super) struct Registration(*mut c_void);

    impl Drop for Registration {
        fn drop(&mut self) {
            // SAFETY: registered by `register`, and given back only here.
            unsafe { UnregisterEventHotKey(self.0) };
        }
    }

    /// Holds the key that presses `keystroke` on the current layout, unless macOS has a
    /// shortcut of its own there or another app holds it.
    pub(super) fn hold(keystroke: &Keystroke) -> Result<Registration, Refusal> {
        let (key_code, modifiers) = key_code(keystroke).ok_or(Refusal::NoKey)?;
        check_system(key_code, modifiers)?;
        register(key_code, modifiers)
    }

    fn register(key_code: u32, modifiers: u32) -> Result<Registration, Refusal> {
        let id = EventHotKeyID {
            signature: SIGNATURE,
            id: 1,
        };
        let mut hot_key = null_mut();
        // SAFETY: called on the main thread, as gpui runs every task there.
        let status = unsafe {
            RegisterEventHotKey(
                key_code,
                modifiers,
                id,
                GetApplicationEventTarget(),
                HOT_KEY_EXCLUSIVE,
                &mut hot_key,
            )
        };
        match status {
            0 => Ok(Registration(hot_key)),
            EVENT_HOT_KEY_EXISTS => Err(Refusal::Taken),
            status => Err(Refusal::Refused(status)),
        }
    }

    /// The names of the shortcuts macOS starts with, as System Settings gives them, by
    /// key code and modifiers. `CopySymbolicHotKeys` gives no names.
    const SYSTEM_NAMES: [(u32, u32, &str); 17] = [
        (49, CMD, "Show Spotlight search"),
        (49, CMD | OPTION, "Show Finder search window"),
        (49, CONTROL, "Select the previous input source"),
        (49, CONTROL | OPTION, "Select next source in Input menu"),
        (20, CMD | SHIFT, "Save picture of screen as a file"),
        (
            20,
            CONTROL | CMD | SHIFT,
            "Copy picture of screen to the clipboard",
        ),
        (21, CMD | SHIFT, "Save picture of selected area as a file"),
        (
            21,
            CONTROL | CMD | SHIFT,
            "Copy picture of selected area to the clipboard",
        ),
        (23, CMD | SHIFT, "Screenshot and recording options"),
        (126, CONTROL, "Mission Control"),
        (125, CONTROL, "Application windows"),
        (123, CONTROL, "Move left a space"),
        (124, CONTROL, "Move right a space"),
        (12, CONTROL | CMD, "Lock Screen"),
        (2, CMD | OPTION, "Turn Dock hiding on/off"),
        (50, CMD, "Move focus to next window"),
        (120, CONTROL, "Move focus to the menu bar"),
    ];

    /// Shortcuts macOS keeps that aren't in System Settings' list, and so not in
    /// `CopySymbolicHotKeys`'s.
    const RESERVED: [(u32, u32, &str); 3] = [
        (48, CMD, "Switch apps"),
        (48, CMD | SHIFT, "Switch apps"),
        (53, CMD | OPTION, "Force Quit Applications"),
    ];

    fn check_system(key_code: u32, modifiers: u32) -> Result<(), Refusal> {
        let named = |list: &[(u32, u32, &'static str)]| {
            list.iter()
                .find(|(code, mods, _)| *code == key_code && *mods == modifiers)
                .map(|(_, _, name)| *name)
        };
        if let Some(name) = named(&RESERVED) {
            return Err(Refusal::System(Some(name)));
        }
        if symbolic_hot_keys().contains(&(key_code, modifiers)) {
            return Err(Refusal::System(named(&SYSTEM_NAMES)));
        }
        Ok(())
    }

    /// The shortcuts turned on in System Settings › Keyboard › Keyboard Shortcuts, by key
    /// code and the four modifiers. None if macOS won't say.
    fn symbolic_hot_keys() -> Vec<(u32, u32)> {
        let mut hot_keys = null();
        // SAFETY: the array is ours to release.
        if unsafe { CopySymbolicHotKeys(&mut hot_keys) } != 0 || hot_keys.is_null() {
            return Vec::new();
        }
        // The keys are `CFSTR` macros in the headers, not symbols.
        let (code_key, modifiers_key, enabled_key) = (
            CfString::new("kHISymbolicHotKeyCode"),
            CfString::new("kHISymbolicHotKeyModifiers"),
            CfString::new("kHISymbolicHotKeyEnabled"),
        );
        let mut found = Vec::new();
        // SAFETY: each value is a dictionary, as documented, and each read checks the
        // type of what it reads.
        unsafe {
            for ix in 0..CFArrayGetCount(hot_keys) {
                let hot_key = CFArrayGetValueAtIndex(hot_keys, ix);
                let value = |key: &CfString| CFDictionaryGetValue(hot_key, key.0);
                if !truthy(value(&enabled_key)) {
                    continue;
                }
                if let (Some(code), Some(modifiers)) =
                    (number(value(&code_key)), number(value(&modifiers_key)))
                {
                    found.push((code as u32, modifiers as u32 & MODIFIERS));
                }
            }
            CFRelease(hot_keys);
        }
        found
    }

    /// A Core Foundation string, released when dropped.
    struct CfString(CFTypeRef);

    impl CfString {
        fn new(text: &str) -> Self {
            const UTF8: u32 = 0x0800_0100;
            // SAFETY: the bytes are valid UTF-8 of the given length.
            Self(unsafe {
                CFStringCreateWithBytes(null(), text.as_ptr(), text.len() as isize, UTF8, 0)
            })
        }
    }

    impl Drop for CfString {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: created by `new`, released only here.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    /// # Safety
    /// `value` is null or a Core Foundation object.
    unsafe fn number(value: CFTypeRef) -> Option<i64> {
        const SINT64: isize = 4;
        if value.is_null() || unsafe { CFGetTypeID(value) != CFNumberGetTypeID() } {
            return None;
        }
        let mut number = 0i64;
        (unsafe { CFNumberGetValue(value, SINT64, (&raw mut number).cast()) } != 0)
            .then_some(number)
    }

    /// # Safety
    /// `value` is null or a Core Foundation object.
    unsafe fn truthy(value: CFTypeRef) -> bool {
        if value.is_null() {
            return false;
        }
        if unsafe { CFGetTypeID(value) == CFBooleanGetTypeID() } {
            return unsafe { CFBooleanGetValue(value) } != 0;
        }
        unsafe { number(value) }.is_some_and(|number| number != 0)
    }

    /// The key code and Carbon modifiers that press `keystroke`. A key that types a
    /// character is found on the current keyboard layout, so the shortcut follows its
    /// letter to wherever the layout puts it.
    fn key_code(keystroke: &Keystroke) -> Option<(u32, u32)> {
        let modifiers = keystroke.modifiers;
        let mut carbon = 0;
        for (on, bit) in [
            (modifiers.platform, CMD),
            (modifiers.shift, SHIFT),
            (modifiers.alt, OPTION),
            (modifiers.control, CONTROL),
        ] {
            if on {
                carbon |= bit;
            }
        }
        let code = match named_key_code(&keystroke.key) {
            Some(code) => code,
            None => {
                let (code, shifted) = Layout::current()?.typing(keystroke)?;
                if shifted {
                    carbon |= SHIFT;
                }
                code
            }
        };
        Some((code as u32, carbon))
    }

    /// The keys gpui names rather than giving what they type, at the same place on
    /// every layout.
    const NAMED_KEYS: [(&str, u16); 35] = [
        ("space", 49),
        ("tab", 48),
        ("enter", 36),
        ("backspace", 51),
        ("escape", 53),
        ("delete", 117),
        ("insert", 114),
        ("home", 115),
        ("end", 119),
        ("pageup", 116),
        ("pagedown", 121),
        ("up", 126),
        ("down", 125),
        ("left", 123),
        ("right", 124),
        ("f1", 122),
        ("f2", 120),
        ("f3", 99),
        ("f4", 118),
        ("f5", 96),
        ("f6", 97),
        ("f7", 98),
        ("f8", 100),
        ("f9", 101),
        ("f10", 109),
        ("f11", 103),
        ("f12", 111),
        ("f13", 105),
        ("f14", 107),
        ("f15", 113),
        ("f16", 106),
        ("f17", 64),
        ("f18", 79),
        ("f19", 80),
        ("f20", 90),
    ];

    /// The keypad's keys, which type what keys on the main keyboard type too. A shortcut
    /// goes on the main keyboard's, which every Mac has, and on the keypad's only if no
    /// other key types it.
    const KEYPAD: [u16; 18] = [
        65, 67, 69, 71, 75, 76, 78, 81, 82, 83, 84, 85, 86, 87, 88, 89, 91, 92,
    ];

    fn named_key_code(key: &str) -> Option<u16> {
        NAMED_KEYS
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, code)| *code)
    }

    /// The keyboard layout in use, held while it is read.
    struct Layout {
        source: *mut c_void,
        data: *const c_void,
        kind: u32,
    }

    // `UCKeyTranslate`'s modifier bits: Carbon's, shifted down 8.
    const NO_MOD: u32 = 0;
    const CMD_MOD: u32 = 1;
    const SHIFT_MOD: u32 = 2;

    impl Layout {
        fn current() -> Option<Self> {
            // SAFETY: the source is ours to release; the layout data lives as long as it.
            unsafe {
                let source = TISCopyCurrentKeyboardLayoutInputSource();
                if source.is_null() {
                    return None;
                }
                let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
                if data.is_null() {
                    CFRelease(source);
                    return None;
                }
                Some(Self {
                    source,
                    data: CFDataGetBytePtr(data).cast(),
                    kind: LMGetKbdType() as u32,
                })
            }
        }

        /// What the key types with `modifiers`, read the way gpui reads it for a key
        /// press: a dead key gives its accent on its own.
        fn chars(&self, code: u16, modifiers: u32) -> String {
            const KEY_DOWN: u16 = 0;
            const SPACE: u16 = 49;
            let mut dead_key_state = 0;
            let mut chars = [0u16; 4];
            let mut length = 0;
            // SAFETY: the layout is held, and the buffer is the length given.
            unsafe {
                UCKeyTranslate(
                    self.data,
                    code,
                    KEY_DOWN,
                    modifiers,
                    self.kind,
                    0,
                    &mut dead_key_state,
                    chars.len(),
                    &mut length,
                    chars.as_mut_ptr(),
                );
                if dead_key_state != 0 {
                    UCKeyTranslate(
                        self.data,
                        SPACE,
                        KEY_DOWN,
                        modifiers,
                        self.kind,
                        0,
                        &mut dead_key_state,
                        chars.len(),
                        &mut length,
                        chars.as_mut_ptr(),
                    );
                }
            }
            String::from_utf16(&chars[..length.min(chars.len())]).unwrap_or_default()
        }

        /// The key that gpui reads as `keystroke`'s key, and whether it takes ⇧ that gpui
        /// left out of the keystroke: it reads ⌘⇧2 as ⌘@ on a US layout. Mirrors gpui's
        /// `Keystroke` for a key press on macOS, Command layouts such as Dvorak-QWERTY
        /// and layouts that type no ASCII included.
        fn typing(&self, keystroke: &Keystroke) -> Option<(u16, bool)> {
            let command_layout = keystroke.modifiers.platform || self.always_command();
            let (mut shifted_key, mut keypad_key) = (None, None);
            for code in 0..128 {
                if NAMED_KEYS.iter().any(|(_, named)| *named == code) {
                    continue;
                }
                if KEYPAD.contains(&code) {
                    if keypad_key.is_none() && self.chars(code, NO_MOD) == keystroke.key {
                        keypad_key = Some(code);
                    }
                    continue;
                }
                let (mut base, mut shifted) =
                    (self.chars(code, NO_MOD), self.chars(code, SHIFT_MOD));
                if command_layout {
                    let with_command = self.chars(code, CMD_MOD);
                    let with_both = self.chars(code, CMD_MOD | SHIFT_MOD);
                    if with_both != with_command {
                        shifted = with_both;
                    } else if with_command.to_ascii_uppercase() != with_command {
                        shifted = with_command.to_ascii_uppercase();
                    }
                    base = with_command;
                }
                if base == keystroke.key {
                    return Some((code, false));
                }
                // gpui keeps ⇧ only with a key that types lowercase letters.
                let keeps_shift = base.chars().all(|ch| ch.is_ascii_lowercase());
                if !keystroke.modifiers.shift
                    && !keeps_shift
                    && shifted == keystroke.key
                    && shifted_key.is_none()
                {
                    shifted_key = Some(code);
                }
            }
            shifted_key
                .map(|code| (code, true))
                .or(keypad_key.map(|code| (code, false)))
        }

        /// A layout that types no ASCII, such as Russian, is read through its ⌘ layer, as
        /// gpui does.
        fn always_command(&self) -> bool {
            !self.chars(0, NO_MOD).is_ascii() && self.chars(0, CMD_MOD).is_ascii()
        }
    }

    impl Drop for Layout {
        fn drop(&mut self) {
            // SAFETY: copied in `current`, released only here.
            unsafe { CFRelease(self.source) };
        }
    }

    /// Shows the app if it is hidden and the window if it is in the Dock, so that bringing
    /// the app to the front brings the window with it.
    pub(super) fn unhide(window: &Window) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        if app.isHidden() {
            app.unhideWithoutActivation();
        }
        // `Window::window_handle` is gpui's own handle, not the platform's.
        if let Ok(handle) = HasWindowHandle::window_handle(window)
            && let RawWindowHandle::AppKit(handle) = handle.as_raw()
            // SAFETY: the handle's view is the window's content view, which lives as long
            // as the window.
            && let Some(ns_window) = unsafe { handle.ns_view.cast::<NSView>().as_ref() }.window()
            && ns_window.isMiniaturized()
        {
            ns_window.deminiaturize(None);
        }
    }
}
