use fluent_bundle::{FluentBundle, FluentResource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Available languages with their display names and flag emojis.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Language {
    English,
    German,
    French,
    Chinese,
    Japanese,
}

impl Language {
    pub fn locale_id(&self) -> &'static str {
        match self {
            Language::English => "en-US",
            Language::German => "de-DE",
            Language::French => "fr-FR",
            Language::Chinese => "zh-CN",
            Language::Japanese => "ja-JP",
        }
    }

    pub fn flag(&self) -> &'static str {
        match self {
            Language::English => "🇺🇸",
            Language::German => "🇩🇪",
            Language::French => "🇫🇷",
            Language::Chinese => "🇨🇳",
            Language::Japanese => "🇯🇵",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Language::English => "English",
            Language::German => "Deutsch",
            Language::French => "Français",
            Language::Chinese => "中文",
            Language::Japanese => "日本語",
        }
    }

    pub fn all() -> Vec<Language> {
        vec![
            Language::English,
            Language::German,
            Language::French,
            Language::Chinese,
            Language::Japanese,
        ]
    }

    pub fn from_locale(locale: &str) -> Language {
        match locale {
            "de" | "de-DE" => Language::German,
            "fr" | "fr-FR" => Language::French,
            "zh" | "zh-CN" => Language::Chinese,
            "ja" | "ja-JP" => Language::Japanese,
            _ => Language::English,
        }
    }
}

impl Default for Language {
    fn default() -> Self {
        Language::English
    }
}

/// Holds compiled Fluent bundles per language.
#[derive(Default)]
pub struct I18n {
    bundles: RwLock<HashMap<Language, Arc<FluentBundle<FluentResource>>>>,
    current: RwLock<Language>,
}

impl I18n {
    pub fn new() -> Self {
        let mut bundles = HashMap::new();
        for lang in Language::all() {
            if let Some(bundle) = load_bundle(lang.clone()) {
                bundles.insert(lang, Arc::new(bundle));
            }
        }
        // Always ensure English exists as fallback
        if !bundles.contains_key(&Language::English) {
            let bundle = FluentBundle::new(vec!["en-US".parse().unwrap()]);
            bundles.insert(Language::English, Arc::new(bundle));
        }
        Self {
            bundles: RwLock::new(bundles),
            current: RwLock::new(Language::default()),
        }
    }

    pub fn set_language(&self, lang: Language) {
        *self.current.write().unwrap() = lang;
    }

    pub fn language(&self) -> Language {
        *self.current.read().unwrap()
    }

    pub fn t(&self, key: &str, args: Option<&HashMap<String, fluent::FluentValue>>) -> String {
        let lang = self.language();
        let bundle = self
            .bundles
            .read()
            .unwrap()
            .get(&lang)
            .cloned()
            .unwrap_or_else(|| {
                self.bundles
                    .read()
                    .unwrap()
                    .get(&Language::English)
                    .cloned()
                    .unwrap()
            });

        let Some(message) = bundle.get_message(key) else {
            return key.to_string();
        };
        let Some(pattern) = message.value() else {
            return key.to_string();
        };

        let mut errors = vec![];
        let mut fluent_args = fluent::FluentArgs::new();
        if let Some(a) = args {
            for (k, v) in a {
                fluent_args.set(k, v.clone());
            }
        }
        let value = bundle.format_pattern(pattern, Some(&mut fluent_args), &mut errors);
        if !errors.is_empty() {
            return key.to_string();
        }
        value.to_string()
    }
}

fn load_bundle(lang: Language) -> Option<FluentBundle<FluentResource>> {
    let (locale_id_str, content) = match lang {
        Language::English => ("en-US", include_str!("../resources/locales/en-US.ftl")),
        Language::German => ("de-DE", include_str!("../resources/locales/de-DE.ftl")),
        Language::French => ("fr-FR", include_str!("../resources/locales/fr-FR.ftl")),
        Language::Chinese => ("zh-CN", include_str!("../resources/locales/zh-CN.ftl")),
        Language::Japanese => ("ja-JP", include_str!("../resources/locales/ja-JP.ftl")),
    };
    let res = FluentResource::try_new(content.to_string()).ok()?;
    let locale_id_for_bundle = locale_id_str.parse().ok()?;
    let mut bundle = FluentBundle::new(vec![locale_id_for_bundle]);
    bundle.set_use_isolating(false);
    bundle.add_resource(res).ok()?;
    Some(bundle)
}

