-- Kotlin-compatible content skeleton required by filesystem scanning. These
-- tables already exist in a current Flyway database, so every statement is
-- additive for databases shared across migration stages.
CREATE TABLE IF NOT EXISTS SERIES
(
    ID                 varchar  NOT NULL PRIMARY KEY,
    CREATED_DATE       datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    LAST_MODIFIED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FILE_LAST_MODIFIED datetime NOT NULL,
    NAME               varchar  NOT NULL,
    URL                varchar  NOT NULL,
    LIBRARY_ID         varchar  NOT NULL,
    BOOK_COUNT         int      NOT NULL DEFAULT 0,
    DELETED_DATE       datetime NULL DEFAULT NULL,
    ONESHOT            boolean  NOT NULL DEFAULT 0,
    FOREIGN KEY (LIBRARY_ID) REFERENCES LIBRARY (ID)
);

CREATE INDEX IF NOT EXISTS idx__series__library_id ON SERIES (LIBRARY_ID);
CREATE INDEX IF NOT EXISTS idx__series__last_modified_date ON SERIES (LAST_MODIFIED_DATE);
CREATE INDEX IF NOT EXISTS idx__series__created_date ON SERIES (CREATED_DATE);

CREATE TABLE IF NOT EXISTS SERIES_METADATA
(
    CREATED_DATE          datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    LAST_MODIFIED_DATE    datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    STATUS                varchar  NOT NULL,
    STATUS_LOCK           boolean  NOT NULL DEFAULT 0,
    TITLE                 varchar  NOT NULL,
    TITLE_LOCK            boolean  NOT NULL DEFAULT 0,
    TITLE_SORT            varchar  NOT NULL,
    TITLE_SORT_LOCK       boolean  NOT NULL DEFAULT 0,
    SERIES_ID             varchar  NOT NULL PRIMARY KEY,
    PUBLISHER             varchar  NOT NULL DEFAULT '',
    PUBLISHER_LOCK        boolean  NOT NULL DEFAULT 0,
    READING_DIRECTION     varchar  NULL,
    READING_DIRECTION_LOCK boolean NOT NULL DEFAULT 0,
    AGE_RATING            int      NULL,
    AGE_RATING_LOCK       boolean  NOT NULL DEFAULT 0,
    SUMMARY               varchar  NOT NULL DEFAULT '',
    SUMMARY_LOCK          boolean  NOT NULL DEFAULT 0,
    LANGUAGE              varchar  NOT NULL DEFAULT '',
    LANGUAGE_LOCK         boolean  NOT NULL DEFAULT 0,
    GENRES_LOCK           boolean  NOT NULL DEFAULT 0,
    TAGS_LOCK             boolean  NOT NULL DEFAULT 0,
    TOTAL_BOOK_COUNT      int      NULL,
    TOTAL_BOOK_COUNT_LOCK boolean  NOT NULL DEFAULT 0,
    SHARING_LABELS_LOCK   boolean  NOT NULL DEFAULT 0,
    LINKS_LOCK            boolean  NOT NULL DEFAULT 0,
    ALTERNATE_TITLES_LOCK boolean  NOT NULL DEFAULT 0,
    FOREIGN KEY (SERIES_ID) REFERENCES SERIES (ID)
);

CREATE INDEX IF NOT EXISTS idx__series_metadata__title ON SERIES_METADATA (TITLE);

CREATE TABLE IF NOT EXISTS BOOK_METADATA_AGGREGATION
(
    CREATED_DATE       datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    LAST_MODIFIED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    RELEASE_DATE       date     NULL,
    SUMMARY            varchar  NOT NULL DEFAULT '',
    SUMMARY_NUMBER     varchar  NOT NULL DEFAULT '',
    SERIES_ID          varchar  NOT NULL PRIMARY KEY,
    FOREIGN KEY (SERIES_ID) REFERENCES SERIES (ID)
);

