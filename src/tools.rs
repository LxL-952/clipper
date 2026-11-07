//! Outils : nettoyage des fichiers temporaires / caches, et nettoyage de la RAM.
//! Sécurité : on ne supprime que DANS les dossiers listés ici, sans jamais suivre un lien ou une jonction.
use std::path::{Path, PathBuf};

pub struct Category {
    pub id: &'static str,
    pub name: &'static str,
    pub admin: bool,
    pub default: bool,
    pub warn: Option<&'static str>,
}

pub const CATEGORIES: &[Category] = &[
    Category { id: "user_temp", name: "Fichiers temporaires (utilisateur)", admin: false, default: true, warn: None },
    Category { id: "win_temp", name: "Fichiers temporaires (Windows)", admin: true, default: true, warn: None },
    Category { id: "shaders", name: "Caches de shaders (DirectX, NVIDIA, AMD)", admin: false, default: true,
        warn: Some("Chaque jeu recompilera ses shaders au prochain lancement : quelques saccades pendant un chargement.") },
    Category { id: "win_update", name: "Cache de Windows Update", admin: true, default: true, warn: None },
    Category { id: "delivery", name: "Optimisation de distribution", admin: true, default: true, warn: None },
    Category { id: "crash", name: "Rapports d'erreurs et vidages mémoire", admin: true, default: true, warn: None },
    Category { id: "thumbs", name: "Cache des miniatures", admin: false, default: true, warn: Some("Les miniatures de l'Explorateur seront recréées.") },
    Category { id: "browsers", name: "Caches des navigateurs (Edge, Chrome, Firefox)", admin: false, default: false,
        warn: Some("Seulement le cache : mots de passe, historique et cookies ne sont pas touchés.") },
    Category { id: "recycle", name: "Corbeille", admin: false, default: false, warn: Some("Suppression définitive.") },
];

fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(k).unwrap_or_default())
}

/// Sous-dossiers de `base/*/<rel>` (profils de navigateur).
fn per_profile(base: PathBuf, rel: &str) -> Vec<PathBuf> {
    std::fs::read_dir(&base).into_iter().flatten().flatten().map(|e| e.path().join(rel)).filter(|p| p.is_dir()).collect()
}

/// Dossiers dont le CONTENU est supprimé. Chaque entrée est un chemin fixe, jamais une saisie utilisateur.
pub fn roots(id: &str) -> Vec<PathBuf> {
    let (local, win) = (env("LOCALAPPDATA"), env("SystemRoot"));
    match id {
        "user_temp" => vec![std::env::temp_dir()],
        "win_temp" => vec![win.join("Temp")],
        "shaders" => vec![local.join("D3DSCache"), local.join(r"NVIDIA\DXCache"), local.join(r"NVIDIA\GLCache"), local.join(r"AMD\DxCache"), local.join(r"AMD\DxcCache")],
        "win_update" => vec![win.join(r"SoftwareDistribution\Download")],
        "delivery" => vec![win.join(r"ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache")],
        "crash" => vec![local.join("CrashDumps"), env("ProgramData").join(r"Microsoft\Windows\WER\ReportArchive"), env("ProgramData").join(r"Microsoft\Windows\WER\ReportQueue")],
        "thumbs" => vec![local.join(r"Microsoft\Windows\Explorer")],
        "browsers" => [
            per_profile(local.join(r"Microsoft\Edge\User Data"), r"Cache\Cache_Data"),
            per_profile(local.join(r"Google\Chrome\User Data"), r"Cache\Cache_Data"),
            per_profile(local.join(r"Mozilla\Firefox\Profiles"), "cache2"),
        ]
        .concat(),
        _ => vec![],
    }
}

/// Le buffer de Clipper vit dans %TEMP% : il ne doit jamais être nettoyé.
fn protected(p: &Path, keep: &[PathBuf]) -> bool {
    keep.iter().any(|k| p.starts_with(k))
}

/// Seuls les fichiers thumbcache_*.db du dossier Explorer sont concernés.
fn wanted(id: &str, p: &Path, m: &std::fs::Metadata) -> bool {
    // Temp : seulement les fichiers de plus de 24 h, pour ne rien casser chez une app en cours.
    let old = m.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a.as_secs() > 86_400);
    if (id == "user_temp" || id == "win_temp") && !old {
        return false;
    }
    id != "thumbs" || p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("thumbcache_") || n.starts_with("iconcache_"))
}

/// Parcourt sans suivre les liens. `f` reçoit chaque fichier.
fn walk(dir: &Path, f: &mut dyn FnMut(&Path, &std::fs::Metadata)) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(m) = std::fs::symlink_metadata(e.path()) else { continue };
        if m.file_type().is_symlink() || is_reparse(&m) {
            continue;
        }
        if m.is_dir() {
            walk(&e.path(), f);
        } else {
            f(&e.path(), &m);
        }
    }
}

