ALTER TABLE MEDIA_PAGE ADD COLUMN FILE_HASH_ALGORITHM varchar NULL;

-- These tables already exist in current Kotlin databases. Rust creates them
-- for native databases now so the deliberate hash-algorithm reset is uniform.
CREATE TABLE IF NOT EXISTS PAGE_HASH
(
    HASH               varchar  NOT NULL PRIMARY KEY,
    SIZE               int8     NULL,
    ACTION             varchar  NOT NULL,
    DELETE_COUNT       int      NOT NULL DEFAULT 0,
    CREATED_DATE       datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    LAST_MODIFIED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS PAGE_HASH_THUMBNAIL
(
    HASH      varchar NOT NULL PRIMARY KEY,
    THUMBNAIL blob    NOT NULL
);

-- RGB24_V1 intentionally replaces Java ImageIO's encoder-dependent JPEG hash.
DELETE FROM PAGE_HASH_THUMBNAIL;
DELETE FROM PAGE_HASH;
UPDATE MEDIA_PAGE
   SET FILE_HASH = '', FILE_HASH_ALGORITHM = NULL
 WHERE MEDIA_TYPE = 'image/jpeg';
