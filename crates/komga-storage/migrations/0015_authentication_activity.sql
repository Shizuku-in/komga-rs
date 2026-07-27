-- Kotlin-compatible authentication audit history. This is additive for both
-- Rust databases and Kotlin databases that already own the table.
CREATE TABLE IF NOT EXISTS AUTHENTICATION_ACTIVITY
(
    USER_ID         varchar  NULL DEFAULT NULL,
    EMAIL           varchar  NULL DEFAULT NULL,
    IP              varchar  NULL DEFAULT NULL,
    USER_AGENT      varchar  NULL DEFAULT NULL,
    SUCCESS         boolean  NOT NULL,
    ERROR           varchar  NULL DEFAULT NULL,
    DATE_TIME       datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    SOURCE          varchar  NULL DEFAULT NULL,
    API_KEY_ID      varchar  NULL DEFAULT NULL,
    API_KEY_COMMENT varchar  NULL DEFAULT NULL,
    FOREIGN KEY (USER_ID) REFERENCES USER (ID)
);

CREATE INDEX IF NOT EXISTS IDX__AUTHENTICATION_ACTIVITY__USER_ID
    ON AUTHENTICATION_ACTIVITY (USER_ID);
CREATE INDEX IF NOT EXISTS IDX__AUTHENTICATION_ACTIVITY__DATE_TIME
    ON AUTHENTICATION_ACTIVITY (DATE_TIME);
