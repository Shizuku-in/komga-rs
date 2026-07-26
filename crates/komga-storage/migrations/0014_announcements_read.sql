-- Kotlin-compatible per-user announcement read state.
CREATE TABLE IF NOT EXISTS ANNOUNCEMENTS_READ
(
    USER_ID         varchar NOT NULL,
    ANNOUNCEMENT_ID varchar NOT NULL,
    PRIMARY KEY (USER_ID, ANNOUNCEMENT_ID),
    FOREIGN KEY (USER_ID) REFERENCES USER (ID)
);
