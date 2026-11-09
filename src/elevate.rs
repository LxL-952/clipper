//! Mode administrateur sans demande UAC à chaque lancement : tâche planifiée « privilèges les plus élevés »
//! créée une fois (une seule demande UAC), puis chaque lancement normal passe par elle.
use std::os::windows::process::CommandExt;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TASK: &str = "Clipper (administrateur)";
const EVENT: &str = "Local\\ClipperOpenSettings";

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut tok: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
            return false;
        }
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(tok, TokenElevation, &mut e as *mut _ as _, std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len);
        CloseHandle(tok);
        ok != 0 && e.TokenIsElevated != 0
    }
}

/// Une instance tourne déjà (élevée ou non) : on la réveille. Vrai si c'est le cas.
pub fn signal_existing() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
    unsafe {
        let h = OpenEventW(EVENT_MODIFY_STATE, 0, w(EVENT).as_ptr());
        if h.is_null() {
            return false;
        }
        SetEvent(h);
        CloseHandle(h);
        true
    }
}

/// Événement d'instance unique, ouvrable par une instance non élevée (sinon un 2e lancement normal
/// ne pouvait pas réveiller l'instance admin et en démarrait une seconde).
/// `wait` : lancé par la tâche, l'ancienne instance est peut-être encore en train de se fermer.
pub fn own_event(wait: bool) -> Option<windows_sys::Win32::Foundation::HANDLE> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, LocalFree};
    use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};
    unsafe {
        let mut sd = std::ptr::null_mut();
        // Utilisateurs authentifiés et SYSTEM ; étiquette d'intégrité basse : une instance non élevée peut le signaler.
        let sddl = w("D:(A;;GA;;;AU)(A;;GA;;;SY)S:(ML;;NW;;;LW)");
        let have_sd = ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) != 0;
        let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
        let name = w(EVENT);
        let mut tries = if wait { 50 } else { 1 };
        let result = loop {
            let h = CreateEventW(if have_sd { &sa } else { std::ptr::null() }, 0, 0, name.as_ptr());
            if GetLastError() != ERROR_ALREADY_EXISTS {
                break (!h.is_null()).then_some(h);
            }
            tries -= 1;
            if tries == 0 {
                SetEvent(h);
                CloseHandle(h);
                break None;
            }
            CloseHandle(h);
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        if have_sd {
            LocalFree(sd as _);
        }
        result
    }
}

fn schtasks(args: &[&str]) -> bool {
    Command::new("schtasks").creation_flags(CREATE_NO_WINDOW).args(args).output().is_ok_and(|o| o.status.success())
}

/// Lance l'instance admin via la tâche. `tray` est transmis par un fichier (la tâche a des arguments fixes).
pub fn run_task(tray: bool) -> bool {
    let flag = crate::config::dir().join("launch-tray");
    if tray {
        let _ = std::fs::create_dir_all(crate::config::dir());
        let _ = std::fs::write(&flag, b"");
    } else {
        let _ = std::fs::remove_file(&flag);
    }
    schtasks(&["/run", "/tn", TASK])
}

/// Programme que lance la tâche (None : pas de tâche).
fn task_exe() -> Option<String> {
    let out = Command::new("schtasks").creation_flags(CREATE_NO_WINDOW).args(["/query", "/tn", TASK, "/xml"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    // schtasks écrit en UTF-16 ou dans la page de codes de la console selon les versions : on lit les deux.
    let b = &out.stdout;
    let text = if b.len() > 3 && b[1] == 0 {
        String::from_utf16_lossy(&b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>())
    } else {
        String::from_utf8_lossy(b).into_owned()
    };
    command_of(&text)
}

/// Contenu de <Command> dans le XML d'une tâche.
fn command_of(xml: &str) -> Option<String> {
    let c = xml.split("<Command>").nth(1)?.split("</Command>").next()?;
    Some(c.trim().trim_matches('"').replace("&amp;", "&"))
}

/// La tâche existe et lance bien ce programme. Sinon (Clipper installé ailleurs depuis, tâche d'une
/// ancienne copie) le « mode administrateur » ouvrait une autre version de Clipper, ou rien.
pub fn task_ok() -> bool {
    let Ok(me) = std::env::current_exe() else { return false };
    task_exe().is_some_and(|t| t.eq_ignore_ascii_case(&me.display().to_string()))
}

/// Relance Clipper sans élévation, depuis une instance administrateur : l'Explorateur (non élevé) le lance.
/// Le fichier « relaunch » dit à la nouvelle instance d'attendre la fermeture de celle-ci.
pub fn relaunch_normal() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let flag = crate::config::dir().join("relaunch");
    let _ = std::fs::write(&flag, b"");
    let ok = Command::new("explorer.exe").arg(exe).spawn().is_ok();
    if !ok {
        let _ = std::fs::remove_file(flag);
    }
    ok
}

/// Vrai si l'instance précédente a demandé cette relance il y a moins de 30 s (fichier consommé).
pub fn take_relaunch_flag() -> bool {
    let flag = crate::config::dir().join("relaunch");
    let fresh = std::fs::metadata(&flag).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() < 30);
    std::fs::remove_file(&flag).is_ok() && fresh
}

