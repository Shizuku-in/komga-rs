//! Bundled and local font discovery and delivery.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use axum::{
    Json,
    body::Body,
    extract::{Path as AxumPath, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

use crate::{AppState, book_file_routes::content_disposition, error::ApiError};

const URL_PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b':')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

const EMBEDDED_OPEN_DYSLEXIC: [(&str, &[u8]); 8] = [
    (
        "OpenDyslexic-Bold-Italic.woff",
        include_bytes!("../assets/fonts/OpenDyslexic/OpenDyslexic-Bold-Italic.woff"),
    ),
    (
        "OpenDyslexic-Bold-Italic.woff2",
        include_bytes!("../assets/fonts/OpenDyslexic/OpenDyslexic-Bold-Italic.woff2"),
    ),
    (
        "OpenDyslexic-Bold.woff",
        include_bytes!("../assets/fonts/OpenDyslexic/OpenDyslexic-Bold.woff"),
    ),
    (
        "OpenDyslexic-Bold.woff2",
        include_bytes!("../assets/fonts/OpenDyslexic/OpenDyslexic-Bold.woff2"),
    ),
    (
        "OpenDyslexic-Italic.woff",
        include_bytes!("../assets/fonts/OpenDyslexic/OpenDyslexic-Italic.woff"),
    ),
    (
        "OpenDyslexic-Italic.woff2",
        include_bytes!("../assets/fonts/OpenDyslexic/OpenDyslexic-Italic.woff2"),
    ),
    (
        "OpenDyslexic-Regular.woff",
        include_bytes!("../assets/fonts/OpenDyslexic/OpenDyslexic-Regular.woff"),
    ),
    (
        "OpenDyslexic-Regular.woff2",
        include_bytes!("../assets/fonts/OpenDyslexic/OpenDyslexic-Regular.woff2"),
    ),
];

#[derive(Debug, Clone)]
enum FontSource {
    Embedded(&'static [u8]),
    File { path: PathBuf, root: PathBuf },
}

#[derive(Debug, Clone)]
struct FontResource {
    name: String,
    extension: String,
    source: FontSource,
}

impl FontResource {
    async fn bytes(&self) -> Option<Vec<u8>> {
        match &self.source {
            FontSource::Embedded(bytes) => Some(bytes.to_vec()),
            FontSource::File { path, root } => {
                let metadata = tokio::fs::symlink_metadata(path).await.ok()?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return None;
                }
                let canonical = tokio::fs::canonicalize(path).await.ok()?;
                if !canonical.starts_with(root) {
                    return None;
                }
                tokio::fs::read(canonical).await.ok()
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FontCatalog {
    families: BTreeMap<String, Vec<FontResource>>,
}

impl Default for FontCatalog {
    fn default() -> Self {
        let resources = EMBEDDED_OPEN_DYSLEXIC
            .iter()
            .map(|(name, bytes)| FontResource {
                name: (*name).to_owned(),
                extension: supported_extension(Path::new(name)).expect("embedded font extension must be supported"),
                source: FontSource::Embedded(bytes),
            })
            .collect();
        Self {
            families: BTreeMap::from([("OpenDyslexic".into(), resources)]),
        }
    }
}

impl FontCatalog {
    pub(crate) fn load(directory: &Path) -> Self {
        let mut catalog = Self::default();
        let Ok(metadata) = fs::symlink_metadata(directory) else {
            return catalog;
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            tracing::warn!(path = %directory.display(), "font directory is not a non-symlink directory");
            return catalog;
        }
        let Ok(root) = directory.canonicalize() else {
            tracing::warn!(path = %directory.display(), "could not canonicalize font directory");
            return catalog;
        };
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(%error, path = %root.display(), "could not read font directory");
                return catalog;
            }
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() || !file_type.is_dir() {
                continue;
            }
            let Some(family) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if family.is_empty() || family.chars().any(char::is_control) {
                continue;
            }
            let family_path = entry.path();
            let mut resources = match fs::read_dir(&family_path) {
                Ok(entries) => entries
                    .flatten()
                    .filter_map(|entry| local_resource(entry, &root))
                    .collect::<Vec<_>>(),
                Err(error) => {
                    tracing::warn!(%error, path = %family_path.display(), "could not read font family directory");
                    continue;
                }
            };
            resources.sort_by(|left, right| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
                    .then_with(|| left.name.cmp(&right.name))
            });
            if !resources.is_empty() {
                catalog.families.insert(family, resources);
            }
        }
        catalog
    }

