//! The main window's native thumbnail toolbar. Commands use the SMTC channel.

use ::windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::{
            HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi},
            Shell::{
                DefSubclassProc, ITaskbarList3, RemoveWindowSubclass, SetWindowSubclass, THB_FLAGS,
                THB_ICON, THB_TOOLTIP, THBF_DISABLED, THBF_ENABLED, THBN_CLICKED, THUMBBUTTON,
                TaskbarList,
            },
            WindowsAndMessaging::{
                CreateIconFromResourceEx, DestroyIcon, HICON, LR_DEFAULTCOLOR,
                RegisterWindowMessageW, SM_CXICON, WM_COMMAND, WM_DPICHANGED, WM_NCDESTROY,
            },
        },
    },
    core::{Error, Result, w},
};
use async_channel::Sender;
use souvlaki::MediaControlEvent;
use std::{cell::Cell, cell::RefCell, ffi::c_void, rc::Rc};

const SUBCLASS_ID: usize = 0x5744_4e43;
const PREVIOUS: u32 = 1;
const TOGGLE: u32 = 2;
const NEXT: u32 = 3;

struct Icon(HICON);

impl Icon {
    fn new(png: &[u8], size: i32) -> Result<Self> {
        // SAFETY: Windows reads this valid PNG slice and returns an owned icon.
        unsafe { CreateIconFromResourceEx(png, true, 0x0003_0000, size, size, LR_DEFAULTCOLOR) }
            .map(Self)
    }
}

impl Drop for Icon {
    fn drop(&mut self) {
        // SAFETY: This icon is owned exclusively; the taskbar copies its pixels.
        let _ = unsafe { DestroyIcon(self.0) };
    }
}

struct Icons {
    previous: Icon,
    play: Icon,
    pause: Icon,
    next: Icon,
}

impl Icons {
    fn new(hwnd: HWND) -> Result<Self> {
        // SAFETY: The caller owns a live window on this thread.
        let size = unsafe { GetSystemMetricsForDpi(SM_CXICON, GetDpiForWindow(hwnd)) };
        Ok(Self {
            previous: Icon::new(include_bytes!("../../assets/taskbar/skip-back.png"), size)?,
            play: Icon::new(include_bytes!("../../assets/taskbar/play.png"), size)?,
            pause: Icon::new(include_bytes!("../../assets/taskbar/pause.png"), size)?,
            next: Icon::new(
                include_bytes!("../../assets/taskbar/skip-forward.png"),
                size,
            )?,
        })
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Playback {
    has_track: bool,
    playing: bool,
}

struct State {
    hwnd: HWND,
    created_message: u32,
    attached: Cell<bool>,
    playback: Cell<Playback>,
    taskbar: RefCell<Option<ITaskbarList3>>,
    icons: RefCell<Icons>,
    commands: Sender<MediaControlEvent>,
}

impl State {
    fn buttons(&self) -> [THUMBBUTTON; 3] {
        let icons = self.icons.borrow();
        let playback = self.playback.get();
        [
            button(
                PREVIOUS,
                icons.previous.0,
                "Previous track",
                playback.has_track,
            ),
            button(
                TOGGLE,
                if playback.playing {
                    icons.pause.0
                } else {
                    icons.play.0
                },
                if playback.playing { "Pause" } else { "Play" },
                playback.has_track,
            ),
            button(NEXT, icons.next.0, "Next track", playback.has_track),
        ]
    }

    fn register(&self) -> Result<()> {
        // Explorer sends TaskbarButtonCreated again after restarting. A fresh
        // interface also discards any connection to its previous instance.
        self.taskbar.replace(None);
        // SAFETY: GPUI initializes OLE on this UI thread. The OS has announced
        // this live window's taskbar button; all icon handles remain owned.
        let taskbar: ITaskbarList3 =
            unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)? };
        unsafe {
            taskbar.HrInit()?;
            taskbar.ThumbBarAddButtons(self.hwnd, &self.buttons())?;
        }
        if self.attached.get() {
            self.taskbar.replace(Some(taskbar));
        }
        Ok(())
    }

