//! Pilotes NVIDIA : version installée (nvidia-smi), dernière version (API officielle NVIDIA),
//! téléchargement limité à *.nvidia.com, contrôle taille + signature, installation silencieuse.
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Clipper";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct Gpu {
    pub name: String,
    pub driver: String,
}

pub fn installed() -> Option<Gpu> {
    let out = Command::new("nvidia-smi")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["--query-gpu=name,driver_version", "--format=csv,noheader"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let (name, driver) = s.lines().next()?.split_once(", ")?;
    Some(Gpu { name: name.trim().into(), driver: driver.trim().into() })
}

pub struct Latest {
    pub version: String,
    pub url: String,
}

/// Seuls les liens HTTPS vers un domaine NVIDIA sont acceptés.
pub fn is_official(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', ':', '?', '#']).next().unwrap_or("").to_ascii_lowercase();
    !host.contains('@') && (host == "nvidia.com" || host.ends_with(".nvidia.com"))
}

fn get(url: &str) -> Result<String, String> {
    ureq::get(url)
        .header("User-Agent", UA)
        .call()
        .map_err(|e| format!("Réseau : {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())
}

/// Identifiants produit NVIDIA (psid = série, pfid = modèle) à partir du nom du GPU.
fn product_ids(gpu: &str) -> Result<(String, String), String> {
    let xml = get("https://www.nvidia.com/Download/API/lookupValueSearch.aspx?TypeID=3")?;
    // <LookupValue ParentID="129"><Name>GeForce RTX 4060 Laptop GPU</Name><Value>1007</Value>
    let gpu = gpu.trim_start_matches("NVIDIA ");
    let needle = format!("<Name>{gpu}</Name>");
    let at = xml.find(&needle).ok_or(format!("Modèle « {gpu} » inconnu de NVIDIA."))?;
    let before = &xml[..at];
    let psid = before.rsplit("ParentID=\"").next().and_then(|s| s.split('"').next()).ok_or("Réponse NVIDIA illisible.")?;
    let after = &xml[at + needle.len()..];
    let pfid = after.split("<Value>").nth(1).and_then(|s| s.split('<').next()).ok_or("Réponse NVIDIA illisible.")?;
    Ok((psid.into(), pfid.into()))
}

fn json_field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let k = format!("\"{key}\" : \"");
    let i = json.find(&k)? + k.len();
    json[i..].split('"').next()
}

/// `studio` = pilote Studio (CRD) au lieu de Game Ready.
pub fn latest(gpu: &str, studio: bool) -> Result<Latest, String> {
    let (psid, pfid) = product_ids(gpu)?;
    let json = get(&format!(
        "https://gfwsl.geforce.com/services_toolkit/services/com/nvidia/services/AjaxDriverService.php\
         ?func=DriverManualLookup&psid={psid}&pfid={pfid}&osID=57&languageCode=1033&dch=1&qnf=0&{}&sort1=0&numberOfResults=1",
        if studio { "upCRD=1" } else { "isWHQL=1&upCRD=0" } // Studio : le filtre WHQL ne renvoie rien
    ))?;
    let version = json_field(&json, "Version").ok_or("Aucun pilote trouvé.")?.to_string();
    let url = json_field(&json, "DownloadURL").ok_or("Aucun lien de téléchargement.")?.to_string();
    if !is_official(&url) {
        return Err(format!("Lien refusé (pas un domaine NVIDIA) : {url}"));
    }
    Ok(Latest { version, url })
}

/// Compare "621.14" et "621.01" numériquement.
pub fn is_newer(latest: &str, installed: &str) -> bool {
    let v = |s: &str| s.split('.').map(|p| p.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    v(latest) > v(installed)
}

/// Télécharge avec progression. Vérifie : domaine final NVIDIA, taille = Content-Length, signature NVIDIA.
pub fn download(url: &str, dest: &Path, mut progress: impl FnMut(u64, u64)) -> Result<PathBuf, String> {
    use ureq::ResponseExt;
    let mut resp = ureq::get(url).header("User-Agent", UA).call().map_err(|e| format!("Réseau : {e}"))?;
    let final_url = resp.get_uri().to_string();
    if !is_official(&final_url) {
        return Err(format!("Redirection refusée hors NVIDIA : {final_url}"));
    }
    let total: u64 = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse().ok()).ok_or("Taille du fichier inconnue.")?;
    std::fs::create_dir_all(dest.parent().unwrap_or(Path::new("."))).map_err(|e| e.to_string())?;
    let mut f = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut r = resp.body_mut().with_config().limit(u64::MAX).reader();
    let (mut done, mut buf, mut last) = (0u64, vec![0u8; 1 << 16], 0u64);
    loop {
        let n = r.read(&mut buf).map_err(|e| format!("Téléchargement interrompu : {e}"))?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if done - last > total / 200 {
            progress(done, total);
            last = done;
        }
    }
    drop(f);
    if done != total {
        let _ = std::fs::remove_file(dest);
        return Err(format!("Fichier incomplet : {done} / {total} octets."));
    }
    if !signed_by_nvidia(dest) {
        let _ = std::fs::remove_file(dest);
        return Err("Signature numérique invalide : fichier supprimé.".into());
    }
    progress(done, total);
    Ok(dest.to_path_buf())
}

/// Signature Authenticode valide et émise pour NVIDIA Corporation.
fn signed_by_nvidia(file: &Path) -> bool {
    let script = format!(
        "$s=Get-AuthenticodeSignature -LiteralPath '{}'; if($s.Status -eq 'Valid' -and $s.SignerCertificate.Subject -match 'O=NVIDIA Corporation'){{exit 0}}else{{exit 1}}",
        file.display().to_string().replace('\'', "''")
    );
    Command::new("powershell")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .is_ok_and(|s| s.success())
}

/// Lance l'installateur en silencieux, avec la demande UAC de Windows. Attend la fin.
pub fn install(exe: &Path) -> Result<(), String> {
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (verb, file, params) = (w("runas"), w(&exe.display().to_string()), w("-s -noreboot"));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err("Installation annulée (droits administrateur refusés).".into());
    }
    let mut code = 0u32;
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut code);
    }
    if code == 0 { Ok(()) } else { Err(format!("L'installateur NVIDIA a renvoyé le code {code}.")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_nvidia_domains() {
        assert!(is_official("https://us.download.nvidia.com/Windows/621.14/x.exe"));
        assert!(is_official("https://nvidia.com/a"));
        assert!(!is_official("http://us.download.nvidia.com/x.exe"));
        assert!(!is_official("https://nvidia.com.evil.io/x.exe"));
        assert!(!is_official("https://evilnvidia.com/x.exe"));
        assert!(!is_official("https://download.nvidia.com@evil.io/x.exe"));
    }

    #[test]
    #[ignore = "réseau"]
    fn live_lookup() {
        let gr = latest("GeForce RTX 4060 Laptop GPU", false).unwrap();
        let st = latest("GeForce RTX 4060 Laptop GPU", true).unwrap();
        println!("Game Ready {} {}\nStudio {} {}", gr.version, gr.url, st.version, st.url);
        let g = installed().unwrap();
        println!("installé {} {}", g.name, g.driver);
    }

    #[test]
    fn version_compare() {
        assert!(is_newer("621.14", "621.01"));
        assert!(!is_newer("621.01", "621.01"));
        assert!(is_newer("630.00", "621.99"));
    }
}

#[cfg(test)]
mod live_download {
    #[test]
    #[ignore = "télécharge ~1 Go"]
    fn download_and_verify() {
        let l = super::latest("NVIDIA GeForce RTX 4060 Laptop GPU", false).unwrap();
        let dest = std::path::PathBuf::from(std::env::var("CLIPPER_DL").unwrap()).join(format!("{}.exe", l.version));
        let t = std::time::Instant::now();
        let mut ticks = 0;
        let r = super::download(&l.url, &dest, |d, tot| {
            ticks += 1;
            if ticks % 40 == 0 { println!("{:.0} %", d as f64 / tot as f64 * 100.0) }
        });
        println!("résultat {r:?} en {:.0} s", t.elapsed().as_secs_f64());
        assert!(r.is_ok());
        println!("taille {} octets", std::fs::metadata(&dest).unwrap().len());
    }
}

#[cfg(test)]
mod signature {
    #[test]
    fn rejects_non_nvidia_signed_exe() {
        // notepad.exe est signé par Microsoft : valide, mais pas NVIDIA.
        assert!(!super::signed_by_nvidia(std::path::Path::new(r"C:\Windows\System32\notepad.exe")));
        assert!(!super::signed_by_nvidia(std::path::Path::new(r"C:\nexiste\pas.exe")));
    }
}
