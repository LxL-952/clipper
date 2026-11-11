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

/// Carte NVIDIA et version du pilote. Registre d'abord (instantané, à jour dès la fin d'une installation,
/// marche même quand nvidia-smi est absent du PATH ou occupé) ; nvidia-smi en secours.
pub fn installed() -> Option<Gpu> {
    from_registry().or_else(from_smi)
}

/// « 32.0.15.9144 » (version Windows du pilote) -> « 591.44 » (version NVIDIA : les 5 derniers chiffres).
fn nvidia_version(win: &str) -> Option<String> {
    let parts: Vec<&str> = win.trim().split('.').collect();
    if parts.len() != 4 || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let digits = format!("{}{:0>4}", parts[2], parts[3]);
    let d = &digits[digits.len().checked_sub(5)?..];
    Some(format!("{}.{}", &d[..3], &d[3..]))
}

/// Classe « cartes graphiques » du registre : une clé par carte (0000, 0001…).
fn from_registry() -> Option<Gpu> {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let read = |key: &str, value: &str| -> Option<String> {
        let mut buf = vec![0u16; 512];
        let mut len = (buf.len() * 2) as u32;
        let ok = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, w(key).as_ptr(), w(value).as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr() as _, &mut len) } == 0;
        ok.then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]))
    };
    (0..16).find_map(|i| {
        let key = format!(r"SYSTEM\CurrentControlSet\Control\Class\{{4d36e968-e325-11ce-bfc1-08002be10318}}\{i:04}");
        let name = read(&key, "DriverDesc")?;
        if !name.to_ascii_uppercase().contains("NVIDIA") {
            return None;
        }
        Some(Gpu { name: name.trim().into(), driver: nvidia_version(&read(&key, "DriverVersion")?)? })
    })
}

fn from_smi() -> Option<Gpu> {
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
    find_product(&xml, gpu).ok_or(format!("Modèle « {} » inconnu de NVIDIA.", normalize(gpu)))
}

/// NVIDIA écrit tantôt « NVIDIA GeForce RTX 4080 SUPER », tantôt « GeForce RTX 4060 Laptop GPU ».
fn normalize(name: &str) -> String {
    let n = name.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase();
    n.strip_prefix("nvidia ").unwrap_or(&n).to_string()
}

/// <LookupValue ParentID="129"><Name>GeForce RTX 4060 Laptop GPU</Name><Value>1007</Value>
fn find_product(xml: &str, gpu: &str) -> Option<(String, String)> {
    let gpu = normalize(gpu);
    xml.split("<LookupValue ").skip(1).find_map(|e| {
        let psid = e.split("ParentID=\"").nth(1)?.split('"').next()?;
        let name = e.split("<Name>").nth(1)?.split("</Name>").next()?;
        let pfid = e.split("<Value>").nth(1)?.split('<').next()?;
        (normalize(name) == gpu).then(|| (psid.to_string(), pfid.to_string()))
    })
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
    match code {
        0 => Ok(()),
        _ => Err(format!(
            "L'installateur NVIDIA a échoué (code 0x{code:08X}). Une autre installation de pilote est peut-être en cours : attendez qu'elle finisse, ou redémarrez le PC, puis réessayez"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_driver_version_to_nvidia() {
        assert_eq!(nvidia_version("32.0.15.9144").as_deref(), Some("591.44"));
        assert_eq!(nvidia_version("32.0.15.6094").as_deref(), Some("560.94"));
        assert_eq!(nvidia_version("31.0.15.3623").as_deref(), Some("536.23"));
        assert_eq!(nvidia_version("n/a"), None);
    }

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
    fn product_names_with_or_without_nvidia_prefix() {
        let xml = r#"<LookupValues><LookupValue ParentID="127"><Name>NVIDIA GeForce RTX 4080 SUPER</Name><Value>1041</Value></LookupValue><LookupValue ParentID="129"><Name>GeForce RTX 4060 Laptop GPU</Name><Value>1007</Value></LookupValue><LookupValue ParentID="127"><Name>NVIDIA GeForce RTX 4080</Name><Value>999</Value></LookupValue></LookupValues>"#;
        assert_eq!(find_product(xml, "NVIDIA GeForce RTX 4080 SUPER"), Some(("127".into(), "1041".into())));
        assert_eq!(find_product(xml, "NVIDIA GeForce RTX 4080"), Some(("127".into(), "999".into())));
        assert_eq!(find_product(xml, "NVIDIA GeForce RTX 4060 Laptop GPU"), Some(("129".into(), "1007".into())));
        assert_eq!(find_product(xml, "GeForce RTX 9999"), None);
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

    /// Même chemin que le bouton « Installer » de Clipper : installateur NVIDIA en silencieux, UAC Windows.
    #[test]
    #[ignore = "installe le pilote"]
    fn install_downloaded() {
        let l = super::latest("NVIDIA GeForce RTX 4060 Laptop GPU", false).unwrap();
        let dest = std::path::PathBuf::from(std::env::var("CLIPPER_DL").unwrap()).join(format!("{}.exe", l.version));
        let t = std::time::Instant::now();
        let r = super::install(&dest);
        println!("installation {r:?} en {:.0} s", t.elapsed().as_secs_f64());
        assert!(r.is_ok());
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
