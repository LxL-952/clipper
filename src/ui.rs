//! Fenêtres WebView créées à la demande et détruites à la fermeture (RAM rendue).
//! Contenu servi depuis l'exe par le protocole `clipper://localhost/…` (vu en `http://clipper.localhost/…`).
use include_dir::{Dir, include_dir};
use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use tao::dpi::LogicalSize;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};
use wry::http::{Request, Response, StatusCode, header};
use wry::{WebView, WebViewBuilder};

static UI: Dir = include_dir!("$CARGO_MANIFEST_DIR/ui");

/// Dossier de sortie actuel : les fenêtres restent ouvertes (la barre n'est jamais détruite), elles
/// doivent servir les vidéos du dossier choisi maintenant, pas de celui de leur création.
static OUT_DIR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

pub fn set_out_dir(p: &Path) {
    *OUT_DIR.lock().unwrap() = Some(p.to_path_buf());
}

const BAR_W: f64 = 1040.0;
const BAR_H: f64 = 760.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Main,
    Bar,
}

pub struct Win {
    pub window: Window,
    pub web: WebView,
}

impl Win {
    /// Rust -> JS : `window.__on(msg)`.
    pub fn push(&self, json: &str) {
        let _ = self.web.evaluate_script(&format!("window.__on&&window.__on({json})"));
    }
}

fn mime(p: &str) -> &'static str {
    match p.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "woff2" => "font/woff2",
        "jpg" => "image/jpeg",
        // MOV = même format que MP4 ; annoncé « quicktime », WebView2 refusait de le lire.
        "mp4" | "mov" => "video/mp4",
        "mkv" => "video/x-matroska",
        _ => "application/octet-stream",
    }
}

fn reply(code: StatusCode, ctype: &str, body: Vec<u8>) -> Response<Cow<'static, [u8]>> {
    Response::builder().status(code).header(header::CONTENT_TYPE, ctype).body(Cow::Owned(body)).unwrap()
}

/// Décodage %XX minimal pour le paramètre `p` des vidéos.
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Lecture par morceaux (requêtes Range) : une vidéo de 3 Go n'est jamais chargée en RAM.
fn serve_video(path: &Path, req: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    const CHUNK: u64 = 2 << 20;
    let Ok(mut f) = std::fs::File::open(path) else { return reply(StatusCode::NOT_FOUND, "text/plain", vec![]) };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let range = req.headers().get(header::RANGE).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("bytes="));
    let (start, end) = match range.and_then(|r| r.split_once('-')) {
        Some((a, b)) => {
            let a: u64 = a.parse().unwrap_or(0);
            let b: u64 = b.parse().unwrap_or(a + CHUNK - 1);
            (a, b.min(a + CHUNK - 1).min(len.saturating_sub(1)))
        }
        None => (0, (CHUNK - 1).min(len.saturating_sub(1))),
    };
    if start >= len {
        return reply(StatusCode::RANGE_NOT_SATISFIABLE, "text/plain", vec![]);
    }
    let mut buf = vec![0; (end - start + 1) as usize];
    if f.seek(SeekFrom::Start(start)).and_then(|_| f.read_exact(&mut buf)).is_err() {
        return reply(StatusCode::INTERNAL_SERVER_ERROR, "text/plain", vec![]);
    }
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_TYPE, mime(&path.to_string_lossy()))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
        .body(Cow::Owned(buf))
        .unwrap()
}

/// Sert l'UI embarquée, les miniatures et les vidéos du dossier de sortie (et rien d'autre).
fn protocol(req: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let out_dir = OUT_DIR.lock().unwrap().clone().unwrap_or_default();
    let out_dir = out_dir.as_path();
    let path = req.uri().path().trim_start_matches('/');
    if let Some(name) = path.strip_prefix("thumb/") {
        let name = pct_decode(name);
        if name.contains(['/', '\\']) || name.contains("..") {
            return reply(StatusCode::FORBIDDEN, "text/plain", vec![]);
        }
        return match std::fs::read(crate::config::dir().join("thumbs").join(name)) {
            Ok(b) => reply(StatusCode::OK, "image/jpeg", b),
            Err(_) => reply(StatusCode::NOT_FOUND, "text/plain", vec![]),
        };
    }
    if let Some(name) = path.strip_prefix("preview/") {
        let name = pct_decode(name);
        if name.contains(['/', '\\']) || name.contains("..") || !name.ends_with(".mp4") {
            return reply(StatusCode::FORBIDDEN, "text/plain", vec![]);
        }
        return serve_video(&crate::config::dir().join("preview").join(name), req);
    }
    if path == "video" {
        let q = req.uri().query().unwrap_or("");
        let p = PathBuf::from(pct_decode(q.strip_prefix("p=").unwrap_or("")));
        // Seulement un fichier directement dans le dossier de sortie.
        if p.parent() != Some(out_dir) {
            return reply(StatusCode::FORBIDDEN, "text/plain", vec![]);
        }
        return serve_video(&p, req);
    }
    match UI.get_file(path) {
        Some(f) => reply(StatusCode::OK, mime(path), f.contents().to_vec()),
        None => reply(StatusCode::NOT_FOUND, "text/plain", vec![]),
    }
}

