//! Overlay de statistiques : fenêtre Win32 native (pas de WebView, ~2 Mo), transparente aux clics,
//! toujours au premier plan, exclue des captures. Rien n'est injecté dans le jeu.
//! FPS : événements ETW publics « Present » de DXGI (comme PresentMon). Demande les droits admin
//! ou le groupe « Utilisateurs du journal de performances » ; sinon la ligne FPS affiche « droits ».
use crate::config::Config;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::collections::VecDeque;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone)]
struct Look {
    items: Vec<String>,
    corner: String,
    size: String,
    opacity: f32,
}

static LOOK: Mutex<Option<Look>> = Mutex::new(None);
static VISIBLE: AtomicBool = AtomicBool::new(false);
static TEXT: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// PID du jeu au premier plan : seuls ses « Present » sont comptés.
static TARGET: AtomicU32 = AtomicU32::new(0);
/// Horodatages QPC des derniers « Present » du jeu (1 s max).
static PRESENTS: Mutex<VecDeque<i64>> = Mutex::new(VecDeque::new());
static ETW_OK: AtomicBool = AtomicBool::new(false);

/// Applique les réglages et l'état visible (mode "always", ou raccourci en mode "hotkey").
pub fn apply(cfg: &Config, shown: bool) {
    *LOOK.lock().unwrap() = Some(Look {
        items: cfg.overlay_items.clone(),
        corner: cfg.overlay_corner.clone(),
        size: cfg.overlay_size.clone(),
        opacity: cfg.overlay_opacity.clamp(0.2, 1.0),
    });
    let v = cfg.overlay_mode == "always" || (cfg.overlay_mode == "hotkey" && shown);
    VISIBLE.store(v, Ordering::Relaxed);
}

/// Lancé une fois : le fil ne fait rien tant que l'overlay est masqué (un réveil par seconde).
pub fn start() {
    std::thread::spawn(|| unsafe { window_thread() });
    std::thread::spawn(etw_thread);
}

// ---------- Statistiques ----------

struct Cpu {
    last: (u64, u64),
}

impl Cpu {
    fn sample(&mut self) -> f32 {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::GetSystemTimes;
        let (mut i, mut k, mut u): (FILETIME, FILETIME, FILETIME) = unsafe { std::mem::zeroed() };
        unsafe { GetSystemTimes(&mut i, &mut k, &mut u) };
        let f = |t: FILETIME| (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64;
        let (idle, total) = (f(i), f(k) + f(u)); // kernel inclut idle
        let (di, dt) = (idle - self.last.0, total - self.last.1);
        self.last = (idle, total);
        if dt == 0 { 0.0 } else { 100.0 * (1.0 - di as f32 / dt as f32) }
    }
}

/// NVML (nvml.dll fourni par le pilote NVIDIA), chargé dans NOTRE processus. Absent : lignes GPU vides.
struct Nvml {
    dev: *mut core::ffi::c_void,
    util: unsafe extern "C" fn(*mut core::ffi::c_void, *mut [u32; 2]) -> i32,
    temp: unsafe extern "C" fn(*mut core::ffi::c_void, u32, *mut u32) -> i32,
    mem: unsafe extern "C" fn(*mut core::ffi::c_void, *mut [u64; 3]) -> i32,
}

impl Nvml {
    fn load() -> Option<Self> {
        use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        let name: Vec<u16> = "nvml.dll".encode_utf16().chain(Some(0)).collect();
        unsafe {
            let lib = LoadLibraryW(name.as_ptr());
            if lib.is_null() {
                return None;
            }
            let sym = |s: &[u8]| GetProcAddress(lib, s.as_ptr());
            let init: unsafe extern "C" fn() -> i32 = std::mem::transmute(sym(b"nvmlInit_v2\0")?);
            let by_index: unsafe extern "C" fn(u32, *mut *mut core::ffi::c_void) -> i32 = std::mem::transmute(sym(b"nvmlDeviceGetHandleByIndex_v2\0")?);
            if init() != 0 {
                return None;
            }
            let mut dev = std::ptr::null_mut();
            if by_index(0, &mut dev) != 0 {
                return None;
            }
            Some(Self {
                dev,
                util: std::mem::transmute(sym(b"nvmlDeviceGetUtilizationRates\0")?),
                temp: std::mem::transmute(sym(b"nvmlDeviceGetTemperature\0")?),
                mem: std::mem::transmute(sym(b"nvmlDeviceGetMemoryInfo\0")?),
            })
        }
    }
}

/// Température CPU : compteur Windows « Thermal Zone Information » (capteur ACPI de la carte mère).
/// Sans pilote ni droits admin. Sur certains PC il est absent ou approximatif : la ligne affiche alors « n/d ».
struct Thermal {
    query: *mut core::ffi::c_void,
    counter: *mut core::ffi::c_void,
}

impl Thermal {
    fn open() -> Option<Self> {
        use windows_sys::Win32::System::Performance::*;
        let path: Vec<u16> = r"\Thermal Zone Information(*)\Temperature".encode_utf16().chain(Some(0)).collect();
        unsafe {
            let (mut q, mut c) = (std::ptr::null_mut(), std::ptr::null_mut());
            if PdhOpenQueryW(std::ptr::null(), 0, &mut q) != 0 {
                return None;
            }
            if PdhAddEnglishCounterW(q, path.as_ptr(), 0, &mut c) != 0 {
                PdhCloseQuery(q);
                return None;
            }
            Some(Self { query: q, counter: c })
        }
    }

