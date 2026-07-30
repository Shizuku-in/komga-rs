//! Language-aware title sorting keys.

use std::sync::OnceLock;

use jieba_rs::Jieba;
use komga_core::{AlternateTitle, SeriesMetadata, TitleSortSource};
use lindera::{dictionary::load_dictionary, mode::Mode, segmenter::Segmenter, tokenizer::Tokenizer};
use pinyin::Converter;
use unicode_normalization::UnicodeNormalization;
use wana_kana::{ConvertJapanese, IsJapaneseChar, IsJapaneseStr, Options};

static CHINESE_TOKENIZER: OnceLock<Jieba> = OnceLock::new();
static JAPANESE_TOKENIZER: OnceLock<Result<Tokenizer, String>> = OnceLock::new();

const JAPANESE_HIRAGANA_LABEL: &str = "ja-Hira";
const JAPANESE_KATAKANA_LABEL: &str = "ja-Kana";

/// Builds a normalized sorting key for a title and BCP 47 language tag.
///
/// Chinese primary language tags use dictionary-segmented, toneless Mandarin
/// pinyin with syllables joined inside each word. Traditional Chinese is
/// simplified for segmentation and pronunciation lookup only. Japanese
/// primary language tags use embedded IPADIC readings converted to Hepburn
/// romaji. Other languages retain the normalized title. A Japanese tokenizer
/// failure is logged and also falls back to the normalized title.
pub fn generate_title_sort(title: &str, language: &str) -> String {
    let normalized = normalize(title);
    let generated = match primary_language(language).to_ascii_lowercase().as_str() {
        "zh" => Ok(chinese_title_sort(&normalized)),
        "ja" => japanese_title_sort(&normalized),
        _ => Ok(normalized.clone()),
    };
    match generated {
        Ok(value) => normalize(&value),
        Err(error) => {
            tracing::warn!(%error, title = normalized, language, "could not generate title sorting key");
            normalized
        }
    }
}

/// Applies automatic series sort-title policy after a metadata update.
///
/// An explicit API or provider sort title always wins for the current update.
/// Otherwise an unlocked legacy/default or previously generated value is
/// regenerated from the updated title and language. Other unlocked values are
/// marked custom and preserved.
pub fn apply_series_title_sort(previous: &SeriesMetadata, updated: &mut SeriesMetadata, explicit_title_sort: bool) {
    if explicit_title_sort {
        updated.title_sort_source = TitleSortSource::Custom;
        return;
    }
    if updated.title_sort_lock {
        return;
    }

    let previous_generated = generate_title_sort(&previous.title, &previous.language);
    if previous.title_sort_source == TitleSortSource::Generated
        || previous.title_sort == previous.title
        || previous.title_sort == previous_generated
    {
        updated.title_sort = generate_title_sort(&updated.title, &updated.language);
        updated.title_sort_source = TitleSortSource::Generated;
    } else {
        updated.title_sort_source = TitleSortSource::Custom;
    }
}

/// Regenerates Japanese kana readings in an unlocked alternate-title list.
///
/// IPADIC supplies a canonical katakana reading. Both compact katakana and
/// hiragana forms are persisted so either script can participate in the
/// existing alternate-title search. The standard `ja-Kana` and `ja-Hira`
/// labels identify entries owned by this policy; all other entries are
/// preserved. A locked alternate-title list is never changed.
pub fn apply_series_title_readings(metadata: &mut SeriesMetadata) {
    if metadata.alternate_titles_lock {
        return;
    }
    let readings = match generate_japanese_title_readings(&metadata.title, &metadata.language) {
        Ok(readings) => readings,
        Err(error) => {
            tracing::warn!(
                %error,
                title = metadata.title,
                language = metadata.language,
                "could not generate Japanese title readings"
            );
            return;
        }
    };
    metadata
        .alternate_titles
        .retain(|title| !matches!(title.label.as_str(), JAPANESE_KATAKANA_LABEL | JAPANESE_HIRAGANA_LABEL));

    if let Some((katakana, hiragana)) = readings {
        metadata.alternate_titles.extend([
            AlternateTitle {
                label: JAPANESE_KATAKANA_LABEL.into(),
                title: katakana,
            },
            AlternateTitle {
                label: JAPANESE_HIRAGANA_LABEL.into(),
                title: hiragana,
            },
        ]);
    }
}