/// `init` = JSON de `window.__INIT__`. Chaque message IPC est relayé à la boucle via `on_ipc`.
pub fn open<T: 'static>(
    kind: Kind,
    target: &EventLoopWindowTarget<T>,
    init: &str,
    proxy: EventLoopProxy<T>,
    on_ipc: fn(Kind, String) -> T,
) -> Option<Win> {
    use tao::platform::windows::{IconExtWindows, WindowBuilderExtWindows};
    let mut wb = WindowBuilder::new().with_title("Clipper").with_decorations(false).with_window_icon(tao::window::Icon::from_resource(1, None).ok());
    wb = match kind {
        Kind::Main => wb.with_inner_size(LogicalSize::new(900.0, 620.0)).with_min_inner_size(LogicalSize::new(760.0, 520.0)).with_visible(false),
        Kind::Bar => {
            // Transparente, au premier plan, hors barre des tâches. Placée et focalisée après création (voir place_bar).
            // Assez grande pour la barre, la galerie et le lecteur ; le vide est transparent et un clic dessus ferme.
            wb.with_inner_size(LogicalSize::new(BAR_W, BAR_H))
                .with_visible(false)
                .with_transparent(true)
                .with_always_on_top(true)
                .with_skip_taskbar(true)
                .with_focused(false)
                .with_resizable(false)
                .with_undecorated_shadow(false)
        }
    };
    let window = wb.build(target).ok()?;
    if kind == Kind::Bar {
        tool_window(&window); // créée cachée : affichée par show_bar à chaque Alt+Z, jamais détruite
    }
    // Fenêtre principale : cachée jusqu'à la 1re image de la page (message "ready"), sinon WebView2
    // affiche un écran blanc avant l'intro. main.rs la montre au plus tard après 1,5 s.
    let page = if kind == Kind::Main { "index.html" } else { "bar.html" };
    let web = WebViewBuilder::new()
        .with_custom_protocol("clipper".into(), move |_, req| protocol(&req))
        .with_initialization_script(format!("window.__INIT__={init};"))
        .with_ipc_handler(move |req: Request<String>| {
            let _ = proxy.send_event(on_ipc(kind, req.into_body()));
        })
        .with_transparent(kind == Kind::Bar)
        .with_background_color(if kind == Kind::Bar { (0, 0, 0, 0) } else { (20, 25, 19, 255) })
        .with_devtools(cfg!(debug_assertions))
        // Son de l'intro sans clic préalable. Même réglage pour les deux fenêtres : elles partagent
        // l'environnement WebView2, qui refuse des options différentes.
        .with_autoplay(true)
        .with_url(format!("clipper://localhost/{page}"))
        .build(&window)
        .ok()?;
    Some(Win { window, web })
}

/// WS_EX_TOOLWINDOW : ni barre des tâches ni Alt+Tab. La barre peut prendre le focus (souris rendue par le jeu).
fn tool_window(w: &Window) {
    use tao::platform::windows::WindowExtWindows;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_TOOLWINDOW};
    let h = w.hwnd() as _;
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_TOOLWINDOW as isize);
    }
}

/// Encode un chemin pour `/video?p=…`.
pub fn pct_encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn pct_roundtrip() {
        let p = r"C:\Users\Vous\Videos\Clipper\Clip 2026-11-07 21-14-03.mp4";
        assert_eq!(super::pct_decode(&super::pct_encode(p)), p);
        assert_eq!(super::pct_decode("a%2"), "a%2");
    }

    #[test]
    fn ui_is_embedded() {
        assert!(super::UI.get_file("index.html").is_some());
        assert!(super::UI.get_file("bar.html").is_some());
    }
}

/// Haut-centre de l'écran du jeu (fenêtre au premier plan), en pixels réels :
/// juste même quand les deux écrans n'ont pas la même mise à l'échelle.
pub fn show_bar(w: &Window) {
    use tao::dpi::PhysicalPosition;
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    let ok = unsafe { GetMonitorInfoW(MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY), &mut mi) } != 0;
    if ok {
        let r = mi.rcMonitor;
        // Taille recalculée pour l'écran du jeu (mise à l'échelle différente d'un écran à l'autre),
        // limitée à l'écran.
        let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForWindow(GetForegroundWindow()) }.max(96) as f64 / 96.0;
        let (mw, mh) = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
        let size = tao::dpi::PhysicalSize::new(((BAR_W * dpi) as u32).min(mw), ((BAR_H * dpi) as u32).min(mh));
        w.set_inner_size(size);
        w.set_outer_position(PhysicalPosition::new(r.left + (mw as i32 - size.width as i32) / 2, r.top));
    }
    w.set_visible(true);
    // Focus pris : le jeu rend la souris (curseur visible, plus bloqué), et Échap ferme la barre.
    // Le focus revient au jeu à la fermeture (main.rs, hide_bar).
    w.set_focus();
}
