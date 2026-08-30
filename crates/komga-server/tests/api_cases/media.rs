use super::*;

const JXL_1X1_RED: &[u8] = &[
    0xff, 0x0a, 0x00, 0x10, 0x10, 0x09, 0x08, 0x00, 0x01, 0x00, 0x28, 0x00, 0x4b, 0x18, 0x8b, 0x15, 0xc2, 0x49, 0x41,
    0x4e, 0x7f, 0x00,
];

#[tokio::test]
async fn cbz_pages_conversion_thumbnail_cache_and_manual_analysis_match_contract() {
    let (_temporary, storage, tasks, book, media) = analyzed_media_fixture("archives/zip.zip", "Volume 1.cbz").await;
    let generated = generate_thumbnail(&book, &media, 300).unwrap();
    let thumbnail = ThumbnailBook {
        id: new_id(),
        dimension: image_dimension(&generated.bytes).unwrap(),
        file_size: i64::try_from(generated.bytes.len()).unwrap(),
        thumbnail: Some(generated.bytes),
        url: None,
        selected: false,
        thumbnail_type: "GENERATED".into(),
        media_type: generated.media_type,
        book_id: book.id.clone(),
    };
    storage.replace_generated_thumbnail(&thumbnail).await.unwrap();

    let app = build_app(AppState::new(storage, tasks.clone()).await.unwrap());
    let unauthorized = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let pages = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pages.status(), StatusCode::OK);
    let pages = body_json(pages).await;
    assert_eq!(pages[0]["number"], 1);
    assert_eq!(pages[0]["fileName"], "komga.png");
    assert_eq!(pages[0]["mediaType"], "image/png");
    assert_eq!(pages[0]["width"], 48);
    assert_eq!(pages[0]["height"], 48);
    assert_eq!(pages[0]["sizeBytes"], 3108);

    let page = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()[header::CONTENT_TYPE], "image/png");
    assert!(page.headers().contains_key(header::CONTENT_DISPOSITION));
    let last_modified = page.headers()[header::LAST_MODIFIED].to_str().unwrap().to_owned();
    assert!(body_bytes(page).await.starts_with(b"\x89PNG"));

    let cached = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::IF_MODIFIED_SINCE, &last_modified)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);

    let converted = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/v1/books/{}/pages/0?zero_based=true&convert=jpeg",
                book.id
            ))
            .header(header::AUTHORIZATION, &admin)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(converted.status(), StatusCode::OK);
    assert_eq!(converted.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(converted).await.starts_with(&[0xff, 0xd8]));

    let page_thumbnail = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1/thumbnail", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page_thumbnail.status(), StatusCode::OK);
    assert_eq!(page_thumbnail.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(page_thumbnail).await.starts_with(&[0xff, 0xd8]));

    let book_thumbnail = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/thumbnail", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(book_thumbnail.status(), StatusCode::OK);
    assert_eq!(book_thumbnail.headers()[header::CONTENT_TYPE], "image/jpeg");

    let invalid_page = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/0", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_page.status(), StatusCode::BAD_REQUEST);

    let raw = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1/raw", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(raw.status(), StatusCode::BAD_REQUEST);

    let analyze = app
        .oneshot(
            Request::post(format!("/api/v1/books/{}/analyze", book.id))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(analyze.status(), StatusCode::ACCEPTED);
    let queued = tasks.list().await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].simple_type, "AnalyzeBook");
    assert_eq!(queued[0].group_id.as_deref(), Some(book.series_id.as_str()));
    assert_eq!(queued[0].priority, 6);
}

