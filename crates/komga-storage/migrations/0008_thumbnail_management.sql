CREATE TABLE IF NOT EXISTS THUMBNAIL_SERIES
(
    ID                 varchar  NOT NULL PRIMARY KEY,
    URL                varchar  NULL,
    SELECTED           boolean  NOT NULL DEFAULT 0,
    THUMBNAIL          blob     NULL,
    TYPE               varchar  NOT NULL,
    CREATED_DATE       datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    LAST_MODIFIED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    SERIES_ID          varchar  NOT NULL,
    WIDTH              int      NOT NULL DEFAULT 0,
    HEIGHT             int      NOT NULL DEFAULT 0,
    MEDIA_TYPE         varchar  NOT NULL DEFAULT '',
    FILE_SIZE          int8     NOT NULL DEFAULT 0,
    FOREIGN KEY (SERIES_ID) REFERENCES SERIES (ID)
);

CREATE INDEX IF NOT EXISTS idx__thumbnail_series__series_id ON THUMBNAIL_SERIES (SERIES_ID);
CREATE INDEX IF NOT EXISTS idx__thumbnail_series__width ON THUMBNAIL_SERIES (WIDTH);
CREATE INDEX IF NOT EXISTS idx__thumbnail_series__height ON THUMBNAIL_SERIES (HEIGHT);
CREATE INDEX IF NOT EXISTS idx__thumbnail_series__file_size ON THUMBNAIL_SERIES (FILE_SIZE);
