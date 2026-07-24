//! ComicRack reading-list parsing.

use komga_core::{ReadListRequest, ReadListRequestBook};
use thiserror::Error;

/// A client-facing ComicRack reading-list validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ComicRackListError {
    /// XML could not be parsed safely.
    #[error("ERR_1015")]
    InvalidXml,
    /// No book entries were present.
    #[error("ERR_1029")]
    NoBooks,
    /// The reading-list name was absent or blank.
    #[error("ERR_1030")]
    MissingName,
    /// A book omitted its series or number.
    #[error("ERR_1031")]
    MissingBookField,
}

/// Parses a ComicRack `.cbl` document without resolving external resources.
pub fn parse_read_list(bytes: &[u8]) -> Result<ReadListRequest, ComicRackListError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ComicRackListError::InvalidXml)?;
    let lowercase = text.to_ascii_lowercase();
    if lowercase.contains("<!doctype") || lowercase.contains("<!entity") {
        return Err(ComicRackListError::InvalidXml);
    }
    let document = roxmltree::Document::parse(text).map_err(|_| ComicRackListError::InvalidXml)?;
    let root = document.root_element();
    if root.tag_name().name() != "ReadingList" {
        return Err(ComicRackListError::InvalidXml);
    }
    let name = root
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "Name")
        .and_then(|node| node.text())
        .ok_or(ComicRackListError::MissingName)?;
    if name.trim().is_empty() {
        return Err(ComicRackListError::MissingName);
    }
    let books = root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "Books")
        .flat_map(|node| node.children())
        .filter(|node| node.is_element() && node.tag_name().name() == "Book")
        .map(|node| {
            let series = node
                .attribute("Series")
                .filter(|value| !value.trim().is_empty())
                .ok_or(ComicRackListError::MissingBookField)?;
            let number = node.attribute("Number").ok_or(ComicRackListError::MissingBookField)?;
            let volume = node.attribute("Volume").and_then(|value| value.parse::<i32>().ok());
            let mut candidates = Vec::with_capacity(2);
            if let Some(volume) = volume.filter(|value| *value != 1) {
                candidates.push(format!("{series} ({volume})"));
            }
            candidates.push(series.to_owned());
            Ok(ReadListRequestBook {
                series: candidates,
                number: number.trim().to_owned(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if books.is_empty() {
        return Err(ComicRackListError::NoBooks);
    }
    Ok(ReadListRequest {
        name: name.to_owned(),
        books,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_candidates_and_ignores_unknown_content() {
        let request = parse_read_list(br#"<ReadingList><Name>List</Name><Unknown/><Books><Book Series="Saga" Number=" 007 " Volume="2" Year="2020"/><Book Series="One" Number="1" Volume="1"/></Books></ReadingList>"#).unwrap();
        assert_eq!(request.books[0].series, ["Saga (2)", "Saga"]);
        assert_eq!(request.books[0].number, "007");
        assert_eq!(request.books[1].series, ["One"]);
    }

    #[test]
    fn reports_kotlin_error_codes() {
        for (xml, expected) in [
            ("not xml", ComicRackListError::InvalidXml),
            ("<ReadingList><Books/></ReadingList>", ComicRackListError::MissingName),
            (
                "<ReadingList><Name> </Name><Books/></ReadingList>",
                ComicRackListError::MissingName,
            ),
            (
                "<ReadingList><Name>x</Name><Books/></ReadingList>",
                ComicRackListError::NoBooks,
            ),
            (
                "<ReadingList><Name>x</Name><Books><Book Series=\"x\"/></Books></ReadingList>",
                ComicRackListError::MissingBookField,
            ),
        ] {
            assert_eq!(parse_read_list(xml.as_bytes()), Err(expected));
        }
        assert_eq!(
            parse_read_list(b"<!DOCTYPE x><ReadingList><Name>x</Name><Books/></ReadingList>"),
            Err(ComicRackListError::InvalidXml)
        );
    }
}