#[tokio::test]
async fn jpeg_xl_pages_are_analyzed_served_and_converted() {
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    archive.start_file("page.jxl", SimpleFileOptions::default()).unwrap();
    archive.write_all(JXL_1X1_RED).unwrap();
    let bytes = archive.finish().unwrap().into_inner();
    let (_temporary, storage, tasks, book, media) = analyzed_media_bytes_fixture(&bytes, "Volume.cbz").await;
    assert_eq!(media.page_count, 1);
    assert_eq!(media.pages[0].media_type, "image/jxl");
    assert_eq!(
        media.pages[0].dimension,
        Some(komga_core::Dimension { width: 1, height: 1 })
    );

    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");
    let page_path = format!("/api/v1/books/{}/pages/1", book.id);

    let original = app
        .clone()
        .oneshot(
            Request::get(&page_path)
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(original.status(), StatusCode::OK);
    assert_eq!(original.headers()[header::CONTENT_TYPE], "image/jxl");
    assert!(
        original.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .ends_with(".jxl\"")
    );
    assert_eq!(body_bytes(original).await, JXL_1X1_RED);

    let converted = app
        .clone()
        .oneshot(
            Request::get(format!("{page_path}?convert=jpeg"))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(converted.status(), StatusCode::OK);
    assert_eq!(converted.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(converted).await.starts_with(&[0xff, 0xd8]));

    let thumbnail = app
        .oneshot(
            Request::get(format!("{page_path}/thumbnail"))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(thumbnail.status(), StatusCode::OK);
    assert_eq!(thumbnail.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(thumbnail).await.starts_with(&[0xff, 0xd8]));
}

#[cfg(feature = "heif-native")]
#[tokio::test]
async fn avif_pages_are_analyzed_served_and_converted() {
    let avif = hex_fixture(include_str!("../../../komga-media/tests/fixtures/blue.avif.hex"));
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    archive.start_file("page.avif", SimpleFileOptions::default()).unwrap();
    archive.write_all(&avif).unwrap();
    let bytes = archive.finish().unwrap().into_inner();
    let (_temporary, storage, tasks, book, media) = analyzed_media_bytes_fixture(&bytes, "Volume.cbz").await;
    assert_eq!(media.page_count, 1);
    assert_eq!(media.pages[0].media_type, "image/avif");
    assert_eq!(
        media.pages[0].dimension,
        Some(komga_core::Dimension { width: 3, height: 2 })
    );

    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");
    let page_path = format!("/api/v1/books/{}/pages/1", book.id);

    let original = app
        .clone()
        .oneshot(
            Request::get(&page_path)
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(original.status(), StatusCode::OK);
    assert_eq!(original.headers()[header::CONTENT_TYPE], "image/avif");
    assert!(
        original.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .ends_with(".avif\"")
    );
    assert_eq!(body_bytes(original).await, avif);

    let converted = app
        .clone()
        .oneshot(
            Request::get(format!("{page_path}?convert=jpeg"))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(converted.status(), StatusCode::OK);
    assert_eq!(converted.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(converted).await.starts_with(&[0xff, 0xd8]));

    let thumbnail = app
        .oneshot(
            Request::get(format!("{page_path}/thumbnail"))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(thumbnail.status(), StatusCode::OK);
    assert_eq!(thumbnail.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(thumbnail).await.starts_with(&[0xff, 0xd8]));
}

#[cfg(feature = "heif-native")]
fn hex_fixture(value: &str) -> Vec<u8> {
    value
        .split_ascii_whitespace()
        .flat_map(|line| line.as_bytes().as_chunks::<2>().0.iter().copied())
        .map(|byte| u8::from_str_radix(std::str::from_utf8(&byte).unwrap(), 16).unwrap())
        .collect()
}

#[tokio::test]
async fn book_and_series_thumbnail_management_contract() {
    let (_temporary, storage, tasks, book, media) = analyzed_media_fixture("archives/zip.zip", "Volume.cbz").await;
    let generated = generate_thumbnail(&book, &media, 300).unwrap();
    let generated_thumbnail = ThumbnailBook {
        id: "generated-cover".into(),
        thumbnail: Some(generated.bytes.clone()),
        url: None,
        selected: true,
        thumbnail_type: "GENERATED".into(),
        media_type: generated.media_type,
        file_size: i64::try_from(generated.bytes.len()).unwrap(),
        dimension: image_dimension(&generated.bytes).unwrap(),
        book_id: book.id.clone(),
    };
    storage.replace_generated_thumbnail(&generated_thumbnail).await.unwrap();
    let app = build_app(AppState::new(storage.clone(), tasks.clone()).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");
    let thumbnails_path = format!("/api/v1/books/{}/thumbnails", book.id);

    let (content_type, body) = multipart_file(&generated.bytes);
    let uploaded = app
        .clone()
        .oneshot(
            Request::post(format!("{thumbnails_path}?selected=false"))
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(uploaded.status(), StatusCode::OK);
    let uploaded = body_json(uploaded).await;
    assert_eq!(uploaded["type"], "USER_UPLOADED");
    assert_eq!(uploaded["selected"], false);
    assert_eq!(uploaded["width"], generated_thumbnail.dimension.width);
    let uploaded_id = uploaded["id"].as_str().unwrap();

    let listed = app
        .clone()
        .oneshot(
            Request::get(&thumbnails_path)
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(listed).await.as_array().unwrap().len(), 2);
    let selected_path = format!("{thumbnails_path}/{uploaded_id}/selected");
    assert_eq!(
        app.clone()
            .oneshot(
                Request::put(selected_path)
                    .header(header::AUTHORIZATION, &admin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );

    let image = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/thumbnail", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(image.status(), StatusCode::OK);
    assert_eq!(
        image.headers()[header::CACHE_CONTROL],
        "private, max-age=0, must-revalidate"
    );
    let etag = image.headers()[header::ETAG].clone();
    let cached = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/thumbnail", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::IF_NONE_MATCH, etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);

    let generated_delete = app
        .clone()
        .oneshot(
            Request::delete(format!("{thumbnails_path}/generated-cover"))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(generated_delete.status(), StatusCode::BAD_REQUEST);

    let series_path = format!("/api/v1/series/{}/thumbnails", book.series_id);
    let (content_type, body) = multipart_file(&generated.bytes);
    let series_upload = app
        .clone()
        .oneshot(
            Request::post(&series_path)
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(series_upload.status(), StatusCode::OK);
    let series_upload = body_json(series_upload).await;
    assert_eq!(series_upload["seriesId"], book.series_id);
    let series_thumbnail = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/series/{}/thumbnail", book.series_id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(series_thumbnail.status(), StatusCode::OK);

    let (content_type, body) = multipart_file(b"not an image");
    let unsupported = app
        .clone()
        .oneshot(
            Request::post(&thumbnails_path)
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unsupported.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let mut larger_than_default_limit = generated.bytes.clone();
    larger_than_default_limit.resize(3 * 1024 * 1024, 0);
    let (content_type, body) = multipart_file(&larger_than_default_limit);
    let larger_upload = app
        .clone()
        .oneshot(
            Request::post(format!("{thumbnails_path}?selected=false"))
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(larger_upload.status(), StatusCode::OK);

    let mut above_upload_limit = generated.bytes.clone();
    above_upload_limit.resize(11 * 1024 * 1024, 0);
    let (content_type, body) = multipart_file(&above_upload_limit);
    let oversized = app
        .clone()
        .oneshot(
            Request::post(&thumbnails_path)
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let regenerated = app
        .oneshot(
            Request::put("/api/v1/books/thumbnails?for_bigger_result_only=true")
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(regenerated.status(), StatusCode::ACCEPTED);
    assert_eq!(
        tasks
            .count_by_simple_type()
            .await
            .unwrap()
            .get("FindBookThumbnailsToRegenerate"),
        Some(&1)
    );
}

#[tokio::test]
async fn book_thumbnail_by_id_enforces_content_restriction() {
    let (_temporary, storage, tasks, book, media) = analyzed_media_fixture("archives/zip.zip", "Volume.cbz").await;
    let generated = generate_thumbnail(&book, &media, 300).unwrap();
    storage
        .replace_generated_thumbnail(&ThumbnailBook {
            id: "restricted-cover".into(),
            thumbnail: Some(generated.bytes.clone()),
            url: None,
            selected: true,
            thumbnail_type: "GENERATED".into(),
            media_type: generated.media_type,
            file_size: i64::try_from(generated.bytes.len()).unwrap(),
            dimension: image_dimension(&generated.bytes).unwrap(),
            book_id: book.id.clone(),
        })
        .await
        .unwrap();

    let app = build_app(AppState::new(storage.clone(), tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let password_hash = bcrypt::hash("reader-password", 4).unwrap();

    let mut age_restricted = User::new("thumbnail-age@example.org".into(), password_hash);
    age_restricted.restrictions = ContentRestrictions::new(
        Some(AgeRestriction {
            age: 12,
            restriction: AllowExclude::AllowOnly,
        }),
        Vec::<String>::new(),
        Vec::<String>::new(),
    );
    storage.create_user(&age_restricted).await.unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/thumbnails/restricted-cover", book.id))
                .header(
                    header::AUTHORIZATION,
                    basic("thumbnail-age@example.org", "reader-password"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
