//! Fenêtres natives de l'interface : une fenêtre tao, un tampon de dessin (gfx::Canvas), les entrées
//! traduites pour les contrôles, et l'affichage du tampon tel quel par Windows (aucun moteur web).
//! Fenêtre principale : opaque, SetDIBitsToDevice. Barre Alt+Z : fenêtre à transparence par pixel
//! (UpdateLayeredWindow) ; ses zones vides laissent passer les clics vers le jeu.
use crate::gfx::Canvas;
use crate::widgets::{Input, UiState};
use std::time::Instant;
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use tao::event_loop::EventLoopWindowTarget;
use tao::platform::windows::WindowExtWindows;
use tao::window::{CursorIcon, Window, WindowBuilder};

pub const BAR_W: f64 = 1040.0;
pub const BAR_H: f64 = 760.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Main,
    Bar,
}

pub struct Win {
    pub window: Window,
    pub kind: Kind,
    pub canvas: Canvas,
    pub input: Input,
    pub st: UiState,
    last_click: Option<(Instant, (f32, f32))>,
    hand: bool,
    /// Barre : image GDI gardée d'une image à l'autre (même taille). Avant, une image de ~5 Mo était créée
    /// puis détruite à chaque dessin, 60 fois par seconde pendant la lecture d'un clip.
    layered: Option<Layered>,
}

/// DC mémoire + DIB de la barre, à la taille du tampon.
struct Layered {
    mem: windows_sys::Win32::Graphics::Gdi::HDC,
    bmp: windows_sys::Win32::Graphics::Gdi::HBITMAP,
    old: windows_sys::Win32::Graphics::Gdi::HGDIOBJ,
    bits: *mut u8,
    size: (i32, i32),
}

impl Drop for Layered {
    fn drop(&mut self) {
        use windows_sys::Win32::Graphics::Gdi::{DeleteDC, DeleteObject, SelectObject};
        unsafe {
            SelectObject(self.mem, self.old);
            DeleteObject(self.bmp as _);
            DeleteDC(self.mem);
        }
    }
}

impl Win {
    pub fn scale(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    /// Tampon recréé à la taille de la fenêtre (redimensionnement, changement d'écran).
    pub fn fit(&mut self) {
        let sz = self.window.inner_size();
        let s = self.scale();
        if (sz.width.max(1), sz.height.max(1)) != (self.canvas.pm.width(), self.canvas.pm.height()) || self.canvas.s != s {
            self.canvas = Canvas::new(sz.width, sz.height, s);
        }
    }

    /// Libère le tampon (fenêtre cachée) : il sera recréé par `fit` au prochain affichage.
    pub fn release(&mut self) {
        self.canvas = Canvas::new(1, 1, 1.0);
        self.layered = None;
    }

    /// Traduit un événement de fenêtre. Vrai : l'interface doit être redessinée.
    pub fn event(&mut self, ev: &WindowEvent) -> bool {
        let s = self.scale();
        match ev {
            WindowEvent::CursorMoved { position, .. } => {
                self.input.mouse = (position.x as f32 / s, position.y as f32 / s);
                true
            }
            WindowEvent::CursorLeft { .. } => {
                self.input.mouse = (-1.0, -1.0);
                true
            }
            WindowEvent::MouseInput { state, button, .. } => {
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => {
                        self.input.down = true;
                        self.input.pressed = true;
                        let now = Instant::now();
                        let m = self.input.mouse;
                        self.input.dbl = self.last_click.is_some_and(|(t, p)| now.duration_since(t).as_millis() < 400 && (p.0 - m.0).abs() < 4.0 && (p.1 - m.1).abs() < 4.0);
                        self.last_click = if self.input.dbl { None } else { Some((now, m)) };
                    }
                    (MouseButton::Left, ElementState::Released) => {
                        self.input.down = false;
                        self.input.released = true;
                    }
                    (MouseButton::Right, ElementState::Pressed) => self.input.rpressed = true,
                    _ => return false,
                }
                true
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.input.wheel += match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 100.0,
                    _ => 0.0,
                };
                true
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    self.input.keys.push(format!("{:?}", event.physical_key));
                    if let Some(t) = event.text {
                        if !self.input.ctrl && !self.input.alt {
                            self.input.text.push_str(t);
                        }
                    }
                    return true;
                }
                false
            }
            WindowEvent::ModifiersChanged(m) => {
                (self.input.ctrl, self.input.alt, self.input.shift) = (m.control_key(), m.alt_key(), m.shift_key());
                false
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => true,
            WindowEvent::Focused(on) => {
                if !on {
                    self.input.down = false;
                    (self.input.ctrl, self.input.alt, self.input.shift) = (false, false, false);
                }
                true
            }
            _ => false,
        }
    }

    /// Curseur « main » sur ce qui se clique.
    pub fn set_hand(&mut self, hand: bool) {
        if hand != self.hand {
            self.hand = hand;
            self.window.set_cursor_icon(if hand { CursorIcon::Hand } else { CursorIcon::Default });
        }
    }

    /// Affiche le tampon dans la fenêtre.
    pub fn present(&mut self) {
        use windows_sys::Win32::Graphics::Gdi::*;
        let (w, h) = (self.canvas.pm.width() as i32, self.canvas.pm.height() as i32);
        let hwnd = self.window.hwnd() as windows_sys::Win32::Foundation::HWND;
        let mut bi: BITMAPINFO = unsafe { std::mem::zeroed() };
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h; // lignes de haut en bas, comme le tampon
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        let data = self.canvas.pm.data();
        unsafe {
            if self.kind == Kind::Main {
                let dc = GetDC(hwnd);
                SetDIBitsToDevice(dc, 0, 0, w as u32, h as u32, 0, 0, 0, h as u32, data.as_ptr() as _, &bi, DIB_RGB_COLORS);
                ReleaseDC(hwnd, dc);
                return;
            }
            use windows_sys::Win32::Foundation::{POINT, SIZE};
            use windows_sys::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};
            let screen = GetDC(std::ptr::null_mut());
            if self.layered.as_ref().is_none_or(|l| l.size != (w, h)) {
                self.layered = None; // ancienne taille libérée d'abord
                let mem = CreateCompatibleDC(screen);
                let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
                let bmp = CreateDIBSection(mem, &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
                if bmp.is_null() || bits.is_null() {
                    DeleteDC(mem);
                } else {
                    let old = SelectObject(mem, bmp as _);
                    self.layered = Some(Layered { mem, bmp, old, bits: bits as *mut u8, size: (w, h) });
                }
            }
            if let Some(l) = &self.layered {
                std::ptr::copy_nonoverlapping(data.as_ptr(), l.bits, data.len());
                let size = SIZE { cx: w, cy: h };
                let src = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
                // tao réécrit le style étendu à chaque set_visible / set_focus… et retire WS_EX_LAYERED :
                // sans lui, UpdateLayeredWindow échoue (erreur 87) et la barre reste invisible.
                layered_tool_window(&self.window);
                UpdateLayeredWindow(hwnd, screen, std::ptr::null(), &size, l.mem, &src, 0, &blend, ULW_ALPHA);
            }
            ReleaseDC(std::ptr::null_mut(), screen);
        }
    }
}

