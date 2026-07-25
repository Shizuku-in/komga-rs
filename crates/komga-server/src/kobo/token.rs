use base64::{Engine, engine::general_purpose};
use komga_core::KomgaSyncToken;

const KOMGA_TOKEN_PREFIX: &str = "KOMGA.";

pub(super) fn decode(value: Option<&str>) -> KomgaSyncToken {
    let Some(value) = value else {
        return KomgaSyncToken::initial();
    };
    if let Some(encoded) = value.strip_prefix(KOMGA_TOKEN_PREFIX) {
        return decode_base64(encoded)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
    }
    if !value.contains('.') {
        if let Some(raw) = decode_base64(value)
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|json| {
                json.pointer("/data/raw_kobo_store_token")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
        {
            return KomgaSyncToken {
                raw_kobo_sync_token: raw,
                ..KomgaSyncToken::default()
            };
        }
        return KomgaSyncToken::default();
    }
    KomgaSyncToken {
        raw_kobo_sync_token: value.to_owned(),
        ..KomgaSyncToken::default()
    }
}

pub(super) fn encode(token: &KomgaSyncToken) -> String {
    let json = serde_json::to_vec(token).expect("Komga sync tokens are serializable");
    format!("{KOMGA_TOKEN_PREFIX}{}", general_purpose::STANDARD_NO_PAD.encode(json))
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    general_purpose::STANDARD_NO_PAD
        .decode(value)
        .or_else(|_| general_purpose::STANDARD.decode(value))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_vectors_cover_komga_calibre_store_and_invalid_values() {
        let token = KomgaSyncToken {
            raw_kobo_sync_token: "store.token".into(),
            ongoing_sync_point_id: Some("ongoing".into()),
            last_successful_sync_point_id: Some("last".into()),
            ..KomgaSyncToken::default()
        };
        assert_eq!(decode(Some(&encode(&token))), token);

        let calibre = general_purpose::STANDARD.encode(br#"{"data":{"raw_kobo_store_token":"official.token"}}"#);
        assert_eq!(decode(Some(&calibre)).raw_kobo_sync_token, "official.token");
        assert_eq!(decode(Some("one.two")).raw_kobo_sync_token, "one.two");
        assert_eq!(decode(Some("KOMGA.not-base64")), KomgaSyncToken::default());
        assert_eq!(decode(Some("not-base64")), KomgaSyncToken::default());
    }
}