    fn update(&self) {
        // Release the RefCell borrow before COM can dispatch window messages.
        let taskbar = self.taskbar.borrow().clone();
        if let Some(taskbar) = taskbar {
            // SAFETY: The interface belongs to this live window and UI thread.
            if let Err(error) = unsafe { taskbar.ThumbBarUpdateButtons(self.hwnd, &self.buttons()) }
            {
                log::warn!("could not update taskbar controls: {error}");
            }
        }
    }
}

fn button(id: u32, icon: HICON, tip: &str, enabled: bool) -> THUMBBUTTON {
    let mut button = THUMBBUTTON {
        dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
        iId: id,
        hIcon: icon,
        dwFlags: if enabled { THBF_ENABLED } else { THBF_DISABLED },
        ..Default::default()
    };
    for (slot, codepoint) in button.szTip.iter_mut().take(259).zip(tip.encode_utf16()) {
        *slot = codepoint;
    }
    button
}

fn command(wparam: WPARAM) -> Option<MediaControlEvent> {
    if (wparam.0 >> 16) != THBN_CLICKED as usize {
        return None;
    }
    match wparam.0 as u16 as u32 {
        PREVIOUS => Some(MediaControlEvent::Previous),
        TOGGLE => Some(MediaControlEvent::Toggle),
        NEXT => Some(MediaControlEvent::Next),
        _ => None,
    }
}

/// Owns the callback and icons on GPUI's UI thread; Rc prevents cross-thread use.
pub(super) struct TaskbarControls(Rc<State>);

impl TaskbarControls {
    /// # Safety
    /// `hwnd` must be a live window owned by the current, COM-initialized thread.
    pub(super) unsafe fn attach(
        hwnd: *mut c_void,
        commands: Sender<MediaControlEvent>,
    ) -> Result<Self> {
        let hwnd = HWND(hwnd);
        // SAFETY: This constant is a valid, terminated UTF-16 message name.
        let created_message = unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) };
        if created_message == 0 {
            return Err(Error::from_thread());
        }
        let state = Rc::new(State {
            hwnd,
            created_message,
            attached: Cell::new(true),
            playback: Cell::default(),
            taskbar: RefCell::default(),
            icons: RefCell::new(Icons::new(hwnd)?),
            commands,
        });
        // One strong reference belongs to the subclass until removal or
        // WM_NCDESTROY, whichever comes first.
        let callback_state = Rc::into_raw(state.clone());
        // SAFETY: The window belongs to this thread. callback_state stays alive
        // until the subclass is removed, and the callback uses the system ABI.
        if !unsafe {
            SetWindowSubclass(
                hwnd,
                Some(window_proc),
                SUBCLASS_ID,
                callback_state as usize,
            )
        }
        .as_bool()
        {
            // SAFETY: Registration failed, so Windows never took this reference.
            drop(unsafe { Rc::from_raw(callback_state) });
            return Err(Error::from_thread());
        }
        Ok(Self(state))
    }

    pub(super) fn sync(&self, has_track: bool, playing: bool) {
        let playback = Playback { has_track, playing };
        if self.0.playback.replace(playback) != playback && self.0.attached.get() {
            self.0.update();
        }
    }
}

impl Drop for TaskbarControls {
    fn drop(&mut self) {
        if !self.0.attached.get() {
            return;
        }
        // SAFETY: This callback was installed on this thread. On failure its
        // reference stays alive for WM_NCDESTROY to release safely.
        if unsafe { RemoveWindowSubclass(self.0.hwnd, Some(window_proc), SUBCLASS_ID) }.as_bool() {
            self.0.attached.set(false);
            // SAFETY: Successful removal returns the subclass's one reference.
            drop(unsafe { Rc::from_raw(Rc::as_ptr(&self.0)) });
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    let pointer = data as *const State;
    // SAFETY: SetWindowSubclass holds one strong reference. Take our own so
    // reentrant COM/window messages cannot free state underneath this callback.
    let state = unsafe {
        Rc::increment_strong_count(pointer);
        Rc::from_raw(pointer)
    };
    if message == WM_NCDESTROY {
        // SAFETY: WM_NCDESTROY runs on the owning thread before HWND disposal.
        unsafe { RemoveWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID) };
        if state.attached.replace(false) {
            // SAFETY: Release the reference transferred to this subclass once.
            drop(unsafe { Rc::from_raw(pointer) });
        }
    } else if message == state.created_message {
        if let Err(error) = state.register() {
            log::warn!("could not create taskbar controls: {error}");
        }
    } else if message == WM_COMMAND
        && let Some(command) = command(wparam)
    {
        if state.playback.get().has_track {
            let _ = state.commands.try_send(command);
        }
        return LRESULT(0);
    }
    // SAFETY: Forward all other messages to the next procedure in the chain.
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    if message == WM_DPICHANGED && state.attached.get() {
        // GPUI has applied the DPI change before the icons are rebuilt.
        match Icons::new(hwnd) {
            Ok(icons) => {
                state.icons.replace(icons);
                state.update();
            }
            Err(error) => log::warn!("could not resize taskbar icons: {error}"),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::windows::Win32::{
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, HWND_MESSAGE, SendMessageW, WS_POPUP,
        },
    };

    struct Apartment;

    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: The test paired this reference with CoInitializeEx.
            unsafe { CoUninitialize() };
        }
    }