pub fn open<T: 'static>(kind: Kind, target: &EventLoopWindowTarget<T>) -> Option<Win> {
    use tao::platform::windows::{IconExtWindows, WindowBuilderExtWindows};
    let mut wb = WindowBuilder::new().with_title("Clipper").with_decorations(false).with_window_icon(tao::window::Icon::from_resource(1, None).ok()).with_visible(false);
    wb = match kind {
        Kind::Main => wb.with_inner_size(LogicalSize::new(900.0, 620.0)).with_min_inner_size(LogicalSize::new(840.0, 520.0)),
        Kind::Bar => wb.with_inner_size(LogicalSize::new(BAR_W, BAR_H)).with_always_on_top(true).with_skip_taskbar(true).with_focused(false).with_resizable(false).with_undecorated_shadow(false),
    };
    let window = wb.build(target).ok()?;
    if kind == Kind::Bar {
        layered_tool_window(&window);
    }
    let sz = window.inner_size();
    let canvas = if kind == Kind::Main { Canvas::new(sz.width, sz.height, window.scale_factor() as f32) } else { Canvas::new(1, 1, 1.0) };
    Some(Win { window, kind, canvas, input: Input { mouse: (-1.0, -1.0), ..Default::default() }, st: UiState::default(), last_click: None, hand: false, layered: None })
}

/// WS_EX_TOOLWINDOW : ni barre des tâches ni Alt+Tab. WS_EX_LAYERED : transparence par pixel.
fn layered_tool_window(w: &Window) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_LAYERED, WS_EX_TOOLWINDOW};
    let h = w.hwnd() as _;
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        let want = ex | (WS_EX_TOOLWINDOW | WS_EX_LAYERED) as isize;
        if want != ex {
            SetWindowLongPtrW(h, GWL_EXSTYLE, want);
        }
    }
}

/// Haut-centre de l'écran du jeu (fenêtre au premier plan), en pixels réels :
/// juste même quand les deux écrans n'ont pas la même mise à l'échelle.
pub fn show_bar(w: &Window) {
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    let ok = unsafe { GetMonitorInfoW(MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY), &mut mi) } != 0;
    if ok {
        let m = mi.rcMonitor;
        // Taille recalculée pour l'écran du jeu (mise à l'échelle différente d'un écran à l'autre), limitée à l'écran.
        let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForWindow(GetForegroundWindow()) }.max(96) as f64 / 96.0;
        let (mw, mh) = ((m.right - m.left) as u32, (m.bottom - m.top) as u32);
        let size = PhysicalSize::new(((BAR_W * dpi) as u32).min(mw), ((BAR_H * dpi) as u32).min(mh));
        // Déplacée sur l'écran du jeu AVANT d'être dimensionnée : passer d'un écran à 125 % à un écran à 100 %
        // fait redimensionner la fenêtre par Windows (WM_DPICHANGED), la barre sortait tronquée des deux côtés.
        w.set_outer_position(PhysicalPosition::new(m.left, m.top));
        w.set_inner_size(size);
        w.set_outer_position(PhysicalPosition::new(m.left + (mw as i32 - size.width as i32) / 2, m.top));
    }
}

/// Affiche la barre déjà peinte. ShowWindow direct, pas `set_visible` de tao : tao réécrit le style
/// étendu en l'affichant, la fenêtre apparaissait un instant sans WS_EX_LAYERED (cadre et ombre de Windows).
/// Focus pris : le jeu rend la souris (curseur visible, plus bloqué), et Échap ferme la barre.
/// Le focus revient au jeu à la fermeture (main.rs, hide_bar).
pub fn reveal_bar(w: &Window) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_SHOW, SetForegroundWindow, ShowWindow};
    layered_tool_window(w);
    unsafe {
        ShowWindow(w.hwnd() as _, SW_SHOW);
        SetForegroundWindow(w.hwnd() as _);
    }
}

/// Cache la barre (pendant de `reveal_bar`, sans tao pour la même raison).
pub fn conceal_bar(w: &Window) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
    unsafe { ShowWindow(w.hwnd() as _, SW_HIDE) };
}