    /// Zone la plus chaude, en °C.
    fn celsius(&self) -> Option<f64> {
        use windows_sys::Win32::System::Performance::*;
        unsafe {
            PdhCollectQueryData(self.query);
            let (mut size, mut n) = (0u32, 0u32);
            PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut n, std::ptr::null_mut());
            if size == 0 {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut n, items) != 0 {
                return None;
            }
            // 373 K pile = valeur par défaut de certains BIOS (pas une mesure) : écartée, comme 0.
            let k = (0..n as usize).map(|i| (*items.add(i)).FmtValue.Anonymous.doubleValue).filter(|&k| k != 373.0 && k > 274.0).fold(f64::MIN, f64::max);
            let c = k - 273.15;
            (1.0..=110.0).contains(&c).then_some(c)
        }
    }
}

fn fps_text() -> (String, String) {
    if !ETW_OK.load(Ordering::Relaxed) {
        return ("—".into(), "droits".into());
    }
    let mut freq = 0i64;
    let mut now = 0i64;
    unsafe {
        windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut freq);
        windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut now);
    }
    let mut q = PRESENTS.lock().unwrap();
    while q.front().is_some_and(|&t| now - t > freq) {
        q.pop_front();
    }
    let n = q.len();
    if n < 2 {
        return ("0".into(), "—".into());
    }
    let span = (q[n - 1] - q[0]) as f64 / freq as f64;
    let ft = span * 1000.0 / (n - 1) as f64;
    (format!("{n}"), format!("{ft:.1} ms").replace('.', ","))
}

fn foreground_pid() -> u32 {
    let mut pid = 0;
    // Propriétaire de la fenêtre au premier plan : simple lecture, aucun handle sur le jeu.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
    pid
}

