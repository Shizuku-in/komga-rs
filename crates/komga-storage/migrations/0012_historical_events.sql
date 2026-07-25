-- Current Kotlin databases already own these tables. Native Rust databases
-- create the same schema without foreign keys to Book or Series so audit rows
-- survive content deletion.
CREATE TABLE IF NOT EXISTS HISTORICAL_EVENT
(
    ID        varchar PRIMARY KEY,
    TYPE      varchar  NOT NULL,
    BOOK_ID   varchar  NULL,
    SERIES_ID varchar  NULL,
    TIMESTAMP datetime NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS HISTORICAL_EVENT_PROPERTIES
(
    ID    varchar NOT NULL,
    KEY   varchar NOT NULL,
    VALUE varchar NOT NULL,
    PRIMARY KEY (ID, KEY),
    FOREIGN KEY (ID) REFERENCES HISTORICAL_EVENT (ID)
);

CREATE INDEX IF NOT EXISTS IDX_HISTORICAL_EVENT_TIMESTAMP
    ON HISTORICAL_EVENT (TIMESTAMP);
