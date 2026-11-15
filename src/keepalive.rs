//! Garde la capture WGC (Windows Graphics Capture) vivante sur un écran immobile.
//!
//! WGC n'envoie une image que quand Windows redessine l'écran. Bureau immobile, écran de chargement figé :
//! plus rien n'arrive, le buffer s'arrête et le chien de garde relançait ffmpeg toutes les 6 s (mesuré :
//! 1 440 relances en 2 h 30 de PC inactif). Desktop Duplication, lui, répétait la dernière image.
//!
//! Remède : un point d'un pixel dans le coin de l'écran capturé, dont l'opacité alterne entre 1/255 et
//! 2/255 (invisible : au plus un niveau de gris d'écart). Chaque changement fait redessiner l'écran par
//! Windows, donc WGC livre une image ; le filtre `fps` de ffmpeg complète jusqu'au débit du clip.
//! Caché pendant un jeu plein écran : le jeu redessine de lui-même, et une fenêtre par-dessus empêcherait
//! l'affichage direct du jeu (plus de latence). Sauf si plus rien n'arrive depuis 2 s (écran de chargement
//! figé) : montré le temps que les images reviennent. Ne touche à aucun autre processus.
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Qui a besoin du point : le buffer, ou l'essai de WGC au lancement (3 images à obtenir, même sur un écran
/// immobile). Écran capturé (HMONITOR) de chacun ; aucun : point retiré.
#[derive(Clone, Copy)]
pub enum Owner {
    Buffer = 0,
    Probe = 1,
}
static TARGET: Mutex<[Option<usize>; 2]> = Mutex::new([None; 2]);
static STARTED: AtomicBool = AtomicBool::new(false);
/// Plus aucune image depuis 2 s (chargement figé d'un jeu plein écran) : point montré même par-dessus le jeu.
static FORCE: AtomicBool = AtomicBool::new(false);

/// Changements par seconde : assez pour que `fps` sorte les images au fil de l'eau (au plus 50 ms de retard
/// sur un écran immobile), bien moins que le débit du clip.
const HZ: u64 = 20;

/// Vrai : la capture ne reçoit plus d'images (vu par le chien de garde) ; faux : elles arrivent de nouveau.
pub fn force(on: bool) {
    FORCE.store(on, Ordering::Relaxed);
}

/// Capture WGC de `owner` lancée sur l'écran `monitor` (Some) ou arrêtée (None).
pub fn set(owner: Owner, monitor: Option<usize>) {
    TARGET.lock().unwrap()[owner as usize] = monitor;
    if monitor.is_some() && !STARTED.swap(true, Ordering::Relaxed) {
        std::thread::spawn(|| unsafe { run() });
    }
}

unsafe extern "system" fn wndproc(h: windows_sys::Win32::Foundation::HWND, m: u32, w: usize, l: isize) -> isize {
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::DefWindowProcW(h, m, w, l) }
}

unsafe fn run() {
    use windows_sys::Win32::Graphics::Gdi::{BLACK_BRUSH, GetMonitorInfoW, GetStockObject, MONITORINFO};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        let cls: Vec<u16> = "ClipperKeepAlive".encode_utf16().chain(Some(0)).collect();
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.lpszClassName = cls.as_ptr();
        wc.hbrBackground = GetStockObject(BLACK_BRUSH) as _;
        RegisterClassW(&wc);
        // LAYERED + TRANSPARENT : les clics passent au travers. TOOLWINDOW : absent d'Alt+Tab. NOACTIVATE : jamais le
        // focus. Sans titre : n'apparaît pas dans les listes de fenêtres (partage d'écran de Discord, OBS).
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let h = CreateWindowExW(ex, cls.as_ptr(), std::ptr::null(), WS_POPUP, 0, 0, 1, 1, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null());
        if h.is_null() {
            crate::log("capture : point de maintien WGC impossible à créer");
            STARTED.store(false, Ordering::Relaxed);
            return;
        }
        let (mut shown, mut alpha, mut game, mut checked) = (None::<usize>, 1u8, false, Instant::now() - Duration::from_secs(5));
        loop {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                DispatchMessageW(&msg);
            }
            // Jeu plein écran au premier plan : relu 4 fois par seconde (lecture de la taille d'une fenêtre). Après
            // un Alt+Tab vers un bureau immobile, le point revient vite : pas de relance de la capture pour rien.
            if checked.elapsed() >= Duration::from_millis(250) {
                game = crate::foreground::fullscreen_foreground();
                checked = Instant::now();
            }
            let want = TARGET.lock().unwrap().iter().find_map(|m| *m).filter(|_| !game || FORCE.load(Ordering::Relaxed));
            if want != shown {
                match want {
                    Some(m) => {
                        let mut mi: MONITORINFO = std::mem::zeroed();
                        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                        let (x, y) = if GetMonitorInfoW(m as _, &mut mi) != 0 { (mi.rcMonitor.left, mi.rcMonitor.top) } else { (0, 0) };
                        SetWindowPos(h, HWND_TOPMOST, x, y, 1, 1, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                    }
                    None => {
                        ShowWindow(h, SW_HIDE);
                    }
                }
                shown = want;
            }
            if shown.is_some() {
                alpha = 3 - alpha; // 1 <-> 2
                SetLayeredWindowAttributes(h, 0, alpha, LWA_ALPHA);
            }
            std::thread::sleep(Duration::from_millis(if shown.is_some() { 1000 / HZ } else { 250 }));
        }
    }
}