    struct TestWindow(HWND);

    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: The test owns this message-only window on this thread.
            unsafe { DestroyWindow(self.0) }.expect("destroy test window");
        }
    }

    fn click(hwnd: HWND, id: u32, notification: u32) {
        // SAFETY: The test window and its subclass both belong to this thread.
        unsafe {
            SendMessageW(
                hwnd,
                WM_COMMAND,
                Some(WPARAM(((notification as usize) << 16) | id as usize)),
                None,
            );
        }
    }

    #[test]
    fn native_buttons_route_once_and_release_their_window_callback() {
        // SAFETY: This test initializes and releases COM on its own thread.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .unwrap();
        let _apartment = Apartment;
        for close_window_first in [false, true] {
            // SAFETY: STATIC is a built-in class; this hidden message-only
            // window is created, used, and destroyed entirely on this thread.
            let window = TestWindow(
                unsafe {
                    CreateWindowExW(
                        Default::default(),
                        w!("STATIC"),
                        w!("Windence toolbar test"),
                        WS_POPUP,
                        0,
                        0,
                        64,
                        64,
                        Some(HWND_MESSAGE),
                        None,
                        None,
                        None,
                    )
                }
                .unwrap(),
            );
            let (commands, incoming) = async_channel::unbounded();
            // SAFETY: The test owns this live window and initialized COM above.
            let toolbar = unsafe { TaskbarControls::attach(window.0.0, commands) }.unwrap();
            assert!(
                toolbar
                    .0
                    .buttons()
                    .iter()
                    .all(|button| button.dwFlags == THBF_DISABLED)
            );
            click(window.0, TOGGLE, THBN_CLICKED);
            assert!(incoming.try_recv().is_err(), "no commands without a track");
            toolbar.sync(true, true);
            let playing = toolbar.0.buttons();
            assert_eq!(playing[1].hIcon, toolbar.0.icons.borrow().pause.0);
            assert_eq!(playing[1].dwFlags, THBF_ENABLED);
            toolbar.sync(true, false);
            assert_eq!(
                toolbar.0.buttons()[1].hIcon,
                toolbar.0.icons.borrow().play.0
            );
            click(window.0, TOGGLE, 0);
            click(window.0, 99, THBN_CLICKED);
            assert!(incoming.try_recv().is_err(), "ignore unrelated WM_COMMAND");
            for id in [PREVIOUS, TOGGLE, NEXT] {
                click(window.0, id, THBN_CLICKED);
            }
            assert!(matches!(
                incoming.try_recv(),
                Ok(MediaControlEvent::Previous)
            ));
            assert!(matches!(incoming.try_recv(), Ok(MediaControlEvent::Toggle)));
            assert!(matches!(incoming.try_recv(), Ok(MediaControlEvent::Next)));
            assert!(incoming.try_recv().is_err());
            if close_window_first {
                drop(window);
                assert!(!toolbar.0.attached.get());
                assert_eq!(Rc::strong_count(&toolbar.0), 1);
                drop(toolbar);
            } else {
                drop(toolbar);
                click(window.0, TOGGLE, THBN_CLICKED);
                drop(window);
            }
            assert!(incoming.is_closed(), "the subclass must release its sender");
            assert!(incoming.try_recv().is_err());
        }
    }
}