fn is_reparse(m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT (jonctions)
}

/// (octets, fichiers) d'une catégorie.
pub fn scan(id: &str, keep: &[PathBuf]) -> (u64, u64) {
    if id == "recycle" {
        return recycle_size();
    }
    let (mut b, mut n) = (0, 0);
    for r in roots(id) {
        walk(&r, &mut |p, m| {
            let len = m.len();
            if wanted(id, p, m) && !protected(p, keep) {
                b += len;
                n += 1;
            }
        });
    }
    (b, n)
}

/// Supprime ; les fichiers verrouillés (en cours d'utilisation) sont ignorés. Renvoie (octets libérés, ignorés).
pub fn clean(id: &str, keep: &[PathBuf]) -> (u64, u64) {
    if id == "recycle" {
        let (b, _) = recycle_size();
        use windows_sys::Win32::UI::Shell::{SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW};
        let ok = unsafe { SHEmptyRecycleBinW(std::ptr::null_mut(), std::ptr::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND) } == 0;
        return if ok { (b, 0) } else { (0, 1) };
    }
    roots(id).iter().map(|r| clean_root(id, r, keep)).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
}

fn clean_root(id: &str, r: &Path, keep: &[PathBuf]) -> (u64, u64) {
    let (mut freed, mut skipped) = (0, 0);
    walk(r, &mut |p, m| {
        let len = m.len();
        if !wanted(id, p, m) || protected(p, keep) {
            return;
        }
        if std::fs::remove_file(p).is_ok() { freed += len } else { skipped += 1 }
    });
    remove_empty_dirs(r);
    (freed, skipped)
}

fn remove_empty_dirs(root: &Path) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let Ok(m) = std::fs::symlink_metadata(e.path()) else { continue };
        if m.is_dir() && !m.file_type().is_symlink() && !is_reparse(&m) {
            remove_empty_dirs(&e.path());
            let _ = std::fs::remove_dir(e.path()); // échoue sans dommage si non vide
        }
    }
}

fn recycle_size() -> (u64, u64) {
    use windows_sys::Win32::UI::Shell::{SHQUERYRBINFO, SHQueryRecycleBinW};
    let mut i: SHQUERYRBINFO = unsafe { std::mem::zeroed() };
    i.cbSize = std::mem::size_of::<SHQUERYRBINFO>() as u32;
    unsafe { SHQueryRecycleBinW(std::ptr::null(), &mut i) };
    (i.i64Size as u64, i.i64NumItems as u64)
}

/// RAM physique (libre, totale) en octets.
pub fn ram() -> (u64, u64) {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    unsafe { GlobalMemoryStatusEx(&mut m) };
    (m.ullAvailPhys, m.ullTotalPhys)
}

/// Exécuté dans le processus élevé `clipper.exe --clean-ram`.
/// Appel système global : aucun processus n'est ouvert (pas de handle sur le jeu).
/// `standby_only` : jeu plein écran au premier plan, on ne vide que la liste standby
/// (la mémoire du jeu n'est pas touchée, pas de saccade).
pub fn clean_ram_elevated(standby_only: bool) -> i32 {
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSetSystemInformation(class: u32, info: *const u32, len: u32) -> i32;
    }
    const SYSTEM_MEMORY_LIST_INFORMATION: u32 = 80;
    const MEMORY_EMPTY_WORKING_SETS: u32 = 2;
    const MEMORY_PURGE_STANDBY_LIST: u32 = 4;
    enable_privilege("SeProfileSingleProcessPrivilege");
    enable_privilege("SeIncreaseQuotaPrivilege");
    let mut fails = 0;
    let cmds: &[u32] = if standby_only { &[MEMORY_PURGE_STANDBY_LIST] } else { &[MEMORY_EMPTY_WORKING_SETS, MEMORY_PURGE_STANDBY_LIST] };
    for &cmd in cmds {
        if unsafe { NtSetSystemInformation(SYSTEM_MEMORY_LIST_INFORMATION, &cmd, 4) } < 0 {
            fails += 1;
        }
    }
    fails
}

/// Active un privilège sur NOTRE jeton (processus courant).
fn enable_privilege(name: &str) {
    use windows_sys::Win32::Foundation::{CloseHandle, LUID};
    use windows_sys::Win32::Security::{AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let w: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let mut tok = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES, &mut tok) == 0 {
            return;
        }
        let mut luid: LUID = std::mem::zeroed();
        if LookupPrivilegeValueW(std::ptr::null(), w.as_ptr(), &mut luid) != 0 {
            let tp = TOKEN_PRIVILEGES { PrivilegeCount: 1, Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }] };
            AdjustTokenPrivileges(tok, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
        }
        CloseHandle(tok);
    }
}

