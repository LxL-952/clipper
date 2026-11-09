//! Mises à jour intégrées : une seule requête à l'API GitHub au lancement (désactivable), puis, sur
//! demande, téléchargement de l'installateur de la dernière release, empreinte SHA-256 vérifiée contre
//! celle que publie GitHub, installation silencieuse (l'installateur ferme puis relance Clipper).
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::PathBuf;

const API: &str = "https://api.github.com/repos/LxL-952/clipper/releases/latest";
const DOWNLOADS: &str = "https://github.com/LxL-952/clipper/releases/download/";
const UA: &str = concat!("Clipper/", env!("CARGO_PKG_VERSION"));

/// Release trouvée au lancement, en attente du clic « Installer ».
pub static FOUND: std::sync::Mutex<Option<Release>> = std::sync::Mutex::new(None);

#[derive(Clone)]
pub struct Release {
    pub version: String,
    url: String,
    size: u64,
    sha256: String,
}

/// Dernière release si elle est plus récente que cette version (None sinon).
pub fn check() -> Result<Option<Release>, String> {
    let r = latest()?;
    Ok(crate::drivers::is_newer(&r.version, env!("CARGO_PKG_VERSION")).then_some(r))
}

/// Dernière release publiée, quelle que soit sa version.
fn latest() -> Result<Release, String> {
    let text = ureq::get(API)
        .header("User-Agent", UA)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("Réseau : {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let j: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let version = j["tag_name"].as_str().unwrap_or("").trim_start_matches('v').to_string();
    let asset = j["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str().is_some_and(|n| n.starts_with("Clipper-Setup-") && n.ends_with(".exe")))
        .ok_or("Release sans installateur.")?;
    let url = asset["browser_download_url"].as_str().unwrap_or("").to_string();
    // Seulement un fichier publié dans les releases de ce dépôt, et seulement avec son empreinte.
    if !url.starts_with(DOWNLOADS) {
        return Err(format!("Adresse refusée : {url}"));
    }
    let sha256 = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).ok_or("Empreinte SHA-256 absente : mise à jour refusée.")?.to_ascii_lowercase();
    Ok(Release { version, url, size: asset["size"].as_u64().unwrap_or(0), sha256 })
}

/// Télécharge dans %TEMP% en calculant l'empreinte au fil de l'eau ; fichier supprimé si elle diffère.
pub fn download(r: &Release) -> Result<PathBuf, String> {
    use ureq::ResponseExt;
    let mut resp = ureq::get(&r.url).header("User-Agent", UA).call().map_err(|e| format!("Réseau : {e}"))?;
    // GitHub redirige vers son stockage ; rien d'autre n'est accepté.
    let host = resp.get_uri().host().unwrap_or("").to_string();
    if host != "github.com" && !host.ends_with(".githubusercontent.com") {
        return Err(format!("Redirection refusée : {host}"));
    }
    let dest = std::env::temp_dir().join(format!("Clipper-Setup-{}.exe", r.version));
    let mut f = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
    let mut body = resp.body_mut().with_config().limit(u64::MAX).reader();
    let (mut h, mut buf, mut done) = (Sha256::new(), vec![0u8; 1 << 16], 0u64);
    loop {
        let n = body.read(&mut buf).map_err(|e| format!("Téléchargement interrompu : {e}"))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
    }
    drop(f);
    let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got != r.sha256 || (r.size > 0 && done != r.size) {
        let _ = std::fs::remove_file(&dest);
        return Err("Fichier téléchargé différent de celui publié (empreinte SHA-256) : supprimé.".into());
    }
    Ok(dest)
}

/// Lance l'installateur en mode silencieux : il ferme Clipper (--quit), installe, puis le relance.
pub fn install(setup: &PathBuf) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // Hors du job « tuer à la fermeture » de Clipper : l'installateur doit lui survivre. Sans cela, Clipper
    // se fermait, Windows tuait l'installateur avec lui, et rien n'était installé ni relancé.
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    std::process::Command::new(setup)
        .creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NEW_PROCESS_GROUP)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .spawn()
        .map(drop)
        .map_err(|e| format!("Installateur impossible à lancer : {e}"))
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore = "réseau"]
    fn latest_release_is_readable_and_verified() {
        // Dernière release, même plus ancienne que ce build : on vérifie lecture, téléchargement et empreinte.
        match super::latest() {
            Ok(r) => {
                let p = super::download(&r).expect("téléchargement vérifié");
                println!("{} téléchargé et vérifié : {}", r.version, p.display());
                let _ = std::fs::remove_file(p);
            }
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    #[ignore = "réseau"]
    fn wrong_fingerprint_is_refused_and_deleted() {
        let mut r = super::latest().unwrap();
        r.sha256 = "0".repeat(64);
        assert!(super::download(&r).is_err());
        assert!(!std::env::temp_dir().join(format!("Clipper-Setup-{}.exe", r.version)).exists());
    }
}