    fn families(&self) -> Vec<String> {
        self.families.keys().cloned().collect()
    }

    fn resource(&self, family: &str, file_name: &str) -> Option<FontResource> {
        self.families
            .get(family)?
            .iter()
            .find(|resource| resource.name == file_name)
            .cloned()
    }

    fn css(&self, family: &str) -> Option<String> {
        let resources = self.families.get(family)?;
        let mut groups: Vec<(FontCharacteristics, Vec<&FontResource>)> = Vec::new();
        for resource in resources {
            let characteristics = FontCharacteristics::from_name(&resource.name);
            if let Some((_, group)) = groups.iter_mut().find(|(value, _)| *value == characteristics) {
                group.push(resource);
            } else {
                groups.push((characteristics, vec![resource]));
            }
        }
        let family = css_string(family);
        Some(
            groups
                .into_iter()
                .map(|(characteristics, resources)| {
                    let sources = resources
                        .into_iter()
                        .map(|resource| {
                            let file_name = utf8_percent_encode(&resource.name, URL_PATH_SEGMENT);
                            let format = match resource.extension.as_str() {
                                "ttf" => "truetype",
                                "otf" => "opentype",
                                value => value,
                            };
                            format!("url('{file_name}') format('{format}')")
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    format!(
                        "@font-face {{\n    font-family: '{family}';\n    src: {sources};\n    font-weight: {};\n    font-style: {};\n}}",
                        characteristics.weight, characteristics.style
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
        )
    }
}

fn local_resource(entry: fs::DirEntry, root: &Path) -> Option<FontResource> {
    let file_type = entry.file_type().ok()?;
    if file_type.is_symlink() || !file_type.is_file() {
        return None;
    }
    let name = entry.file_name().to_str()?.to_owned();
    if name.chars().any(char::is_control) {
        return None;
    }
    let path = entry.path();
    let extension = supported_extension(&path)?;
    let canonical = path.canonicalize().ok()?;
    if !canonical.starts_with(root) {
        return None;
    }
    Some(FontResource {
        name,
        extension,
        source: FontSource::File {
            path: canonical,
            root: root.to_owned(),
        },
    })
}

fn supported_extension(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    matches!(extension.as_str(), "woff" | "woff2" | "ttf" | "otf").then_some(extension)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FontCharacteristics {
    style: &'static str,
    weight: &'static str,
}

impl FontCharacteristics {
    fn from_name(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        Self {
            style: if name.contains("italic") { "italic" } else { "normal" },
            weight: if name.contains("bold") { "bold" } else { "normal" },
        }
    }
}

fn css_string(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '\'' => "\\'".chars().collect(),
            value if value.is_control() => "�".chars().collect(),
            value => vec![value],
        })
        .collect()
}

pub(crate) async fn families(State(state): State<AppState>) -> Json<Vec<String>> {
    Json(state.fonts.families())
}

pub(crate) async fn resource(
    State(state): State<AppState>,
    AxumPath((family, file_name)): AxumPath<(String, String)>,
) -> Result<Response, ApiError> {
    let resource = state
        .fonts
        .resource(&family, &file_name)
        .ok_or_else(|| ApiError::not_found("Font not found"))?;
    let bytes = resource
        .bytes()
        .await
        .ok_or_else(|| ApiError::not_found("Font not found"))?;
    binary_response(bytes, &resource.extension, &resource.name)
}

pub(crate) async fn css(
    State(state): State<AppState>,
    AxumPath(family): AxumPath<String>,
) -> Result<Response, ApiError> {
    let css = state
        .fonts
        .css(&family)
        .ok_or_else(|| ApiError::not_found("Font not found"))?;
    let file_name = format!("{family}.css");
    response(css.into_bytes(), "text/css", &file_name)
}

fn binary_response(bytes: Vec<u8>, extension: &str, file_name: &str) -> Result<Response, ApiError> {
    let media_type = match extension {
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        _ => "application/octet-stream",
    };
    response(bytes, media_type, file_name)
}

fn response(bytes: Vec<u8>, media_type: &'static str, file_name: &str) -> Result<Response, ApiError> {
    let length = bytes.len();
    let mut response = Body::from(bytes).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).map_err(|_| ApiError::internal("Could not encode font length"))?,
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        content_disposition(file_name).map_err(|_| ApiError::internal("Could not encode font filename"))?,
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn embedded_fonts_have_kotlin_bytes_and_css() {
        let catalog = FontCatalog::default();
        assert_eq!(catalog.families(), ["OpenDyslexic"]);
        let resource = catalog.resource("OpenDyslexic", "OpenDyslexic-Regular.woff2").unwrap();
        let FontSource::Embedded(bytes) = resource.source else {
            panic!("embedded font used a filesystem source");
        };
        assert_eq!(
            hex::encode(Sha256::digest(bytes)),
            "f007004af3cda5d8076e57c943f8cc8d00a0da25988b1ae1048683d60e7cac1a"
        );
        assert_eq!(
            catalog.css("OpenDyslexic").unwrap(),
            "@font-face {\n    font-family: 'OpenDyslexic';\n    src: url('OpenDyslexic-Bold-Italic.woff') format('woff'),url('OpenDyslexic-Bold-Italic.woff2') format('woff2');\n    font-weight: bold;\n    font-style: italic;\n}\n\n@font-face {\n    font-family: 'OpenDyslexic';\n    src: url('OpenDyslexic-Bold.woff') format('woff'),url('OpenDyslexic-Bold.woff2') format('woff2');\n    font-weight: bold;\n    font-style: normal;\n}\n\n@font-face {\n    font-family: 'OpenDyslexic';\n    src: url('OpenDyslexic-Italic.woff') format('woff'),url('OpenDyslexic-Italic.woff2') format('woff2');\n    font-weight: normal;\n    font-style: italic;\n}\n\n@font-face {\n    font-family: 'OpenDyslexic';\n    src: url('OpenDyslexic-Regular.woff') format('woff'),url('OpenDyslexic-Regular.woff2') format('woff2');\n    font-weight: normal;\n    font-style: normal;\n}"
        );
    }

    #[test]
    fn local_fonts_override_embedded_families_and_ignore_unsafe_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let family = temporary.path().join("OpenDyslexic");
        fs::create_dir_all(family.join("nested")).unwrap();
        fs::write(family.join("Local Bold.TTF"), b"local").unwrap();
        fs::write(family.join("ignored.txt"), b"ignored").unwrap();
        fs::write(family.join("nested/Nested.woff"), b"nested").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(family.join("Local Bold.TTF"), family.join("linked.woff")).unwrap();

        let catalog = FontCatalog::load(temporary.path());
        assert_eq!(catalog.families(), ["OpenDyslexic"]);
        assert!(catalog.resource("OpenDyslexic", "OpenDyslexic-Regular.woff").is_none());
        assert!(catalog.resource("OpenDyslexic", "linked.woff").is_none());
        assert!(catalog.resource("OpenDyslexic", "Nested.woff").is_none());
        assert_eq!(
            catalog.css("OpenDyslexic").unwrap(),
            "@font-face {\n    font-family: 'OpenDyslexic';\n    src: url('Local%20Bold.TTF') format('truetype');\n    font-weight: bold;\n    font-style: normal;\n}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_file_replaced_by_an_external_symlink_is_not_read() {
        let temporary = tempfile::tempdir().unwrap();
        let family = temporary.path().join("Custom");
        fs::create_dir_all(&family).unwrap();
        let font = family.join("Custom.woff");
        fs::write(&font, b"original").unwrap();
        let catalog = FontCatalog::load(temporary.path());
        let resource = catalog.resource("Custom", "Custom.woff").unwrap();
        let external = tempfile::NamedTempFile::new().unwrap();
        fs::remove_file(&font).unwrap();
        std::os::unix::fs::symlink(external.path(), &font).unwrap();

        assert!(resource.bytes().await.is_none());
    }
}
