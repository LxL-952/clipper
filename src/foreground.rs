//! Ce qui est au premier plan : jeu plein écran, bureau sécurisé (verrouillage, UAC). Lu par la capture,
//! le point de maintien WGC, l'overlay, l'icône de notification et la fenêtre. Seulement des tailles et des
//! noms de fenêtres : rien n'est ouvert dans le processus du jeu.

/// Écran de verrouillage, ouverture de session ou fenêtre d'autorisation (UAC) affichés ? Windows montre alors
/// un bureau sécurisé que la capture ne voit pas : ddagrab n'envoie plus rien, sans message d'erreur.
/// Lu sur le bureau qui reçoit le clavier : « Default » sinon, ou inaccessible (bureau Winlogon).
pub fn secure_desktop() -> bool {
    use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, UOI_NAME};
    unsafe {
        let h = OpenInputDesktop(0, 0, 0);
        if h.is_null() {
            return true;
        }
        let mut name = [0u16; 64];
        let ok = GetUserObjectInformationW(h, UOI_NAME, name.as_mut_ptr().cast(), std::mem::size_of_val(&name) as u32, std::ptr::null_mut()) != 0;
        CloseDesktop(h);
        let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        ok && !String::from_utf16_lossy(&name[..len]).eq_ignore_ascii_case("Default")
    }
}

/// Vrai si la fenêtre au premier plan couvre tout son écran (jeu plein écran ou fenêtré sans bordure).
/// Lit seulement la taille de la fenêtre : rien n'est ouvert dans le processus du jeu.
pub fn fullscreen_foreground() -> bool {
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetShellWindow, GetWindowRect};
    unsafe {
        let w = GetForegroundWindow();
        if w.is_null() || w == GetShellWindow() {
            return false;
        }
        let mut r = std::mem::zeroed();
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetWindowRect(w, &mut r) == 0 || GetMonitorInfoW(MonitorFromWindow(w, MONITOR_DEFAULTTONEAREST), &mut mi) == 0 {
            return false;
        }
        let m = mi.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom && !is_desktop(w)
    }
}

/// Le bureau (Progman / WorkerW) couvre aussi l'écran : il ne compte pas comme un jeu.
fn is_desktop(w: windows_sys::Win32::Foundation::HWND) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 32];
    let n = unsafe { GetClassNameW(w, buf.as_mut_ptr(), 32) } as usize;
    let c = String::from_utf16_lossy(&buf[..n]);
    c == "Progman" || c == "WorkerW"
}