pub fn negotiate_best_language(requested: &[String]) -> Language {
    // Match the requested locale tags against the ones we ship. The Fluent
    // language matcher is overkill here, so a simple prefix match keeps the
    // dependency surface small and the startup fast.
    for tag in requested {
        let tag = tag.split([';', '-', '_']).next().unwrap_or(tag);
        match tag {
            "de" => return Language::German,
            "fr" => return Language::French,
            "zh" => return Language::Chinese,
            "ja" => return Language::Japanese,
            "en" => return Language::English,
            _ => {}
        }
    }
    Language::English
}

#[macro_export]
macro_rules! t {
    ($i18n:expr, $key:expr) => {
        $i18n.t($key, None)
    };
    ($i18n:expr, $key:expr, $($arg:expr),* $(,)?) => {
        {
            use std::collections::HashMap;
            let mut args = HashMap::new();
            $({
                let (k, v) = $arg;
                args.insert(k.to_string(), v);
            })*
            $i18n.t($key, Some(&args))
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    const FTL_SOURCES: &[(&str, &str)] = &[
        ("en-US", include_str!("../resources/locales/en-US.ftl")),
        ("de-DE", include_str!("../resources/locales/de-DE.ftl")),
        ("fr-FR", include_str!("../resources/locales/fr-FR.ftl")),
        ("zh-CN", include_str!("../resources/locales/zh-CN.ftl")),
        ("ja-JP", include_str!("../resources/locales/ja-JP.ftl")),
    ];

    const REQUIRED_KEYS: &[&str] = &[
        "settings",
        "preferences",
        "downloads",
        "save-location",
        "browse",
        "open",
        "auto-retry",
        "max-retries",
        "auto-save-rules",
        "delete-rule",
        "enable-rule",
        "appearance",
        "language",
        "interface-language",
        "notifications-tray",
        "storage",
        "clear-cache",
        "import-export",
        "about",
        "open-doujinstyle",
        "show-shortcuts",
    ];

    fn bundle_from(source: &str) -> FluentBundle<FluentResource> {
        let resource = match FluentResource::try_new(source.to_string()) {
            Ok(resource) => resource,
            Err((_, errors)) => panic!("invalid Fluent syntax: {errors:?}"),
        };
        let mut bundle = FluentBundle::new(vec!["en-US".parse().unwrap()]);
        if let Err(errors) = bundle.add_resource(resource) {
            panic!("invalid Fluent resource: {errors:?}");
        }
        bundle
    }

    #[test]
    fn every_locale_has_required_keys() {
        for (locale, source) in FTL_SOURCES {
            let bundle = bundle_from(source);
            for key in REQUIRED_KEYS {
                assert!(
                    bundle.get_message(key).is_some(),
                    "{locale} is missing key `{key}`"
                );
            }
        }
    }

    #[test]
    fn translates_a_sample_key_in_every_language() {
        let i18n = I18n::new();
        for lang in Language::all() {
            i18n.set_language(lang);
            let value = i18n.t("settings", None);
            assert_ne!(value, "settings", "no translation for {lang:?}");
        }
    }

    fn message_ids(source: &str) -> Vec<String> {
        source
            .lines()
            .map(str::trim)
            .filter(|line| {
                !line.is_empty() && !line.starts_with('#') && !line.starts_with('.')
            })
            .filter_map(|line| line.split_once('=').map(|(id, _)| id.trim().to_string()))
            .filter(|id| !id.is_empty())
            .collect()
    }

    #[test]
    fn every_locale_has_all_english_keys() {
        let english = message_ids(FTL_SOURCES[0].1);
        for (locale, source) in &FTL_SOURCES[1..] {
            let ids = message_ids(source);
            let missing: Vec<_> = english.iter().filter(|id| !ids.contains(id)).collect();
            assert!(missing.is_empty(), "{locale} is missing keys: {missing:?}");
        }
    }
}