/// Relance clipper.exe élevé (UAC) avec `args`, attend la fin, renvoie le code de sortie.
pub fn run_elevated(args: &str) -> Result<u32, String> {
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (verb, file, params) = (w("runas"), w(&exe.display().to_string()), w(args));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = 0;
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err("Droits administrateur refusés.".into());
    }
    let mut code = 0u32;
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut code);
    }
    Ok(code)
}

/// Mode élevé `--clean <ids…>` : arrête Windows Update le temps de vider son cache.
pub fn clean_elevated(ids: &[String], keep: &[PathBuf]) -> i32 {
    let sc = |a: &str, s: &str| {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("sc").args([a, s]).creation_flags(0x0800_0000).status();
    };
    let wu = ids.iter().any(|i| i == "win_update");
    if wu {
        sc("stop", "wuauserv");
        sc("stop", "bits");
    }
    for id in ids {
        clean(id, keep);
    }
    if wu {
        sc("start", "bits");
        sc("start", "wuauserv");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_follows_junction_and_respects_keep() {
        let base = std::env::temp_dir().join(format!("clipper-test-{}", std::process::id()));
        let (inside, outside) = (base.join("root"), base.join("outside"));
        std::fs::create_dir_all(inside.join("buf")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(inside.join("a.tmp"), b"1234").unwrap();
        std::fs::write(inside.join("buf").join("seg.ts"), b"12").unwrap();
        std::fs::write(outside.join("precious.txt"), b"keep me").unwrap();
        // Jonction root/link -> outside : ne doit jamais être suivie.
        let _ = std::process::Command::new("cmd").args(["/c", "mklink", "/J"]).arg(inside.join("link")).arg(&outside).output();
        let mut seen = vec![];
        walk(&inside, &mut |p, _m| seen.push(p.to_path_buf()));
        assert!(seen.iter().all(|p| !p.ends_with("precious.txt")));
        assert!(protected(&inside.join("buf").join("seg.ts"), &[inside.join("buf")]));
        assert!(outside.join("precious.txt").exists());
        let _ = std::fs::remove_dir(inside.join("link"));
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore = "lit le disque"]
    fn scan_only() {
        for c in super::CATEGORIES {
            let (b, n) = super::scan(c.id, &[]);
            println!("{:<48} {:>9.1} Mo  {:>6} fichiers", c.name, b as f64 / 1e6, n);
        }
        let (f, t) = super::ram();
        println!("RAM libre {:.1} / {:.1} Go", f as f64 / 1e9, t as f64 / 1e9);
    }
}

#[cfg(test)]
mod sandbox {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn deletes_only_old_unprotected_files_inside_root() {
        let base = std::env::temp_dir().join(format!("clipper-sbx-{}", std::process::id()));
        let (root, outside) = (base.join("root"), base.join("outside"));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("buf")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let old = |p: &Path| {
            std::fs::write(p, b"0123456789").unwrap();
            let f = std::fs::File::options().write(true).open(p).unwrap();
            f.set_modified(SystemTime::now() - Duration::from_secs(3 * 86_400)).unwrap();
        };
        old(&root.join("old.tmp"));
        old(&root.join("sub").join("old2.tmp"));
        old(&root.join("buf").join("seg.ts")); // buffer Clipper : protégé
        std::fs::write(root.join("fresh.tmp"), b"new").unwrap(); // < 24 h : gardé
        old(&outside.join("precious.txt"));
        let _ = std::process::Command::new("cmd").args(["/c", "mklink", "/J"]).arg(root.join("link")).arg(&outside).output();
        // Fichier verrouillé : ignoré sans erreur.
        old(&root.join("locked.tmp"));
        let lock = std::fs::File::options().read(true).share_mode(0).open(root.join("locked.tmp")).ok();
        use std::os::windows::fs::OpenOptionsExt;

        let (freed, skipped) = clean_root("user_temp", &root, &[root.join("buf")]);
        assert_eq!(freed, 20, "old.tmp + sub/old2.tmp");
        assert_eq!(skipped, if lock.is_some() { 1 } else { 0 });
        assert!(!root.join("old.tmp").exists() && !root.join("sub").exists());
        assert!(root.join("fresh.tmp").exists());
        assert!(root.join("buf").join("seg.ts").exists());
        assert!(outside.join("precious.txt").exists(), "la jonction n'a pas été suivie");
        drop(lock);
        let _ = std::fs::remove_dir(root.join("link"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
