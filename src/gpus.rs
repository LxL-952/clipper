//! Toutes les cartes graphiques (NVIDIA, AMD, Intel, puce intégrée comprise) : liste par DXGI, utilisation et
//! mémoire par les compteurs Windows « GPU Engine » / « GPU Adapter Memory » (ceux du Gestionnaire des tâches).
//! Lecture seule, rien n'est ouvert dans un autre processus.

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct Adapter {
    /// Nom complet, ex. « NVIDIA GeForce RTX 4060 Laptop GPU », « AMD Radeon(TM) 780M ».
    pub name: String,
    /// Nom court pour l'overlay, ex. « 4060 », « 780M ».
    pub short: String,
    /// LUID Windows (haut, bas) : relie la carte aux compteurs.
    luid: (i32, u32),
    /// Mémoire dédiée et partagée, en octets.
    dedicated: u64,
    shared: u64,
}

/// « RTX 4060 Laptop GPU » -> « 4060 » : premier mot qui contient un chiffre, sinon le dernier mot.
fn short_name(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    words.iter().find(|w| w.chars().any(|c| c.is_ascii_digit())).or(words.last()).map_or(String::new(), |w| w.to_string())
}

/// Cartes matérielles (le rendu logiciel « Microsoft Basic Render » est écarté), dans l'ordre de Windows.
pub fn list() -> &'static [Adapter] {
    static LIST: OnceLock<Vec<Adapter>> = OnceLock::new();
    LIST.get_or_init(|| unsafe { enum_dxgi() }.unwrap_or_default())
}

#[repr(C)]
struct Desc1 {
    description: [u16; 128],
    vendor: u32,
    device: u32,
    subsys: u32,
    revision: u32,
    dedicated_video: usize,
    dedicated_system: usize,
    shared_system: usize,
    luid_low: u32,
    luid_high: i32,
    flags: u32,
}

/// IDXGIFactory1::EnumAdapters1 puis IDXGIAdapter1::GetDesc1, appelés par leur table de fonctions.
unsafe fn enum_dxgi() -> Option<Vec<Adapter>> {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    type Vt = *const *const usize;
    let lib: Vec<u16> = "dxgi.dll".encode_utf16().chain(Some(0)).collect();
    unsafe {
        let h = LoadLibraryW(lib.as_ptr());
        if h.is_null() {
            return None;
        }
        let create: unsafe extern "system" fn(*const windows_sys::core::GUID, *mut *mut core::ffi::c_void) -> i32 =
            std::mem::transmute(GetProcAddress(h, b"CreateDXGIFactory1\0".as_ptr())?);
        let iid = windows_sys::core::GUID::from_u128(0x770aae78_f26f_4dba_a829_253c83d1b387);
        let mut f = std::ptr::null_mut();
        if create(&iid, &mut f) < 0 || f.is_null() {
            return None;
        }
        let fv = *(f as Vt);
        let release = |o: *mut core::ffi::c_void| {
            let r: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32 = std::mem::transmute(*(*(o as Vt)).add(2));
            r(o);
        };
        let enum1: unsafe extern "system" fn(*mut core::ffi::c_void, u32, *mut *mut core::ffi::c_void) -> i32 = std::mem::transmute(*fv.add(12));
        let mut out = vec![];
        for i in 0..16 {
            let mut a = std::ptr::null_mut();
            if enum1(f, i, &mut a) < 0 || a.is_null() {
                break;
            }
            let get: unsafe extern "system" fn(*mut core::ffi::c_void, *mut Desc1) -> i32 = std::mem::transmute(*(*(a as Vt)).add(10));
            let mut d: Desc1 = std::mem::zeroed();
            if get(a, &mut d) >= 0 && d.flags & 2 == 0 && d.vendor != 0x1414 {
                let n = d.description.iter().position(|&c| c == 0).unwrap_or(128);
                let name = String::from_utf16_lossy(&d.description[..n]).trim().to_string();
                out.push(Adapter { short: short_name(&name), name, luid: (d.luid_high, d.luid_low), dedicated: d.dedicated_video as u64, shared: d.shared_system as u64 });
            }
            release(a);
        }
        release(f);
        Some(out)
    }
}

/// « …luid_0x00000000_0x0000D1F4_phys_0… » -> (0, 0xD1F4).
fn luid_of(instance: &str) -> Option<(i32, u32)> {
    let i = instance.find("luid_0x")?;
    let mut it = instance[i + 5..].split('_');
    let hi = u32::from_str_radix(it.next()?.trim_start_matches("0x"), 16).ok()? as i32;
    let lo = u32::from_str_radix(it.next()?.trim_start_matches("0x"), 16).ok()?;
    Some((hi, lo))
}

/// Requête PDH ouverte une fois : utilisation par moteur, mémoire dédiée et partagée de chaque carte.
pub struct Counters {
    query: *mut core::ffi::c_void,
    engine: *mut core::ffi::c_void,
    dedicated: *mut core::ffi::c_void,
    shared: *mut core::ffi::c_void,
}