/// Lancé par la tâche avec « réduit dans la zone de notification » demandé.
pub fn take_tray_flag() -> bool {
    std::fs::remove_file(crate::config::dir().join("launch-tray")).is_ok()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Mode élevé `--admin-task on|off` : crée (ou remplace) la tâche, ou la supprime. Code 0 = réussi.
pub fn set_task(on: bool) -> i32 {
    if !on {
        let gone = schtasks(&["/delete", "/tn", TASK, "/f"]) || !schtasks(&["/query", "/tn", TASK]);
        return if gone { 0 } else { 1 };
    }
    let Ok(exe) = std::env::current_exe() else { return 1 };
    let user = format!("{}\\{}", std::env::var("USERDOMAIN").unwrap_or_default(), std::env::var("USERNAME").unwrap_or_default());
    // Pas de limite de durée (sinon Windows arrête l'app au bout de 72 h), marche sur batterie.
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Lance Clipper en administrateur sans demande UAC.</Description></RegistrationInfo>
  <Principals><Principal id="Author"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <AllowHardTerminate>true</AllowHardTerminate>
  </Settings>
  <Actions Context="Author"><Exec><Command>{}</Command><Arguments>--from-task</Arguments></Exec></Actions>
</Task>"#,
        xml_escape(&user),
        xml_escape(&exe.display().to_string())
    );
    let file = std::env::temp_dir().join(format!("clipper-task-{}.xml", std::process::id()));
    let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain(xml.encode_utf16().flat_map(u16::to_le_bytes)).collect();
    if std::fs::write(&file, utf16).is_err() {
        return 1;
    }
    let ok = schtasks(&["/create", "/tn", TASK, "/xml", &file.display().to_string(), "/f"]);
    let _ = std::fs::remove_file(&file);
    if ok { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_task_command() {
        let xml = r#"<Actions Context="Author"><Exec><Command>C:\A&amp;B\clipper.exe</Command><Arguments>--from-task</Arguments></Exec></Actions>"#;
        assert_eq!(super::command_of(xml).as_deref(), Some(r"C:\A&B\clipper.exe"));
        assert_eq!(super::command_of("<Task/>"), None);
    }

    #[test]
    fn escapes_paths_for_xml() {
        assert_eq!(super::xml_escape(r#"C:\A&B\<x>"q".exe"#), r#"C:\A&amp;B\&lt;x&gt;&quot;q&quot;.exe"#);
    }
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Lancement avec Windows : valeur « Clipper » de la clé Run (la même que celle de l'installateur).
/// En mode administrateur, ce lancement passe ensuite par la tâche : toujours sans demande UAC.
pub fn autostart() -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    let mut size = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), w("Clipper").as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) == 0 }
}

pub fn set_autostart(on: bool) -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW};
    unsafe {
        if !on {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), w("Clipper").as_ptr());
            return !autostart();
        }
        let Ok(exe) = std::env::current_exe() else { return false };
        let cmd = w(&format!("\"{}\" --tray", exe.display()));
        RegSetKeyValueW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), w("Clipper").as_ptr(), REG_SZ, cmd.as_ptr() as _, (cmd.len() * 2) as u32) == 0
    }
}

const QUIT_EVENT: &str = r"Local\ClipperQuit";

/// Événement « quitter » ouvrable par une instance non élevée : `clipper.exe --quit` ferme l'instance
/// en cours, même lancée en administrateur (mise à jour sans passer par la zone de notification).
pub fn quit_event() -> Option<windows_sys::Win32::Foundation::HANDLE> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Threading::CreateEventW;
    unsafe {
        let mut sd = std::ptr::null_mut();
        let sddl = w("D:(A;;GA;;;AU)(A;;GA;;;SY)S:(ML;;NW;;;LW)");
        let have_sd = ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) != 0;
        let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
        let h = CreateEventW(if have_sd { &sa } else { std::ptr::null() }, 0, 0, w(QUIT_EVENT).as_ptr());
        if have_sd {
            LocalFree(sd as _);
        }
        (!h.is_null()).then_some(h)
    }
}

/// `--quit` : demande à l'instance en cours de se fermer. Vrai si une instance a été prévenue.
pub fn signal_quit() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
    unsafe {
        let h = OpenEventW(EVENT_MODIFY_STATE, 0, w(QUIT_EVENT).as_ptr());
        if h.is_null() {
            return false;
        }
        SetEvent(h);
        CloseHandle(h);
        true
    }
}