CREATE TABLE IF NOT EXISTS BOOK
(
    ID                 varchar  NOT NULL PRIMARY KEY,
    CREATED_DATE       datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    LAST_MODIFIED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FILE_LAST_MODIFIED datetime NOT NULL,
    NAME               varchar  NOT NULL,
    URL                varchar  NOT NULL,
    SERIES_ID          varchar  NOT NULL,
    FILE_SIZE          int8     NOT NULL DEFAULT 0,
    NUMBER             int      NOT NULL DEFAULT 0,
    LIBRARY_ID         varchar  NOT NULL,
    FILE_HASH          varchar  NOT NULL DEFAULT '',
    DELETED_DATE       datetime NULL DEFAULT NULL,
    ONESHOT            boolean  NOT NULL DEFAULT 0,
    FILE_HASH_KOREADER varchar  NOT NULL DEFAULT '',
    FOREIGN KEY (LIBRARY_ID) REFERENCES LIBRARY (ID),
    FOREIGN KEY (SERIES_ID) REFERENCES SERIES (ID)
);

CREATE INDEX IF NOT EXISTS idx__book__series_id ON BOOK (SERIES_ID);
CREATE INDEX IF NOT EXISTS idx__book__library_id ON BOOK (LIBRARY_ID);
CREATE INDEX IF NOT EXISTS idx__book__created_date ON BOOK (CREATED_DATE);

CREATE TABLE IF NOT EXISTS BOOK_METADATA
(
    CREATED_DATE       datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    LAST_MODIFIED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    NUMBER             varchar  NOT NULL,
    NUMBER_LOCK        boolean  NOT NULL DEFAULT 0,
    NUMBER_SORT        real     NOT NULL,
    NUMBER_SORT_LOCK   boolean  NOT NULL DEFAULT 0,
    RELEASE_DATE       date     NULL,
    RELEASE_DATE_LOCK  boolean  NOT NULL DEFAULT 0,
    SUMMARY            varchar  NOT NULL DEFAULT '',
    SUMMARY_LOCK       boolean  NOT NULL DEFAULT 0,
    TITLE              varchar  NOT NULL,
    TITLE_LOCK         boolean  NOT NULL DEFAULT 0,
    AUTHORS_LOCK       boolean  NOT NULL DEFAULT 0,
    TAGS_LOCK          boolean  NOT NULL DEFAULT 0,
    BOOK_ID            varchar  NOT NULL PRIMARY KEY,
    ISBN               varchar  NOT NULL DEFAULT '',
    ISBN_LOCK          boolean  NOT NULL DEFAULT 0,
    LINKS_LOCK         boolean  NOT NULL DEFAULT 0,
    FOREIGN KEY (BOOK_ID) REFERENCES BOOK (ID)
);

CREATE INDEX IF NOT EXISTS idx__book_metadata__number_sort ON BOOK_METADATA (NUMBER_SORT);
CREATE INDEX IF NOT EXISTS idx__book_metadata__release_date ON BOOK_METADATA (RELEASE_DATE);

CREATE TABLE IF NOT EXISTS MEDIA
(
    MEDIA_TYPE          varchar  NULL,
    STATUS              varchar  NOT NULL,
    CREATED_DATE        datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    LAST_MODIFIED_DATE  datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    COMMENT             varchar  NULL,
    BOOK_ID             varchar  NOT NULL PRIMARY KEY,
    PAGE_COUNT          int      NOT NULL DEFAULT 0,
    EXTENSION_CLASS     varchar  NULL,
    _UNUSED             varchar  NULL,
    EXTENSION_VALUE_BLOB blob    NULL,
    EPUB_DIVINA_COMPATIBLE boolean NOT NULL DEFAULT 0,
    EPUB_IS_KEPUB       boolean  NOT NULL DEFAULT 0,
    FOREIGN KEY (BOOK_ID) REFERENCES BOOK (ID)
);

CREATE INDEX IF NOT EXISTS idx__media__status ON MEDIA (STATUS);
