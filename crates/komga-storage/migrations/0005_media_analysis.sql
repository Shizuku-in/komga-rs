-- Current Kotlin media relations needed for CBZ analysis and generated covers.
-- Existing Flyway databases already contain these objects.
CREATE TABLE IF NOT EXISTS MEDIA_PAGE
(
    FILE_NAME  varchar NOT NULL,
    MEDIA_TYPE varchar NOT NULL,
    NUMBER     int     NOT NULL,
    BOOK_ID    varchar NOT NULL,
    WIDTH      int     NULL,
    HEIGHT     int     NULL,
    FILE_HASH  varchar NOT NULL DEFAULT '',
    FILE_SIZE  int8    NULL,
    PRIMARY KEY (BOOK_ID, NUMBER),
    FOREIGN KEY (BOOK_ID) REFERENCES BOOK (ID)
);

CREATE TABLE IF NOT EXISTS MEDIA_FILE
(
    FILE_NAME varchar NOT NULL,
    BOOK_ID   varchar NOT NULL,
    MEDIA_TYPE varchar NULL,
    SUB_TYPE  varchar NULL,
    FILE_SIZE int8    NULL,
    FOREIGN KEY (BOOK_ID) REFERENCES BOOK (ID)
);

CREATE INDEX IF NOT EXISTS idx__media_file__book_id ON MEDIA_FILE (BOOK_ID);

CREATE TABLE IF NOT EXISTS THUMBNAIL_BOOK
(
    ID                 varchar  NOT NULL PRIMARY KEY,
    THUMBNAIL          blob     NULL,
    URL                varchar  NULL,
    SELECTED           boolean  NOT NULL DEFAULT 0,
    TYPE               varchar  NOT NULL,
    CREATED_DATE       datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    LAST_MODIFIED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    BOOK_ID            varchar  NOT NULL,
    WIDTH              int      NOT NULL DEFAULT 0,
    HEIGHT             int      NOT NULL DEFAULT 0,
    MEDIA_TYPE         varchar  NOT NULL DEFAULT '',
    FILE_SIZE          int8     NOT NULL DEFAULT 0,
    FOREIGN KEY (BOOK_ID) REFERENCES BOOK (ID)
);

CREATE INDEX IF NOT EXISTS idx__thumbnail_book__book_id ON THUMBNAIL_BOOK (BOOK_ID);
CREATE INDEX IF NOT EXISTS idx__thumbnail_book__width ON THUMBNAIL_BOOK (WIDTH);
CREATE INDEX IF NOT EXISTS idx__thumbnail_book__height ON THUMBNAIL_BOOK (HEIGHT);
CREATE INDEX IF NOT EXISTS idx__thumbnail_book__file_size ON THUMBNAIL_BOOK (FILE_SIZE);
