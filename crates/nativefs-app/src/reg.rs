//! Tiny registry helpers: writes go to HKCU only (no admin): autostart, link router, context menus.
//! Reads may look at HKLM too (browsers installed for all users).

use windows::core::HSTRING;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteKeyValueW, RegDeleteTreeW, RegEnumKeyExW, RegEnumValueW, RegGetValueW, RegOpenKeyExW,
    RegSetKeyValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
};

/// Where a read looks.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Root {
    User,
    Machine,
}

impl Root {
    fn hkey(self) -> HKEY {
        match self {
            Root::User => HKEY_CURRENT_USER,
            Root::Machine => HKEY_LOCAL_MACHINE,
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Sets a string value (`name = ""` is the key's default value); creates the key when missing.
pub fn set_str(key: &str, name: &str, value: &str) -> Result<(), String> {
    let data = wide(value);
    let err = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(name),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
    };
    if err == ERROR_SUCCESS { Ok(()) } else { Err(format!("registry write {key}\\{name}: error {}", err.0)) }
}

pub fn get_str(key: &str, name: &str) -> Option<String> {
    get_str_in(Root::User, key, name)
}

/// A string value (REG_EXPAND_SZ comes back expanded).
pub fn get_str_in(root: Root, key: &str, name: &str) -> Option<String> {
    let (k, n) = (HSTRING::from(key), HSTRING::from(name));
    let mut size = 0u32;
    let err = unsafe { RegGetValueW(root.hkey(), &k, &n, RRF_RT_REG_SZ, None, None, Some(&mut size)) };
    if err != ERROR_SUCCESS {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    let err = unsafe {
        RegGetValueW(root.hkey(), &k, &n, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
    };
    if err != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Names of a key's subkeys (or of its values with `values = true`); empty when the key is missing.
pub fn names(root: Root, key: &str, values: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut h = HKEY::default();
    if unsafe { RegOpenKeyExW(root.hkey(), &HSTRING::from(key), None, KEY_READ, &mut h) } != ERROR_SUCCESS {
        return out;
    }
    for i in 0.. {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let err = unsafe {
            if values {
                RegEnumValueW(h, i, Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None)
            } else {
                RegEnumKeyExW(h, i, Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None)
            }
        };
        if err != ERROR_SUCCESS {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    unsafe {
        let _ = RegCloseKey(h);
    }
    out
}

pub fn get_bin(key: &str, name: &str) -> Option<Vec<u8>> {
    let (k, n) = (HSTRING::from(key), HSTRING::from(name));
    let mut size = 0u32;
    let err = unsafe { RegGetValueW(HKEY_CURRENT_USER, &k, &n, RRF_RT_REG_BINARY, None, None, Some(&mut size)) };
    if err != ERROR_SUCCESS {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let err = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, &k, &n, RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
    };
    (err == ERROR_SUCCESS).then(|| {
        buf.truncate(size as usize);
        buf
    })
}

/// Deletes one value; a missing value is not an error.
pub fn delete_value(key: &str, name: &str) {
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name));
    }
}

/// Deletes a key with everything below it; a missing key is not an error.
pub fn delete_tree(key: &str) {
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key));
    }
}