fn refresh(cpu: &mut Cpu, nv: &Option<Nvml>, th: &Option<Thermal>) {
    let items = LOOK.lock().unwrap().as_ref().map(|l| l.items.clone()).unwrap_or_default();
    TARGET.store(foreground_pid(), Ordering::Relaxed);
    let c = cpu.sample();
    let (mut gu, mut gt, mut gm) = (None, None, None);
    if let Some(n) = nv {
        unsafe {
            let mut u = [0u32; 2];
            if (n.util)(n.dev, &mut u) == 0 {
                gu = Some(u[0]);
            }
            let mut t = 0u32;
            if (n.temp)(n.dev, 0, &mut t) == 0 {
                gt = Some(t);
            }
            let mut m = [0u64; 3];
            if (n.mem)(n.dev, &mut m) == 0 {
                gm = Some((m[2], m[0]));
            }
        }
    }
    let (free, total) = crate::tools::ram();
    // Même unité que le Gestionnaire des tâches (1 Go = 1024³ octets).
    let go = |b: u64| format!("{:.1}", b as f64 / 1_073_741_824.0).replace('.', ",");
    let (fps, ft) = fps_text();
    let rows = items
        .iter()
        .filter_map(|k| {
            Some(match k.as_str() {
                "fps" => ("FPS".into(), fps.clone()),
                "frametime" => ("IMAGE".into(), ft.clone()),
                "cpu" => ("CPU".into(), format!("{c:.0} %")),
                "gpu" => ("GPU".into(), gu.map_or("—".into(), |v| format!("{v} %"))),
                "gpu_temp" => ("GPU °C".into(), gt.map_or("—".into(), |v| v.to_string())),
                "cpu_temp" => ("CPU °C".into(), th.as_ref().and_then(Thermal::celsius).map_or("n/d".into(), |t| format!("{t:.0}"))),
                "vram" => ("VRAM".into(), gm.map_or("—".into(), |(u, t)| format!("{}/{} Go", go(u), go(t)))),
                "ram" => ("RAM".into(), format!("{}/{} Go", go(total - free), go(total))),
                _ => return None,
            })
        })
        .collect();
    *TEXT.lock().unwrap() = rows;
}

// ---------- Fenêtre ----------

fn font_px(size: &str) -> i32 {
    match size {
        "s" => 13,
        "l" => 20,
        _ => 16,
    }
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_PAINT {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(h, &mut ps);
            let mut r: RECT = std::mem::zeroed();
            GetClientRect(h, &mut r);
            let bg = CreateSolidBrush(0x0013_1914); // fond vert-de-gris (DA E), en BGR
            FillRect(dc, &r, bg);
            DeleteObject(bg as _);
            let px = font_px(&LOOK.lock().unwrap().as_ref().map(|l| l.size.clone()).unwrap_or_default());
            let font = make_font(px);
            let old = SelectObject(dc, font as _);
            SetBkMode(dc, TRANSPARENT as i32);
            let pad = px / 2;
            let row = px + 6;
            for (i, (k, v)) in TEXT.lock().unwrap().iter().enumerate() {
                let y = pad + i as i32 * row;
                let draw = |s: &str, x: i32, right: bool, color: u32| {
                    let t: Vec<u16> = s.encode_utf16().collect();
                    SetTextColor(dc, color);
                    let mut rr = RECT { left: x, top: y, right: r.right - pad, bottom: y + row };
                    DrawTextW(dc, t.as_ptr(), t.len() as i32, &mut rr, if right { DT_RIGHT } else { DT_LEFT } | DT_SINGLELINE | DT_NOCLIP);
                };
                draw(k, pad, false, 0x0086_978D); // gris-vert
                draw(v, pad, true, if i == 0 && k == "FPS" { 0x002E_7DFF } else { 0x00E2_ECE9 }); // orange signal / texte
            }
            SelectObject(dc, old);
            DeleteObject(font as _);
            EndPaint(h, &ps);
            return 0;
        }
        DefWindowProcW(h, msg, w, l)
    }
}

/// Police à chiffres fixes : les colonnes restent alignées quand les valeurs changent.
unsafe fn make_font(px: i32) -> HFONT {
    let face: Vec<u16> = "Consolas".encode_utf16().chain(Some(0)).collect();
    unsafe { CreateFontW(-px, 0, 0, 0, 600, 0, 0, 0, 0, 0, 0, CLEARTYPE_QUALITY as u32, 0, face.as_ptr()) }
}

