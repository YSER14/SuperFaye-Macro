use std::collections::HashSet;

use serde::Serialize;
use windows::Win32::Foundation::E_UNEXPECTED;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory, IDWriteLocalizedStrings,
};
use windows::core::{Error, PCWSTR, Result, w};

#[derive(Serialize)]
pub(super) struct SystemFont {
    name: String,
    label: String,
}

pub(super) fn list_system_fonts() -> Result<Vec<SystemFont>> {
    let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
    let mut collection = None;
    unsafe { factory.GetSystemFontCollection(&mut collection, true)? };
    let collection = collection.ok_or_else(|| Error::from_hresult(E_UNEXPECTED))?;
    let mut fonts = Vec::new();
    let mut names_seen = HashSet::new();

    for index in 0..unsafe { collection.GetFontFamilyCount() } {
        let names = unsafe { collection.GetFontFamily(index)?.GetFamilyNames()? };
        if unsafe { names.GetCount() } == 0 {
            continue;
        }
        let name = match localized_name(&names, w!("en-us"))? {
            Some(english) => english,
            None => read_name(&names, 0)?,
        };
        if name.is_empty() || !names_seen.insert(name.to_lowercase()) {
            continue;
        }

        let mut label = name.clone();
        for locale in [
            w!("zh-cn"),
            w!("zh-hans"),
            w!("zh-tw"),
            w!("zh-hant"),
            w!("zh-hk"),
        ] {
            if let Some(localized) = localized_name(&names, locale)? {
                label = localized;
                break;
            }
        }
        fonts.push(SystemFont { name, label });
    }
    Ok(fonts)
}

fn localized_name(names: &IDWriteLocalizedStrings, locale: PCWSTR) -> Result<Option<String>> {
    let mut index = 0;
    let mut exists = Default::default();
    unsafe { names.FindLocaleName(locale, &mut index, &mut exists)? };
    exists
        .as_bool()
        .then(|| read_name(names, index))
        .transpose()
}

fn read_name(names: &IDWriteLocalizedStrings, index: u32) -> Result<String> {
    let length = unsafe { names.GetStringLength(index)? } as usize;
    let mut buffer = vec![0; length + 1];
    unsafe { names.GetString(index, &mut buffer)? };
    Ok(String::from_utf16_lossy(&buffer[..length]))
}