/// Mesure d'une carte : utilisation en %, (mémoire utilisée, totale) en octets.
#[derive(Clone, Copy, Default, Debug)]
pub struct Sample {
    pub util: Option<f64>,
    pub mem: Option<(u64, u64)>,
}

impl Counters {
    pub fn open() -> Option<Self> {
        use windows_sys::Win32::System::Performance::*;
        let path = |p: &str| p.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        unsafe {
            let mut q = std::ptr::null_mut();
            if PdhOpenQueryW(std::ptr::null(), 0, &mut q) != 0 {
                return None;
            }
            let (mut e, mut d, mut s) = (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            let ok = PdhAddEnglishCounterW(q, path(r"\GPU Engine(*)\Utilization Percentage").as_ptr(), 0, &mut e) == 0;
            PdhAddEnglishCounterW(q, path(r"\GPU Adapter Memory(*)\Dedicated Usage").as_ptr(), 0, &mut d);
            PdhAddEnglishCounterW(q, path(r"\GPU Adapter Memory(*)\Shared Usage").as_ptr(), 0, &mut s);
            if !ok {
                PdhCloseQuery(q);
                return None;
            }
            PdhCollectQueryData(q); // 1re mesure : l'utilisation se calcule entre deux relevés
            Some(Self { query: q, engine: e, dedicated: d, shared: s })
        }
    }

    /// (instance, valeur) de chaque instance d'un compteur.
    unsafe fn values(c: *mut core::ffi::c_void, fmt: u32) -> Vec<(String, f64)> {
        use windows_sys::Win32::System::Performance::*;
        if c.is_null() {
            return vec![];
        }
        unsafe {
            let (mut size, mut n) = (0u32, 0u32);
            PdhGetFormattedCounterArrayW(c, fmt, &mut size, &mut n, std::ptr::null_mut());
            if size == 0 {
                return vec![];
            }
            let mut buf = vec![0u8; size as usize];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(c, fmt, &mut size, &mut n, items) != 0 {
                return vec![];
            }
            (0..n as usize)
                .map(|i| {
                    let it = &*items.add(i);
                    let len = (0..).take_while(|&k| *it.szName.add(k) != 0).count();
                    let name = String::from_utf16_lossy(std::slice::from_raw_parts(it.szName, len));
                    let v = if fmt == PDH_FMT_LARGE { it.FmtValue.Anonymous.largeValue as f64 } else { it.FmtValue.Anonymous.doubleValue };
                    (name, v)
                })
                .collect()
        }
    }

    /// Une mesure de chaque carte. Utilisation = moteur le plus chargé (3D, copie, vidéo…), somme de tous
    /// les processus : même calcul que le Gestionnaire des tâches.
    pub fn sample(&self) -> HashMap<(i32, u32), Sample> {
        use windows_sys::Win32::System::Performance::*;
        let mut out: HashMap<(i32, u32), Sample> = HashMap::new();
        unsafe {
            PdhCollectQueryData(self.query);
            let mut engines: HashMap<((i32, u32), String), f64> = HashMap::new();
            for (inst, v) in Self::values(self.engine, PDH_FMT_DOUBLE) {
                // « pid_1234_luid_…_phys_0_eng_3_engtype_3D » : regroupé par carte et par moteur.
                let (Some(l), Some(e)) = (luid_of(&inst), inst.find("_eng_").map(|i| inst[i..].to_string())) else { continue };
                *engines.entry((l, e)).or_default() += v;
            }
            for ((l, _), v) in engines {
                let s = out.entry(l).or_default();
                s.util = Some(s.util.unwrap_or(0.0).max(v.min(100.0)));
            }
            let ded: HashMap<_, _> = Self::values(self.dedicated, PDH_FMT_LARGE).into_iter().filter_map(|(i, v)| Some((luid_of(&i)?, v as u64))).collect();
            let sh: HashMap<_, _> = Self::values(self.shared, PDH_FMT_LARGE).into_iter().filter_map(|(i, v)| Some((luid_of(&i)?, v as u64))).collect();
            for a in list() {
                // Puce intégrée (moins de 1 Go dédié) : sa mémoire est surtout la RAM partagée.
                let m = if a.dedicated >= 1 << 30 { ded.get(&a.luid).map(|u| (*u, a.dedicated)) } else { sh.get(&a.luid).map(|u| (*u + ded.get(&a.luid).copied().unwrap_or(0), a.shared + a.dedicated)) };
                out.entry(a.luid).or_default().mem = m;
            }
        }
        out
    }
}

impl Adapter {
    pub fn luid(&self) -> (i32, u32) {
        self.luid
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_and_luids() {
        assert_eq!(super::short_name("NVIDIA GeForce RTX 4060 Laptop GPU"), "4060");
        assert_eq!(super::short_name("AMD Radeon(TM) 780M"), "780M");
        assert_eq!(super::luid_of("pid_42_luid_0x00000000_0x0000D1F4_phys_0_eng_3_engtype_3D"), Some((0, 0xD1F4)));
        assert_eq!(super::luid_of("luid_0x00000000_0x00011A2B_phys_0"), Some((0, 0x11A2B)));
        assert_eq!(super::luid_of("rien"), None);
    }
}