/// Largeur = libellé le plus long + écart + valeur la plus longue, mesurés avec la vraie police.
fn measure(h: HWND, px: i32) -> i32 {
    unsafe {
        let dc = GetDC(h);
        let font = make_font(px);
        let old = SelectObject(dc, font as _);
        let w = |s: &str| {
            let t: Vec<u16> = s.encode_utf16().collect();
            let mut sz = std::mem::zeroed();
            GetTextExtentPoint32W(dc, t.as_ptr(), t.len() as i32, &mut sz);
            sz.cx
        };
        let rows = TEXT.lock().unwrap();
        let (kl, vl) = rows.iter().fold((0, 0), |(a, b), (k, v)| (a.max(w(k)), b.max(w(v))));
        drop(rows);
        SelectObject(dc, old);
        DeleteObject(font as _);
        ReleaseDC(h, dc);
        px / 2 * 2 + kl + px + vl
    }
}

/// Coin choisi de l'écran du jeu.
fn place(h: HWND, rows: usize, look: &Look) {
    unsafe {
        let px = font_px(&look.size);
        let (w, hgt) = (measure(h, px), px / 2 * 2 + rows as i32 * (px + 6));
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY), &mut mi);
        let m = mi.rcMonitor;
        let margin = 12;
        let x = if look.corner.ends_with('r') { m.right - w - margin } else { m.left + margin };
        let y = if look.corner.starts_with('b') { m.bottom - hgt - margin } else { m.top + margin };
        SetWindowPos(h, HWND_TOPMOST, x, y, w, hgt, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        SetLayeredWindowAttributes(h, 0, (look.opacity * 255.0) as u8, LWA_ALPHA);
    }
}

unsafe fn window_thread() {
    unsafe {
        let cls: Vec<u16> = "ClipperStats".encode_utf16().chain(Some(0)).collect();
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.lpszClassName = cls.as_ptr();
        if RegisterClassW(&wc) == 0 {
            crate::log(&format!("overlay : RegisterClassW a échoué ({})", windows_sys::Win32::Foundation::GetLastError()));
        }
        // LAYERED + TRANSPARENT : les clics passent au jeu. NOACTIVATE : jamais le focus.
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let h = CreateWindowExW(ex, cls.as_ptr(), cls.as_ptr(), WS_POPUP, 0, 0, 160, 80, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null());
        if h.is_null() {
            crate::log(&format!("overlay : CreateWindowExW a échoué ({})", windows_sys::Win32::Foundation::GetLastError()));
            return;
        }
        // Jamais dans les clips ni les REC (ni dans les captures d'écran).
        SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
        let (mut cpu, nv, th) = (Cpu { last: (0, 0) }, Nvml::load(), Thermal::open());
        cpu.sample();
        let mut shown = false;
        loop {
            let v = VISIBLE.load(Ordering::Relaxed);
            if v {
                refresh(&mut cpu, &nv, &th);
                let look = LOOK.lock().unwrap().clone();
                if let Some(look) = look {
                    place(h, TEXT.lock().unwrap().len(), &look);
                }
                InvalidateRect(h, std::ptr::null(), 1);
                shown = true;
            } else if shown {
                ShowWindow(h, SW_HIDE);
                shown = false;
            }
            // Messages en attente (peinture), puis pause : 2 rafraîchissements/s visible, 1 réveil/s masqué.
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                DispatchMessageW(&msg);
            }
            std::thread::sleep(std::time::Duration::from_millis(if v { 500 } else { 1000 }));
        }
    }
}

// ---------- ETW : événements Present de DXGI ----------

/// Microsoft-Windows-DXGI. Événement 42 = Present_Start (un par image affichée par le jeu).
const DXGI: windows_sys::core::GUID = windows_sys::core::GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);

