//! Chinese or English, following the Windows display language.

use std::sync::OnceLock;

use windows::Win32::Globalization::GetUserDefaultUILanguage;

use crate::manifest::Names;

static CHINESE: OnceLock<bool> = OnceLock::new();

pub fn chinese() -> bool {
    // SAFETY: GetUserDefaultUILanguage takes no arguments and only returns a language id.
    *CHINESE.get_or_init(|| unsafe { GetUserDefaultUILanguage() } & 0x3FF == 0x04)
}

/// Picks the text for the current display language.
pub fn tr(zh: &'static str, en: &'static str) -> &'static str {
    if chinese() { zh } else { en }
}

pub fn name(names: &Names) -> &str {
    if chinese() { &names.zh } else { &names.en }
}

/// What the product calls its user data, lower case in English.
pub fn data_label(product: &crate::manifest::Product) -> String {
    match &product.data_label {
        Some(label) => name(label).to_string(),
        None => tr("设置和数据", "settings and data").to_string(),
    }
}

/// Starts an English sentence with a capital letter; Chinese text is unchanged.
pub fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}