fn normalize(value: &str) -> String {
    value
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn primary_language(language: &str) -> &str {
    language.split('-').next().unwrap_or_default()
}

fn chinese_title_sort(title: &str) -> String {
    let simplified = fast2s::convert(title);
    CHINESE_TOKENIZER
        .get_or_init(Jieba::new)
        .cut(&simplified, false)
        .into_iter()
        .map(|token| Converter::new(token.word).without_tone().to_string_with(""))
        .collect::<Vec<_>>()
        .join(" ")
}

fn japanese_title_sort(title: &str) -> Result<String, String> {
    Ok(japanese_reading_parts(title)?
        .iter()
        .map(|part| part.as_str().to_romaji())
        .collect::<Vec<_>>()
        .join(" "))
}

fn generate_japanese_title_readings(title: &str, language: &str) -> Result<Option<(String, String)>, String> {
    let title = normalize(title);
    if !primary_language(language).eq_ignore_ascii_case("ja") {
        return Ok(None);
    }

    let katakana = japanese_reading_parts(&title)?.concat();
    if !katakana.chars().any(|character| character.is_kana()) {
        return Ok(None);
    }
    let hiragana = katakana.as_str().to_hiragana_with_opt(Options {
        pass_romaji: true,
        keep_prolonged_sound_mark: true,
        ..Options::default()
    });
    Ok(Some((
        normalize(&katakana).replace(' ', ""),
        normalize(&hiragana).replace(' ', ""),
    )))
}

fn japanese_reading_parts(title: &str) -> Result<Vec<String>, String> {
    let tokenizer = JAPANESE_TOKENIZER
        .get_or_init(|| {
            let dictionary = load_dictionary("embedded://ipadic").map_err(|error| error.to_string())?;
            Ok(Tokenizer::new(Segmenter::new(Mode::Normal, dictionary, None)))
        })
        .as_ref()
        .map_err(Clone::clone)?;
    let mut tokens = tokenizer.tokenize(title).map_err(|error| error.to_string())?;
    Ok(tokens
        .iter_mut()
        .map(|token| {
            let surface = token.surface.to_string();
            if !surface.as_str().contains_kanji() && !surface.as_str().is_kana() {
                return surface;
            }
            let reading = token
                .details()
                .get(7)
                .copied()
                .filter(|reading| *reading != "*")
                .map(str::to_owned);
            match reading {
                Some(reading) => reading,
                None if surface.as_str().is_kana() => surface.as_str().to_katakana(),
                None => surface,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::DateTime;
    use komga_core::{ReadingDirection, SeriesStatus};

    use super::*;

    #[test]
    fn chinese_uses_word_segmented_toneless_pinyin_for_all_variants() {
        for (title, language, expected) in [
            ("重庆森林", "zh", "chongqing senlin"),
            ("重庆森林", "zh-Hans", "chongqing senlin"),
            ("重庆森林", "zh-Hant", "chongqing senlin"),
            ("重庆森林", "ZH-hant-TW", "chongqing senlin"),
            ("三国演义", "zh", "sanguoyanyi"),
            ("钢之炼金术师", "zh", "gang zhi lianjinshushi"),
            ("地球往事三部曲", "zh", "diqiu wangshi sanbuqu"),
            ("空之境界", "zh", "kong zhi jingjie"),
            ("玩玩的戀愛關係", "zh-Hant", "wanwan de lianai guanxi"),
            ("驚爆危機", "zh-TW", "jing bao weiji"),
            ("涼宮春日的消失", "ZH-hant-TW", "liang gong chunri de xiaoshi"),
            ("人類衰退之後", "zh-HK", "renlei shuaitui zhihou"),
            ("終將成為妳", "zh-Hant", "zhongjiang chengwei ni"),
        ] {
            assert_eq!(generate_title_sort(title, language), expected, "{title} ({language})");
        }
    }

    #[test]
    fn chinese_preserves_mixed_content_and_unknown_fragments() {
        assert_eq!(
            generate_title_sort("重庆森林 2024 ABC", "zh"),
            "chongqing senlin 2024 abc"
        );
        assert!(generate_title_sort("𠮷野家", "zh").starts_with('𠮷'));
    }

    #[test]
    fn japanese_uses_ipadic_readings_and_hepburn_romaji() {
        for (title, language, expected) in [
            ("進撃の巨人", "ja", "shingeki no kyojin"),
            ("進撃の巨人", "JA-Jpan-JP", "shingeki no kyojin"),
            ("カウボーイビバップ", "ja-JP", "kaubooibibappu"),
            ("雨の日のアイリス", "ja", "ame no hi no airisu"),
            ("安達としまむら", "ja", "adachi to shimamura"),
            ("冴えない彼女の育てかた", "ja", "sae nai kanojo no sodate kata"),
        ] {
            assert_eq!(generate_title_sort(title, language), expected, "{title} ({language})");
        }
    }

    #[test]
    fn japanese_readings_are_added_as_compact_alternate_titles() {
        let mut metadata = series("安達としまむら", "adachi to shimamura", TitleSortSource::Generated);
        metadata.language = "ja".into();

        apply_series_title_readings(&mut metadata);

        assert_eq!(
            metadata.alternate_titles,
            [
                AlternateTitle {
                    label: "ja-Kana".into(),
                    title: "アダチトシマムラ".into(),
                },
                AlternateTitle {
                    label: "ja-Hira".into(),
                    title: "あだちとしまむら".into(),
                },
            ]
        );
    }

    #[test]
    fn japanese_readings_require_a_japanese_language_and_honor_the_lock() {
        let mut metadata = series("雨の日", "雨の日", TitleSortSource::Legacy);
        metadata.alternate_titles.push(AlternateTitle {
            label: "en".into(),
            title: "A Rainy Day".into(),
        });
        apply_series_title_readings(&mut metadata);
        assert_eq!(
            metadata
                .alternate_titles
                .iter()
                .map(|title| title.label.as_str())
                .collect::<Vec<_>>(),
            ["en"]
        );

        metadata.language = "ja-JP".into();
        apply_series_title_readings(&mut metadata);
        assert_eq!(
            metadata
                .alternate_titles
                .iter()
                .map(|title| title.label.as_str())
                .collect::<Vec<_>>(),
            ["en", "ja-Kana", "ja-Hira"]
        );

        metadata.alternate_titles_lock = true;
        metadata.title = "進撃の巨人".into();
        let locked = metadata.alternate_titles.clone();
        apply_series_title_readings(&mut metadata);
        assert_eq!(metadata.alternate_titles, locked);
    }

    #[test]
    fn normalization_and_mixed_content_are_preserved() {
        assert_eq!(generate_title_sort("  ＡＢＣ　１２３  ", "en"), "abc 123");
        assert_eq!(generate_title_sort("第２巻 ABC", "ja"), "dai 2 kan abc");
        assert_eq!(generate_title_sort("進撃！ABC 2", "ja"), "shingeki ! abc 2");
        assert_eq!(generate_title_sort("  The   Title  ", ""), "the title");
        assert_eq!(generate_title_sort("Édition", "fr"), "édition");
    }

    #[test]
    fn unknown_japanese_fragments_retain_the_normalized_surface() {
        assert!(generate_title_sort("𠮷野家", "ja").starts_with('𠮷'));
    }

    #[test]
    fn series_policy_regenerates_automatic_values_and_preserves_custom_values() {
        let legacy = series("重庆", "重庆", TitleSortSource::Legacy);
        let mut updated = legacy.clone();
        updated.title = "重庆森林".into();
        updated.language = "zh".into();
        apply_series_title_sort(&legacy, &mut updated, false);
        assert_eq!(updated.title_sort, "chongqing senlin");
        assert_eq!(updated.title_sort_source, TitleSortSource::Generated);

        let mut generated_again = updated.clone();
        generated_again.title = "進撃の巨人".into();
        generated_again.language = "ja".into();
        apply_series_title_sort(&updated, &mut generated_again, false);
        assert_eq!(generated_again.title_sort, "shingeki no kyojin");
        assert_eq!(generated_again.title_sort_source, TitleSortSource::Generated);

        let custom = series("重庆", "my preferred order", TitleSortSource::Legacy);
        let mut custom_updated = custom.clone();
        custom_updated.language = "zh".into();
        apply_series_title_sort(&custom, &mut custom_updated, false);
        assert_eq!(custom_updated.title_sort, "my preferred order");
        assert_eq!(custom_updated.title_sort_source, TitleSortSource::Custom);
    }

    #[test]
    fn series_policy_honors_locks_explicit_values_and_unlocking_generated_values() {
        let mut locked = series("進撃の巨人", "protected", TitleSortSource::Generated);
        locked.title_sort_lock = true;
        let mut locked_updated = locked.clone();
        locked_updated.language = "ja".into();
        apply_series_title_sort(&locked, &mut locked_updated, false);
        assert_eq!(locked_updated.title_sort, "protected");

        let legacy = series("進撃の巨人", "進撃の巨人", TitleSortSource::Legacy);
        let mut explicit = legacy.clone();
        explicit.title_sort = "giant attack".into();
        apply_series_title_sort(&legacy, &mut explicit, true);
        assert_eq!(explicit.title_sort, "giant attack");
        assert_eq!(explicit.title_sort_source, TitleSortSource::Custom);

        let mut generated_locked = series("進撃の巨人", "stale", TitleSortSource::Generated);
        generated_locked.title_sort_lock = true;
        let mut unlocked = generated_locked.clone();
        unlocked.title_sort_lock = false;
        unlocked.language = "ja".into();
        apply_series_title_sort(&generated_locked, &mut unlocked, false);
        assert_eq!(unlocked.title_sort, "shingeki no kyojin");
    }

    fn series(title: &str, title_sort: &str, source: TitleSortSource) -> SeriesMetadata {
        let timestamp = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .naive_utc();
        SeriesMetadata {
            series_id: "series".into(),
            status: SeriesStatus::Ongoing,
            status_lock: false,
            title: title.into(),
            title_lock: false,
            title_sort: title_sort.into(),
            title_sort_lock: false,
            title_sort_source: source,
            summary: String::new(),
            summary_lock: false,
            reading_direction: None::<ReadingDirection>,
            reading_direction_lock: false,
            publisher: String::new(),
            publisher_lock: false,
            age_rating: None,
            age_rating_lock: false,
            language: String::new(),
            language_lock: false,
            genres: BTreeSet::new(),
            genres_lock: false,
            tags: BTreeSet::new(),
            tags_lock: false,
            total_book_count: None,
            total_book_count_lock: false,
            sharing_labels: BTreeSet::new(),
            sharing_labels_lock: false,
            links: Vec::new(),
            links_lock: false,
            alternate_titles: Vec::new(),
            alternate_titles_lock: false,
            created_date: timestamp,
            last_modified_date: timestamp,
        }
    }
}