unsafe extern "system" fn on_event(r: *mut windows_sys::Win32::System::Diagnostics::Etw::EVENT_RECORD) {
    let h = unsafe { &(*r).EventHeader };
    if h.EventDescriptor.Id == 42 && h.ProcessId == TARGET.load(Ordering::Relaxed) {
        let mut q = PRESENTS.lock().unwrap();
        q.push_back(h.TimeStamp);
        if q.len() > 1000 {
            q.pop_front();
        }
    }
}

/// La session ETW ne tourne que quand l'overlay est visible ET que FPS / temps d'image sont choisis.
fn etw_thread() {
    use windows_sys::Win32::System::Diagnostics::Etw::*;
    let name: Vec<u16> = "ClipperFps".encode_utf16().chain(Some(0)).collect();
    loop {
        let want = VISIBLE.load(Ordering::Relaxed)
            && LOOK.lock().unwrap().as_ref().is_some_and(|l| l.items.iter().any(|i| i == "fps" || i == "frametime"));
        if !want {
            std::thread::sleep(std::time::Duration::from_secs(1));
            continue;
        }
        unsafe {
            // Propriétés + nom de session à la suite, comme l'exige l'API.
            let size = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + name.len() * 2;
            let mut buf = vec![0u8; size];
            let props = buf.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
            (*props).Wnode.BufferSize = size as u32;
            (*props).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            (*props).Wnode.ClientContext = 1; // horodatage QPC
            (*props).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
            (*props).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
            let mut handle = CONTROLTRACE_HANDLE { Value: 0 };
            // Session restée d'un ancien lancement : on l'arrête d'abord.
            ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, name.as_ptr(), props, EVENT_TRACE_CONTROL_STOP);
            (*props).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
            (*props).Wnode.BufferSize = size as u32;
            (*props).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
            if StartTraceW(&mut handle, name.as_ptr(), props) != 0 {
                ETW_OK.store(false, Ordering::Relaxed); // pas les droits : la ligne FPS le dit
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
            EnableTraceEx2(handle, &DXGI, EVENT_CONTROL_CODE_ENABLE_PROVIDER, TRACE_LEVEL_INFORMATION as u8, 0, 0, 0, std::ptr::null());
            let mut log: EVENT_TRACE_LOGFILEW = std::mem::zeroed();
            log.LoggerName = name.as_ptr() as *mut u16;
            log.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
            log.Anonymous2.EventRecordCallback = Some(on_event);
            let th = OpenTraceW(&mut log);
            ETW_OK.store(true, Ordering::Relaxed);
            // Arrêt de la session quand l'overlay est masqué : ProcessTrace rend alors la main.
            let stop_name = name.clone();
            let watcher = std::thread::spawn(move || {
                while VISIBLE.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                let mut b = vec![0u8; size];
                let p = b.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
                (*p).Wnode.BufferSize = size as u32;
                (*p).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
                ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, stop_name.as_ptr(), p, EVENT_TRACE_CONTROL_STOP);
            });
            ProcessTrace(&th, 1, std::ptr::null(), std::ptr::null());
            CloseTrace(th);
            let _ = watcher.join();
            PRESENTS.lock().unwrap().clear();
        }
    }
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore = "lit le GPU"]
    fn stats_without_window() {
        let mut cfg = crate::config::Config::default();
        cfg.overlay_items = ["cpu", "gpu", "gpu_temp", "vram", "ram", "cpu_temp"].map(String::from).to_vec();
        super::apply(&cfg, false);
        let (mut cpu, nv, th) = (super::Cpu { last: (0, 0) }, super::Nvml::load(), super::Thermal::open());
        cpu.sample();
        std::thread::sleep(std::time::Duration::from_millis(500));
        super::refresh(&mut cpu, &nv, &th);
        println!("NVML chargé : {}", nv.is_some());
        for (k, v) in super::TEXT.lock().unwrap().iter() {
            println!("{k:<7} {v}");
        }
    }
}
